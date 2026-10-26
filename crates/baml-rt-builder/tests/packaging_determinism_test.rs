// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "dev-tools")]

use std::{fs, path::Path};

use baml_rt_builder::builder::{AgentDir, BuildDir, RuntimeTypeGenerator, traits::TypeGenerator};
use test_support::common::{agent_fixture, copy_agent_tree_for_build};

const GENERATED_MANIFESTS: [&str; 3] = [
    "session_plan_functions.json",
    "unified_step_executor_functions.json",
    "tool_step_executors.json",
];

#[tokio::test]
async fn packaging_is_byte_stable() {
    let fixture = agent_fixture("unified-step-harness-demo");
    let first = generate_manifest_bytes(&fixture).await;
    let second = generate_manifest_bytes(&fixture).await;

    for manifest in GENERATED_MANIFESTS {
        assert_eq!(
            first.get(manifest),
            second.get(manifest),
            "{manifest} changed between identical builds"
        );
    }
}

async fn generate_manifest_bytes(source: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let staging = tempfile::tempdir().expect("temporary agent directory");
    copy_agent_tree_for_build(source, staging.path()).expect("copy fixture");

    let agent_dir = AgentDir::new(staging.path().to_path_buf()).expect("agent directory");
    let build_dir = BuildDir::new().expect("build directory");
    RuntimeTypeGenerator::new()
        .generate(&agent_dir, &build_dir)
        .await
        .expect("generate fixture artifacts");

    GENERATED_MANIFESTS
        .into_iter()
        .map(|name| {
            let path = build_dir.join(name);
            let bytes = fs::read(&path).unwrap_or_else(|error| {
                panic!("read generated manifest {}: {error}", path.display())
            });
            (name.to_string(), bytes)
        })
        .collect()
}
