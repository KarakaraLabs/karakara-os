// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Authority test for atomic publication.
//!
//! Concurrent publishers must agree on version, content hash, and blob: every publisher gets
//! its own version, the entry records the blob it was built from, and no entry exists without
//! a retrievable blob.

use std::sync::Arc;

use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin, PublishResult},
    entry::ChangeRationale,
    ids::{AgentName, ArtifactHash},
    service::RepositoryService,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};

#[path = "support/common.rs"]
mod common;

const PUBLISHERS: usize = 6;
const AGENT_NAME: &str = "concurrent-agent";

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

/// Publish one built artifact exactly as the HTTP publish path does: reserve the version,
/// store the blob, then insert the entry at the reserved version.
async fn publish_one(
    svc: &RepositoryService,
    index: usize,
) -> baml_rt_repository::Result<PublishResult> {
    let name: AgentName = AGENT_NAME.parse().expect("valid agent name");
    let blob = format!("package bytes for publisher {index}").into_bytes();
    let artifact_hash = ArtifactHash::from_bytes(&blob);

    let reservation = svc.reserve_version(&name).await?;
    svc.put_built_blob(&artifact_hash, &blob).await?;

    let cmd = PublishCommand {
        name,
        source: common::make_source(
            &format!("export function run() {{ return {index}; }}"),
            AGENT_NAME,
            &[],
            "",
            &[],
        ),
        rationale: ChangeRationale::new(format!("publisher {index}")).expect("rationale"),
        origin: PublishOrigin::Original,
    };
    svc.publish_built(cmd, &reservation, artifact_hash, None)
        .await
}

#[tokio::test]
async fn concurrent_publishes_agree_on_version_identity_and_blob() {
    let store = Arc::new(common::setup_store().await);
    let svc = Arc::new(service(&store));

    let publishers: Vec<_> = (0..PUBLISHERS)
        .map(|index| {
            let svc = svc.clone();
            tokio::spawn(async move { publish_one(&svc, index).await })
        })
        .collect();

    let mut results = Vec::new();
    for publisher in publishers {
        let result = publisher
            .await
            .expect("publisher task must not panic")
            .expect("concurrent publish must succeed");
        results.push(result);
    }

    let mut versions: Vec<u32> = results
        .iter()
        .map(|result| result.version_ref.version.as_u32())
        .collect();
    versions.sort_unstable();
    assert_eq!(
        versions,
        (1..=PUBLISHERS as u32).collect::<Vec<_>>(),
        "every publisher must receive a distinct version"
    );

    for result in &results {
        let artifact_hash = result
            .artifact_hash
            .clone()
            .expect("publish result must carry the artifact hash");
        let entry = svc
            .get_by_hash(&result.hash)
            .await
            .expect("lookup published entry")
            .expect("published entry must exist");

        assert_eq!(
            entry.version_ref.version, result.version_ref.version,
            "the stored entry must keep the version the artifact was built for"
        );
        assert_eq!(
            entry.artifact_hash.as_ref(),
            Some(&artifact_hash),
            "the stored entry must record the blob it realizes"
        );

        let bytes = svc
            .get_blob(&artifact_hash)
            .await
            .expect("read published blob")
            .expect("published entry must have a retrievable blob");
        assert_eq!(
            ArtifactHash::from_bytes(&bytes),
            artifact_hash,
            "stored blob bytes must hash to the recorded artifact hash"
        );
    }

    let name: AgentName = AGENT_NAME.parse().expect("valid agent name");
    let versions = svc.list_versions(&name).await.expect("list versions");
    assert_eq!(versions.len(), PUBLISHERS);
    for header in versions {
        let entry = svc
            .get_by_hash(&header.hash)
            .await
            .expect("lookup entry")
            .expect("entry exists");
        let artifact_hash = entry
            .artifact_hash
            .expect("no entry may exist without an artifact hash");
        assert!(
            svc.get_blob(&artifact_hash)
                .await
                .expect("read blob")
                .is_some(),
            "no entry may exist without its blob"
        );
    }
}

#[tokio::test]
async fn expired_reservation_cannot_be_consumed() {
    use baml_rt_repository::{
        entry::{ChangeRationale, NewEntry},
        ids::{AgentName, Generation},
        lineage::Parentage,
    };

    let store = common::setup_store().await;
    let name: AgentName = "expired-agent".parse().expect("valid agent name");
    let reservation = store
        .reserve_version(&name, 1)
        .await
        .expect("reserve version");
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let entry = NewEntry {
        name: name.clone(),
        artifact_hash: None,
        identity_digest: None,
        source: common::make_source("v1", "expired-agent", &[], "", &[]),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("expired").expect("rationale"),
        tags: Vec::new(),
    };
    let error = store
        .insert_reserved_entry(&entry, &reservation)
        .await
        .expect_err("expired reservation must not publish");
    assert!(
        matches!(
            error,
            baml_rt_repository::error::RepositoryError::VersionConflict { .. }
        ),
        "expected VersionConflict, got {error:?}"
    );
}
