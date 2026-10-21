// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Authority vertical slice for the graph-generated TypeSafe System One contract.

#![cfg(feature = "typesafe")]

use std::{fs, path::Path, process::Command};

#[tokio::test]
async fn system_one_generated_contract_authority() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let fixture = workspace
        .join("tests")
        .join("fixtures")
        .join("agents")
        .join("typesafe-system-one-authority");
    let temp = tempfile::TempDir::new().expect("temporary agent directory");
    copy_dir_all(&fixture, temp.path()).expect("copy System One fixture");

    let package_dir = tempfile::TempDir::new().expect("temporary package directory");
    let package = package_dir.path().join("system-one-authority.tar.gz");
    baml_rt_builder::build_agent_package(temp.path(), &package)
        .await
        .expect("build System One authority fixture");

    let extracted = tempfile::TempDir::new().expect("temporary extracted package directory");
    let archive = fs::File::open(&package).expect("open authority package");
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    archive
        .unpack(extracted.path())
        .expect("extract authority package");

    let declarations = fs::read_to_string(temp.path().join("src/baml-runtime.d.ts"))
        .expect("read generated TypeScript declarations");
    let generated_baml = fs::read_to_string(extracted.path().join("baml_src/_baml_runtime.baml"))
        .expect("read generated BAML");
    let catalog = fs::read_to_string(
        extracted
            .path()
            .join("baml_src/_baml_tool_schema_catalog.txt"),
    )
    .expect("read generated schema catalog");
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(temp.path().join("manifest.json")).expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");

    assert_eq!(
        manifest["tools"],
        serde_json::json!(["typesafe/system_one"])
    );
    let catalog_tools: Vec<&str> = catalog
        .lines()
        .filter_map(|line| line.strip_prefix("tool "))
        .collect();
    assert_eq!(catalog_tools, ["typesafe/system_one"]);

    for symbol in [
        "SystemOneInput",
        "SystemOneQuestion",
        "ChoiceQuestion",
        "ScoreQuestion",
        "NoulQuestion",
        "NoulCriteria",
        "SystemOneOutput",
        "SystemOneAnswer",
        "ChoiceAnswer",
        "ScoreAnswer",
        "NoulAnswer",
        "SystemOneUsage",
    ] {
        assert_eq!(
            declaration_count(&declarations, symbol),
            1,
            "graph-owned declaration `{symbol}` must occur once"
        );
    }

    for fragment in [
        "state: JsonValue;",
        "instructions: JsonValue;",
        "criteria: Record<string, JsonValue>;",
        "criteria: JsonValue[];",
        "true: JsonValue;",
        "false: JsonValue;",
        "probabilities: Record<string, number>;",
        "\"typesafe/system_one\": Record<string, never>;",
        "\"typesafe/system_one\": SystemOneInput;",
        "\"typesafe/system_one\": SystemOneOutput;",
    ] {
        assert!(
            declarations.contains(fragment),
            "generated declarations are missing `{fragment}`"
        );
    }
    assert!(
        !declarations.contains("OpaqueJson"),
        "handwritten OpaqueJson TypeScript declarations must not reach generated output"
    );

    let required_open_input = declarations
        .split("export type ToolWithRequiredOpenInput =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("required OpenInput declaration");
    assert_eq!(required_open_input.trim(), "never");

    for fragment in [
        "field_true OpaqueJson @alias(\"true\")",
        "field_false OpaqueJson @alias(\"false\")",
        "type SystemOneQuestion = ChoiceQuestion | ScoreQuestion | NoulQuestion",
        "type SystemOneAnswer = ChoiceAnswer | ScoreAnswer | NoulAnswer",
    ] {
        assert!(
            generated_baml.contains(fragment),
            "generated BAML is missing `{fragment}`"
        );
    }
    assert!(!generated_baml.contains("class SystemOneOutputUsage"));

    run_tsc_no_emit(temp.path());

    insta::assert_snapshot!(
        "system_one_generated_declarations",
        declaration_excerpt(
            &declarations,
            &[
                "SystemOneInput",
                "SystemOneQuestion",
                "ChoiceQuestion",
                "ScoreQuestion",
                "NoulQuestion",
                "NoulCriteria",
                "SystemOneOutput",
                "SystemOneAnswer",
                "ChoiceAnswer",
                "ScoreAnswer",
                "NoulAnswer",
                "SystemOneUsage",
            ],
        )
    );
}

fn declaration_count(rendered: &str, symbol: &str) -> usize {
    let interface_prefix = format!("export interface {symbol} ");
    let type_prefix = format!("export type {symbol} =");
    rendered
        .lines()
        .filter(|line| line.starts_with(&interface_prefix) || line.starts_with(&type_prefix))
        .count()
}

fn declaration_excerpt(rendered: &str, symbols: &[&str]) -> String {
    symbols
        .iter()
        .map(|symbol| {
            let interface_prefix = format!("export interface {symbol} {{");
            let type_prefix = format!("export type {symbol} =");
            let lines: Vec<&str> = rendered.lines().collect();
            let start = lines
                .iter()
                .position(|line| {
                    line.starts_with(&interface_prefix) || line.starts_with(&type_prefix)
                })
                .unwrap_or_else(|| panic!("missing declaration `{symbol}`"));
            if lines[start].starts_with(&type_prefix) {
                lines[start].to_string()
            } else {
                let end = lines[start..]
                    .iter()
                    .position(|line| *line == "}")
                    .map(|offset| start + offset)
                    .unwrap_or_else(|| panic!("unterminated interface `{symbol}`"));
                lines[start..=end].join("\n")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn run_tsc_no_emit(project: &Path) {
    let attempts: [(&str, &[&str]); 2] = [
        ("tsc", &[]),
        ("npx", &["--yes", "--package", "typescript@5", "tsc"]),
    ];
    let mut unavailable = Vec::new();
    for (program, base_args) in attempts {
        match Command::new(program)
            .args(base_args)
            .arg("--project")
            .arg(project.join("tsconfig.json"))
            .arg("--noEmit")
            .current_dir(project)
            .output()
        {
            Ok(output) if output.status.success() => return,
            Ok(output) => panic!(
                "tsc rejected the System One fixture:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => unavailable.push(format!("{program}: {error}")),
        }
    }
    panic!(
        "no TypeScript compiler available ({})",
        unavailable.join("; ")
    );
}
