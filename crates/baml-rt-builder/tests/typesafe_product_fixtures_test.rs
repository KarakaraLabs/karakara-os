// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Compile the read-only TypeSafe product fixtures to prove the one-shot
//! author surface (`invokeTool` + `ToolOutcome`) type-checks against the
//! generated declarations.

#![cfg(feature = "typesafe")]

use std::{fs, path::Path, process::Command};

#[test]
fn typesafe_product_fixtures_compile() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let fixtures = ["typesafe-alert-routing", "typesafe-draft-guardrail"];

    for fixture in fixtures {
        let dir = workspace
            .join("tests")
            .join("fixtures")
            .join("agents")
            .join(fixture);
        let index = fs::read_to_string(dir.join("src/index.ts"))
            .unwrap_or_else(|e| panic!("read {fixture} index.ts: {e}"));
        assert!(
            index.contains("from \"./baml-runtime\""),
            "{fixture} must import generated declarations, not copy DTOs"
        );
        run_tsc_no_emit(&dir);
    }
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
                "tsc rejected the {} fixture:\nstdout:\n{}\nstderr:\n{}",
                project.display(),
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
