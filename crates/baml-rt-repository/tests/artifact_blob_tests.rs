// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Authority test for `ArtifactHash`: the identifier that names the stored package blob.
//!
//! Covers the publish handoff, the entry row, blob write verification, and blob read
//! verification. A corrupted blob must be rejected before anything extracts it.

use std::sync::Arc;

use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin},
    entry::ChangeRationale,
    ids::ArtifactHash,
    service::RepositoryService,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};

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

#[tokio::test]
async fn artifact_hash_verified_from_publish_to_boot() {
    let store = Arc::new(common::setup_store().await);
    let svc = service(&store);

    let blob = b"packaged agent bytes".to_vec();
    let artifact_hash = ArtifactHash::from_bytes(&blob);

    let published = svc
        .publish_built(
            PublishCommand {
                name: "artifact-agent".parse().expect("valid agent name"),
                source: common::make_source(
                    "export function run() {}",
                    "artifact-agent",
                    &[],
                    "",
                    &[],
                ),
                rationale: ChangeRationale::new("artifact handoff").expect("rationale"),
                origin: PublishOrigin::Original,
            },
            artifact_hash.clone(),
        )
        .await
        .expect("publish built artifact");

    assert_eq!(published.artifact_hash.as_ref(), Some(&artifact_hash));

    let entry = svc
        .get_by_hash(&published.hash)
        .await
        .expect("lookup published entry")
        .expect("entry exists");
    assert_eq!(
        entry.artifact_hash.as_ref(),
        Some(&artifact_hash),
        "the published entry must record which blob it realizes"
    );

    let wrong_hash = ArtifactHash::from_bytes(b"different bytes");
    let error = svc
        .put_built_blob(&wrong_hash, &blob)
        .await
        .expect_err("blob write must reject a hash that does not match the bytes");
    assert!(
        error.to_string().contains("Artifact hash mismatch"),
        "expected an artifact integrity failure, got: {error}"
    );

    svc.put_built_blob(&artifact_hash, &blob)
        .await
        .expect("store blob under its artifact hash");
    assert_eq!(
        svc.get_blob(&artifact_hash)
            .await
            .expect("read blob")
            .expect("blob exists"),
        blob
    );

    // Tamper with the stored bytes under the same key. A reader must fail closed
    // rather than hand corrupted bytes to an extractor.
    store
        .put_artifact(&artifact_hash, b"tampered bytes")
        .await
        .expect("overwrite stored blob");
    let error = svc
        .get_blob(&artifact_hash)
        .await
        .expect_err("tampered blob must be rejected on read");
    assert!(
        error.to_string().contains("Artifact hash mismatch"),
        "expected an artifact integrity failure, got: {error}"
    );
}
