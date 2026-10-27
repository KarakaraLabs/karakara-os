// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Context revision pinning: compare-and-set admission and operator audit rows.

use baml_rt_core::{
    ArtifactHash,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{BindingResolution, ContextRouteBindingRequest, ProvenanceError};
use serde_json::Value;
use test_support::testing::provenance_fixtures::build_isolated_store;

fn agent_id(suffix: &str) -> AgentId {
    AgentId::from_uuid(
        UuidId::parse_str(&format!("00000000-0000-0000-0000-0000000000{suffix}")).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_turns_across_a_deploy_pin_one_binding() {
    let store = build_isolated_store().await;
    let context_id = ContextId::from("ctx-binding-deploy");
    let package = "notion-agent";

    // A deploy between two turns changes both identities: new source bytes
    // produce a new AgentId, and the executable package changes hash too.
    let old_agent = agent_id("b1");
    let new_agent = agent_id("b2");
    let old_artifact = ArtifactHash::from_bytes(b"notion-agent-v1");
    let new_artifact = ArtifactHash::from_bytes(b"notion-agent-v2");

    let first = store.resolve_context_route_binding(ContextRouteBindingRequest::new(
        context_id.clone(),
        package,
        old_agent.clone(),
        old_artifact.clone(),
    ));
    let second = store.resolve_context_route_binding(ContextRouteBindingRequest::new(
        context_id.clone(),
        package,
        new_agent.clone(),
        new_artifact.clone(),
    ));
    let (first, second) = tokio::join!(first, second);

    let created = [first, second]
        .into_iter()
        .filter_map(|result| match result {
            Ok(BindingResolution::Created(binding)) => Some(binding),
            Ok(BindingResolution::Existing(binding)) => {
                panic!("first participation must create the binding, not reuse {binding:?}")
            }
            Err(ProvenanceError::ContextRouteBindingConflict(_)) => None,
            Err(error) => panic!("unexpected binding error: {error:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        created.len(),
        1,
        "exactly one first turn may bind the route"
    );
    let created = created.into_iter().next().unwrap();

    let pinned = store
        .read_context_route_binding(&context_id, package)
        .await
        .expect("read binding")
        .expect("binding row");
    assert_eq!(pinned, created);

    // The losing turn is a typed conflict naming both pinned identifiers,
    // never an execution against the other revision. Re-running the losing
    // pair is refused again rather than overwriting the binding.
    let conflict = store
        .resolve_context_route_binding(ContextRouteBindingRequest::new(
            context_id.clone(),
            package,
            if created.agent_id == old_agent {
                new_agent.clone()
            } else {
                old_agent.clone()
            },
            if created.artifact_hash == old_artifact {
                new_artifact.clone()
            } else {
                old_artifact.clone()
            },
        ))
        .await
        .expect_err("a different resolved pair must not execute");
    let ProvenanceError::ContextRouteBindingConflict(conflict) = conflict else {
        panic!("expected a typed context route binding conflict");
    };
    let conflicted_context = conflict.context_id;
    let package_name = conflict.package_name;
    let pinned_agent_id = conflict.pinned_agent_id;
    let pinned_artifact_hash = conflict.pinned_artifact_hash;
    assert_eq!(conflicted_context, context_id.as_str());
    assert_eq!(package_name, package);
    assert_eq!(pinned_agent_id, created.agent_id.as_str());
    assert_eq!(pinned_artifact_hash, created.artifact_hash.as_str());

    let mut response = store
        .db()
        .query(
            "SELECT * FROM prov_node \
             WHERE label = 'AuditDecision' ORDER BY props.a2a_event_order",
        )
        .await
        .expect("audit rows query");
    let rows: Vec<Value> = response.take(0).expect("audit rows");
    let kinds: Vec<&str> = rows
        .iter()
        .filter_map(|row| {
            row.get("props")
                .and_then(|props| props.get("audit_decision_kind"))
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "binding_created")
            .count(),
        1,
        "one create row per bound route: {kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "binding_conflict")
            .count(),
        1,
        "a retried refusal replays one decision row, not one per attempt: {kinds:?}"
    );
    assert_eq!(kinds.len(), 2, "no other audit rows: {kinds:?}");
}
