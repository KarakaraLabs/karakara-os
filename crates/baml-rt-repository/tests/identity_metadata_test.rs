// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Authority test for repository identity metadata.
//!
//! Republishing identical identity-covered content must keep the same identity digest while
//! bumping the repository version. The digest is written in the same statement as the entry and
//! returned alongside the artifact hash in the publish result.

use std::sync::Arc;

use baml_rt_repository::{
    commands::{ForkCommand, PublishCommand, PublishOrigin},
    entry::ChangeRationale,
    identity_digest_from_tar_gz,
    ids::{AgentName, ArtifactHash},
    lineage::EdgeDescription,
    service::RepositoryService,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};
use flate2::{Compression, write::GzEncoder};
use tar::{Builder, Header};

#[path = "support/common.rs"]
mod common;

fn service(store: &Arc<SurrealStore>) -> RepositoryService {
    RepositoryService::new(
        store.clone() as Arc<dyn BlobStore>,
        store.clone() as Arc<dyn MetadataStore>,
        store.clone() as Arc<dyn LineageStore>,
        store.clone() as Arc<dyn SearchStore>,
        store.clone() as Arc<dyn baml_rt_repository::McpRegistryStore>,
        store.clone() as Arc<dyn baml_rt_repository::ExternalToolRegistryStore>,
    )
}

/// Build a minimal packaged agent tar.gz with the given manifest version.
fn package(manifest_version: &str) -> Vec<u8> {
    let manifest = serde_json::json!({
        "version": manifest_version,
        "name": "republish-agent",
        "entry_point": "dist/index.js",
        "tools": [],
        "tags": []
    })
    .to_string();

    let entries: &[(&str, &[u8])] = &[
        ("manifest.json", manifest.as_bytes()),
        (
            "src/index.ts",
            b"export function run() { return 'hello'; }\n",
        ),
        ("baml_src/main.baml", b"function Run() -> string\n"),
        ("baml_src/_baml_runtime.baml", b"generated prompt\n"),
        ("dist/index.js", b"compiled output\n"),
    ];

    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = Builder::new(&mut gzip);
        for (path, content) in entries {
            let mut header = Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, *path, *content).unwrap();
        }
        tar.finish().unwrap();
    }
    gzip.finish().unwrap()
}

#[tokio::test]
async fn republish_keeps_identity_and_bumps_version() {
    let store = Arc::new(common::setup_store().await);
    let svc = service(&store);
    let name: AgentName = "republish-agent".parse().expect("valid agent name");

    let blob_v1 = package("1");
    let blob_v2 = package("2");

    let digest_v1 = identity_digest_from_tar_gz(&blob_v1).expect("valid package v1");
    let digest_v2 = identity_digest_from_tar_gz(&blob_v2).expect("valid package v2");
    assert_eq!(
        digest_v1, digest_v2,
        "repository version is excluded from the identity digest"
    );

    let source = common::make_source(
        "export function run() { return 'hello'; }",
        "republish-agent",
        &[],
        "republish test agent",
        &[],
    );

    let mut results = Vec::new();
    for blob in [&blob_v1, &blob_v2] {
        let artifact_hash = ArtifactHash::from_bytes(blob);
        let reservation = svc.reserve_version(&name).await.expect("reserve version");
        svc.put_built_blob(&artifact_hash, blob)
            .await
            .expect("store blob");
        let result = svc
            .publish_built(
                PublishCommand {
                    name: name.clone(),
                    source: source.clone(),
                    rationale: ChangeRationale::new("republish").expect("rationale"),
                    origin: PublishOrigin::Original,
                },
                &reservation,
                artifact_hash.clone(),
                digest_v1.clone(),
            )
            .await
            .expect("publish built artifact");
        results.push(result);
    }

    assert_eq!(
        results[0].version_ref.version.as_u32(),
        1,
        "first publish is v1"
    );
    assert_eq!(
        results[1].version_ref.version.as_u32(),
        2,
        "republish bumps the version"
    );

    let result_digest_0 = results[0].identity_digest.clone();
    let result_digest_1 = results[1].identity_digest.clone();
    assert_eq!(result_digest_0, digest_v1);
    assert_eq!(result_digest_1, digest_v1);

    assert_ne!(
        results[0].hash, results[1].hash,
        "the deployment content hash moves when the repository version moves"
    );
    assert_ne!(
        results[0].artifact_hash, results[1].artifact_hash,
        "different blobs have different artifact hashes"
    );

    for result in &results {
        let entry = svc
            .get_by_hash(&result.hash)
            .await
            .expect("lookup published entry")
            .expect("published entry exists");
        assert_eq!(
            entry.identity_digest.as_ref(),
            Some(&digest_v1),
            "the stored entry records the identity digest in the same write"
        );
        assert_eq!(
            entry.artifact_hash.as_ref(),
            Some(&result.artifact_hash),
            "the stored entry records the blob it realizes"
        );
    }
}

#[tokio::test]
async fn fork_built_returns_a_deployable_result_with_lineage() {
    let store = Arc::new(common::setup_store().await);
    let svc = service(&store);
    let parent_name: AgentName = "fork-built-parent".parse().expect("valid name");
    let child_name: AgentName = "fork-built-child".parse().expect("valid name");

    let parent_blob = package("parent");
    let parent_artifact = ArtifactHash::from_bytes(&parent_blob);
    let parent_digest = identity_digest_from_tar_gz(&parent_blob).expect("parent digest");
    let parent_reservation = svc
        .reserve_version(&parent_name)
        .await
        .expect("reserve parent");
    svc.put_built_blob(&parent_artifact, &parent_blob)
        .await
        .expect("store parent blob");
    let parent = svc
        .publish_built(
            PublishCommand {
                name: parent_name,
                source: common::make_source(
                    "export function run() { return 1; }",
                    "fork-built-parent",
                    &[],
                    "",
                    &[],
                ),
                rationale: ChangeRationale::new("parent").expect("rationale"),
                origin: PublishOrigin::Original,
            },
            &parent_reservation,
            parent_artifact,
            parent_digest.clone(),
        )
        .await
        .expect("publish parent");

    let child_blob = package("child");
    let child_artifact = ArtifactHash::from_bytes(&child_blob);
    let child_digest = identity_digest_from_tar_gz(&child_blob).expect("child digest");
    let child_reservation = svc
        .reserve_version(&child_name)
        .await
        .expect("reserve child");
    svc.put_built_blob(&child_artifact, &child_blob)
        .await
        .expect("store child blob");
    let child = svc
        .fork_built(
            ForkCommand {
                source_hash: parent.hash.clone(),
                new_name: child_name,
                source: common::make_source(
                    "export function run() { return 2; }",
                    "fork-built-child",
                    &[],
                    "",
                    &[],
                ),
                rationale: ChangeRationale::new("fork").expect("rationale"),
                fork_description: EdgeDescription::new("built fork").expect("description"),
                tags: Vec::new(),
            },
            &child_reservation,
            child_artifact,
            child_digest.clone(),
        )
        .await
        .expect("fork built");

    assert_eq!(child.artifact_hash, ArtifactHash::from_bytes(&child_blob));
    assert_eq!(child.identity_digest, child_digest);
    assert_eq!(child.generation, parent.generation.increment());
    let entry = svc
        .get_by_hash(&child.hash)
        .await
        .expect("lookup fork")
        .expect("fork entry");
    assert_eq!(entry.artifact_hash, Some(child.artifact_hash));
}
