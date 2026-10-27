// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed delegation lineage (`DelegationLinked`).
//!
//! One delegated tool session produces one `Delegation` entity linking the
//! parent task, the delegated child task, the caller and target stable `AgentId`
//! values, and the tool session. Callers query this instead of parsing an `a2a:`
//! context prefix or inferring a target from `message.metadata.kind`.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{
    DelegationLinkedSpec, GraphExporter, ProvEvent, ProvenanceWriter,
    vocabulary::{a2a, a2a_relations},
};
use test_support::testing::provenance_fixtures::build_isolated_store;

fn agent_id(seed: &str) -> AgentId {
    AgentId::from_uuid(UuidId::parse_str(seed).expect("valid uuid"))
}

fn delegation_event(
    ctx: &ContextId,
    parent_task_id: &TaskId,
    child_task_id: &TaskId,
    caller: &AgentId,
    target: &AgentId,
) -> ProvEvent {
    ProvEvent::delegation_linked(DelegationLinkedSpec {
        context_id: ctx.clone(),
        delegation_id: "delegation-1".to_string(),
        parent_task_id: parent_task_id.clone(),
        child_task_id: child_task_id.clone(),
        caller_agent_id: caller.clone(),
        target_agent_id: target.clone(),
        caller_name: "coordinator-agent".to_string(),
        target_name: "responder-agent".to_string(),
        tool_session_id: "tool-session-1".to_string(),
    })
}

#[tokio::test]
async fn delegation_lineage_links_parent_child_agents_and_session() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(4242, 1);
    let parent_task_id = TaskId::from_external(ExternalId::new("parent-task"));
    let child_task_id = TaskId::from_external(ExternalId::new("child-task"));
    let caller = agent_id("00000000-0000-0000-0000-0000000000aa");
    let target = agent_id("00000000-0000-0000-0000-0000000000bb");

    store
        .add_event(delegation_event(
            &ctx,
            &parent_task_id,
            &child_task_id,
            &caller,
            &target,
        ))
        .await
        .expect("write delegation lineage");

    // Retrying the same delegated session must re-link the same node, not append.
    store
        .add_event(delegation_event(
            &ctx,
            &parent_task_id,
            &child_task_id,
            &caller,
            &target,
        ))
        .await
        .expect("retry delegation lineage");

    let exported = GraphExporter::new(store.clone())
        .export_by_context(ctx.as_str())
        .await
        .expect("export context graph");

    let delegations: Vec<_> = exported
        .nodes
        .iter()
        .filter(|node| node.label == "Delegation")
        .collect();
    assert_eq!(
        delegations.len(),
        1,
        "one delegation node per (context, delegation_id): {delegations:?}"
    );
    let delegation = delegations[0];
    assert_eq!(
        delegation.properties.get(a2a::DELEGATION_ID),
        Some(&serde_json::Value::String("delegation-1".to_string())),
        "delegation identity is stored: {:?}",
        delegation.properties
    );
    assert_eq!(
        delegation.properties.get(a2a::DELEGATION_TOOL_SESSION_ID),
        Some(&serde_json::Value::String("tool-session-1".to_string())),
        "tool session correlates the request/reply exchange"
    );
    assert_eq!(
        delegation.properties.get(a2a::DELEGATION_CALLER_NAME),
        Some(&serde_json::Value::String("coordinator-agent".to_string())),
        "caller package-name snapshot"
    );
    assert_eq!(
        delegation.properties.get(a2a::DELEGATION_TARGET_NAME),
        Some(&serde_json::Value::String("responder-agent".to_string())),
        "target package-name snapshot"
    );

    let edge_to = |relation: &str| -> Vec<&str> {
        exported
            .edges
            .iter()
            .filter(|edge| edge.relation == relation)
            .map(|edge| edge.to.as_str())
            .collect()
    };
    let parent_targets = edge_to(a2a_relations::DELEGATION_PARENT);
    let child_targets = edge_to(a2a_relations::DELEGATION_CHILD);
    let caller_targets = edge_to(a2a_relations::DELEGATION_CALLER);
    let target_targets = edge_to(a2a_relations::DELEGATION_TARGET_AGENT);

    for (relation, found) in [
        (a2a_relations::DELEGATION_PARENT, &parent_targets),
        (a2a_relations::DELEGATION_CHILD, &child_targets),
        (a2a_relations::DELEGATION_CALLER, &caller_targets),
        (a2a_relations::DELEGATION_TARGET_AGENT, &target_targets),
    ] {
        assert_eq!(
            found.len(),
            1,
            "exactly one {relation} edge per delegation, got {found:?}"
        );
    }

    let node_label = |id: &str| -> Option<String> {
        exported
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.label.clone())
    };
    assert_eq!(
        node_label(parent_targets[0]).as_deref(),
        Some("A2ATask"),
        "delegation points at the parent task entity"
    );
    assert_eq!(
        node_label(child_targets[0]).as_deref(),
        Some("A2ATask"),
        "delegation points at the delegated child task entity"
    );
    assert_eq!(
        node_label(caller_targets[0]).as_deref(),
        Some("Agent"),
        "delegation points at the caller stable Agent node"
    );
    assert_eq!(
        node_label(target_targets[0]).as_deref(),
        Some("Agent"),
        "delegation points at the target stable Agent node"
    );
    assert_ne!(
        caller_targets[0], target_targets[0],
        "caller and target resolve to different stable Agent nodes"
    );
    assert_ne!(
        parent_targets[0], child_targets[0],
        "parent and child must be distinct task entities"
    );
}
