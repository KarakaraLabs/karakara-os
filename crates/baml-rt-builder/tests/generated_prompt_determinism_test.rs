// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(feature = "dev-tools", feature = "typesafe"))]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use baml_rt_builder::builder::{AgentDir, BuildDir, RuntimeTypeGenerator, traits::TypeGenerator};
use sha2::{Digest, Sha256};
use test_support::common::{copy_agent_tree_for_build, workspace_root};

const SOURCE_ENV: &str = "KARAKARA_PROMPT_DETERMINISM_SOURCE";
const OUTPUT_ENV: &str = "KARAKARA_PROMPT_DETERMINISM_OUTPUT";
const RUNTIME_PRELUDE: &str = "_baml_runtime.baml";
const CATALOG_SIDECAR: &str = "_baml_tool_schema_catalog.txt";

struct Fixture {
    name: &'static str,
    source: PathBuf,
    prelude_hash: &'static str,
    has_catalog: bool,
}

#[test]
fn generated_prompt_artifacts_match_committed_hashes() {
    let root = workspace_root();
    let fixtures = [
        Fixture {
            name: "task-lifecycle-demo",
            source: root.join("tests/fixtures/agents/task-lifecycle-demo"),
            prelude_hash: "89d94af0d24db58f1e25f7790a88d138999c36b2b0ab6acbefb2d0117cf6882b",
            has_catalog: false,
        },
        Fixture {
            name: "coordinator-agent",
            source: root.join("agents/coordinator-agent"),
            prelude_hash: "46d5e0fbc68007ecf56c392e1bfbacba5b02dff899c078fa3938f50bf58db743",
            has_catalog: true,
        },
        Fixture {
            name: "typesafe-system-one-authority",
            source: root.join("tests/fixtures/agents/typesafe-system-one-authority"),
            prelude_hash: "44bfc4862e2a1bb55caf7be83f95ee92d5f922dfb97faec91ac2b329241e31e5",
            has_catalog: true,
        },
        Fixture {
            name: "unified-step-harness-demo",
            source: root.join("tests/fixtures/agents/unified-step-harness-demo"),
            prelude_hash: "28e60add6c60ff006f31980b03022429952123e730dcb5fcd4a543e88b31e0da",
            has_catalog: true,
        },
    ];
    let temp = tempfile::tempdir().expect("temporary artifact directory");

    for fixture in fixtures {
        let outputs: Vec<_> = (0..3)
            .map(|run| {
                let output = temp.path().join(fixture.name).join(run.to_string());
                run_generation_process(&fixture.source, &output);
                output
            })
            .collect();

        assert_eq!(
            sha256_file(&outputs[0].join(RUNTIME_PRELUDE)),
            fixture.prelude_hash,
            "{} prelude no longer matches its committed hash",
            fixture.name
        );
        assert_files_match(&outputs, RUNTIME_PRELUDE, fixture.name);

        if fixture.has_catalog {
            assert_files_match(&outputs, CATALOG_SIDECAR, fixture.name);
        } else {
            for output in &outputs {
                assert!(
                    !output.join(CATALOG_SIDECAR).exists(),
                    "{} must not produce a tool schema catalog",
                    fixture.name
                );
            }
        }
    }
}

#[tokio::test]
async fn generated_prompt_artifacts_worker() {
    let (Ok(source), Ok(output)) = (std::env::var(SOURCE_ENV), std::env::var(OUTPUT_ENV)) else {
        return;
    };
    let source = PathBuf::from(source);
    let output = PathBuf::from(output);
    let staging = tempfile::tempdir().expect("temporary agent directory");
    copy_agent_tree_for_build(&source, staging.path()).expect("copy fixture");

    let agent_dir = AgentDir::new(staging.path().to_path_buf()).expect("agent directory");
    let build_dir = BuildDir::new().expect("build directory");
    RuntimeTypeGenerator::new()
        .generate(&agent_dir, &build_dir)
        .await
        .expect("generate prompt artifacts");

    fs::create_dir_all(&output).expect("create worker output directory");
    copy_artifact(
        &build_dir.join("baml_src").join(RUNTIME_PRELUDE),
        &output.join(RUNTIME_PRELUDE),
    );
    let catalog = build_dir.join("baml_src").join(CATALOG_SIDECAR);
    if catalog.exists() {
        copy_artifact(&catalog, &output.join(CATALOG_SIDECAR));
    }
}

fn run_generation_process(source: &Path, output: &Path) {
    let status = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "generated_prompt_artifacts_worker",
            "--nocapture",
        ])
        .env(SOURCE_ENV, source)
        .env(OUTPUT_ENV, output)
        .env("BAML_REGISTRY_URL", "")
        .status()
        .expect("start generation process");
    assert!(
        status.success(),
        "generation process failed for {}",
        source.display()
    );
}

fn copy_artifact(source: &Path, destination: &Path) {
    let mut bytes =
        fs::read(source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    while matches!(bytes.last().copied(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    bytes.push(b'\n');
    fs::write(destination, bytes)
        .unwrap_or_else(|error| panic!("write {}: {error}", destination.display()));
}

fn assert_files_match(outputs: &[PathBuf], name: &str, fixture: &str) {
    let expected = fs::read(outputs[0].join(name))
        .unwrap_or_else(|error| panic!("read {name} for {fixture}: {error}"));
    for output in &outputs[1..] {
        assert_eq!(
            fs::read(output.join(name)).unwrap_or_else(|error| panic!("read {name}: {error}")),
            expected,
            "{fixture} {name} changed across processes"
        );
    }
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    format!("{:x}", Sha256::digest(bytes))
}
