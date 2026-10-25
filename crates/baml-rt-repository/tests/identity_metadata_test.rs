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
    commands::{PublishCommand, PublishOrigin},
    entry::ChangeRationale,
    identity_digest_from_tar_gz,
    ids::{AgentName, ArtifactHash},
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
                Some(digest_v1.clone()),
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

    let result_digest_0 = results[0]
        .identity_digest
        .clone()
        .expect("publish result carries the identity digest");
    let result_digest_1 = results[1]
        .identity_digest
        .clone()
        .expect("publish result carries the identity digest");
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
            result.artifact_hash.as_ref(),
            "the stored entry records the blob it realizes"
        );
    }
}
