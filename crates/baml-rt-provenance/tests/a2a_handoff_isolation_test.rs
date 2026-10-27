// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P3 A2A handoff isolation: child archives stay private, the child reply is
//! actor-attributed with typed delegation lineage, and unrelated siblings stay blind.

use baml_rt_conversation::view::ConversationItemContent;
use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, DelegationLinkedSpec, ProvEvent, ProvenanceReadPrincipal, ProvenanceWriter,
    SurrealProvenanceStore,
    vocabulary::{a2a, a2a_relations},
};
use baml_rt_tools::{archive_read::render_to_lines, archive_refs::ArchiveEntry};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;

fn agent_id(seed: &str) -> AgentId {
    AgentId::from_uuid(UuidId::parse_str(seed).expect("valid uuid"))
}

fn task(name: &str) -> TaskId {
    TaskId::from_external(ExternalId::new(name))
}

async fn boot(store: &SurrealProvenanceStore, agent: &AgentId, package: &str) {
    store
        .add_event(ProvEvent::agent_booted(
            agent.clone(),
            AgentType::new(package).expect("valid agent type"),
            "1.0.0".to_string(),
            format!("{package}@1.0.0").to_string(),
        ))
        .await
        .expect("boot agent");
}

async fn ensure_task(
    store: &SurrealProvenanceStore,
    ctx: &ContextId,
    task_id: &TaskId,
    agent: &AgentId,
) {
    store
        .add_event(ProvEvent::task_exists(ctx.clone(), task_id.clone()))
        .await
        .expect("task exists");
    store
        .add_event(ProvEvent::task_execution_started(
            ctx.clone(),
            task_id.clone(),
            agent.clone(),
        ))
        .await
        .expect("task execution started");
}

fn sample_entry(anchor: &str) -> ArchiveEntry {
    ArchiveEntry::new(
        render_to_lines(&json!({"tasks": [{"id": "task-21", "status": "in progress"}]})),
        "support/clickup".to_string(),
        None,
        anchor.to_string(),
        "tool_result".to_string(),
    )
}

#[tokio::test]
async fn coordinator_cannot_resolve_child_archive_ref() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9700, 1);
    let coordinator = agent_id("00000000-0000-0000-0000-0000000000a1");
    let clickup = agent_id("00000000-0000-0000-0000-0000000000b1");
    boot(&store, &coordinator, "coordinator-agent").await;
    boot(&store, &clickup, "clickup-agent").await;

    let short = store
        .archive_allocate_and_put(
            &ctx,
            &clickup,
            "clickup-anchor",
            sample_entry("clickup-anchor"),
        )
        .await
        .expect("child allocates its own archive");

    let owner_read = store
        .archive_get_body(
            &ctx,
            short,
            &ProvenanceReadPrincipal::Agent(clickup.clone()),
        )
        .await
        .expect("child read");
    assert!(owner_read.is_some(), "child reads its own archive");

    let coordinator_read = store
        .archive_get_body(
            &ctx,
            short,
            &ProvenanceReadPrincipal::Agent(coordinator.clone()),
        )
        .await
        .expect("coordinator read");
    assert!(
        coordinator_read.is_none(),
        "coordinator must not resolve a child-owned archive ref"
    );

    let operator_read = store
        .archive_get_body(&ctx, short, &ProvenanceReadPrincipal::Operator)
        .await
        .expect("operator read");
    assert!(
        operator_read.is_some(),
        "operator still resolves every body in the context"
    );
}

#[tokio::test]
async fn child_reply_is_actor_attributed_and_delegation_links_target() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9701, 1);
    let coordinator = agent_id("00000000-0000-0000-0000-0000000000a2");
    let clickup = agent_id("00000000-0000-0000-0000-0000000000b2");
    let parent_task = task("coordinator-task");
    let child_task = task("clickup-child-task");
    boot(&store, &coordinator, "coordinator-agent").await;
    boot(&store, &clickup, "clickup-agent").await;
    ensure_task(&store, &ctx, &parent_task, &coordinator).await;
    ensure_task(&store, &ctx, &child_task, &clickup).await;

    // The child's final A2A reply: a real message, actor = child.
    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            child_task.clone(),
            MessageId::from_external(ExternalId::new("clickup-reply")),
            "ROLE_AGENT".to_string(),
            vec!["Task21 and Task16 are in progress".to_string()],
            None,
            clickup.clone(),
            1_900_000_500_000,
            Vec::new(),
        ))
        .await
        .expect("child reply persisted");

    // Typed delegation lineage: parent task -> child task, caller -> target agent.
    store
        .add_event(ProvEvent::delegation_linked(DelegationLinkedSpec {
            context_id: ctx.clone(),
            delegation_id: "delegation-p3".to_string(),
            parent_task_id: parent_task.clone(),
            child_task_id: child_task.clone(),
            caller_agent_id: coordinator.clone(),
            target_agent_id: clickup.clone(),
            caller_name: "coordinator-agent".to_string(),
            target_name: "clickup-agent".to_string(),
            tool_session_id: "tool-session-p3".to_string(),
        }))
        .await
        .expect("delegation lineage");

    let child_page = store
        .context_history_view(&ctx, &clickup, Some(100), None, None)
        .await
        .expect("child view");
    let reply = child_page
        .items
        .iter()
        .find(|item| {
            matches!(
                &item.content,
                ConversationItemContent::Message { text, .. }
                    if text == "Task21 and Task16 are in progress"
            )
        })
        .expect("child reply visible to the child actor");

    let attribution = reply
        .attribution
        .as_deref()
        .expect("child reply carries attribution");
    assert_eq!(
        attribution.actor.as_ref().map(|a| &a.agent_id),
        Some(&clickup),
        "child reply actor is the child agent"
    );
    assert!(
        attribution.target.is_none(),
        "message attribution currently records one direction; the target link is carried by typed delegation lineage"
    );
    assert!(
        reply.is_visible_to_agent(&clickup),
        "child reply is visible to its actor"
    );

    // The typed lineage is the target link: the coordinator is the caller, the child the target.
    let exported = baml_rt_provenance::GraphExporter::new(store.clone())
        .export_by_context(ctx.as_str())
        .await
        .expect("export context graph");
    let delegation = exported
        .nodes
        .iter()
        .find(|node| node.label == "Delegation")
        .expect("one delegation node");
    let props = &delegation.properties;
    assert_eq!(
        props
            .get(a2a::DELEGATION_CALLER_NAME)
            .and_then(serde_json::Value::as_str),
        Some("coordinator-agent"),
        "delegation records the caller"
    );
    assert_eq!(
        props
            .get(a2a::DELEGATION_TARGET_NAME)
            .and_then(serde_json::Value::as_str),
        Some("clickup-agent"),
        "delegation records the target"
    );

    let caller_targets: Vec<&str> = exported
        .edges
        .iter()
        .filter(|edge| edge.relation == a2a_relations::DELEGATION_CALLER)
        .map(|edge| edge.to.as_str())
        .collect();
    let target_targets: Vec<&str> = exported
        .edges
        .iter()
        .filter(|edge| edge.relation == a2a_relations::DELEGATION_TARGET_AGENT)
        .map(|edge| edge.to.as_str())
        .collect();
    assert_eq!(caller_targets.len(), 1, "one caller edge");
    assert_eq!(target_targets.len(), 1, "one target edge");
    assert_ne!(caller_targets[0], target_targets[0], "caller != target");
}

#[tokio::test]
async fn unrelated_sibling_sees_neither_delegation_nor_participant_archives() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9702, 1);
    let coordinator = agent_id("00000000-0000-0000-0000-0000000000a3");
    let clickup = agent_id("00000000-0000-0000-0000-0000000000b3");
    let notion = agent_id("00000000-0000-0000-0000-0000000000c3");
    let parent_task = task("coordinator-task");
    let child_task = task("clickup-child-task");
    let notion_task = task("notion-task");
    boot(&store, &coordinator, "coordinator-agent").await;
    boot(&store, &clickup, "clickup-agent").await;
    boot(&store, &notion, "notion-agent").await;
    ensure_task(&store, &ctx, &parent_task, &coordinator).await;
    ensure_task(&store, &ctx, &child_task, &clickup).await;
    ensure_task(&store, &ctx, &notion_task, &notion).await;

    // The coordinator's delegated Send (coordinator-owned) and the child's reply (child-owned).
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            parent_task.clone(),
            "system/internal_a2a".to_string(),
            None,
            serde_json::json!({"text": "list tasks"}),
            serde_json::json!({"agent_id": coordinator.as_str(), "task_id": parent_task.as_str()}),
            100,
            baml_rt_core::Outcome::Success,
            None,
        ))
        .await
        .expect("coordinator internal_a2a send");
    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            child_task.clone(),
            MessageId::from_external(ExternalId::new("clickup-reply")),
            "ROLE_AGENT".to_string(),
            vec!["two in-progress tasks".to_string()],
            None,
            clickup.clone(),
            1_900_000_500_001,
            Vec::new(),
        ))
        .await
        .expect("child reply");
    store
        .add_event(ProvEvent::delegation_linked(DelegationLinkedSpec {
            context_id: ctx.clone(),
            delegation_id: "delegation-p3-sibling".to_string(),
            parent_task_id: parent_task,
            child_task_id: child_task,
            caller_agent_id: coordinator.clone(),
            target_agent_id: clickup.clone(),
            caller_name: "coordinator-agent".to_string(),
            target_name: "clickup-agent".to_string(),
            tool_session_id: "tool-session-p3-sibling".to_string(),
        }))
        .await
        .expect("delegation lineage");

    let clickup_archive = store
        .archive_allocate_and_put(
            &ctx,
            &clickup,
            "clickup-anchor",
            sample_entry("clickup-anchor"),
        )
        .await
        .expect("child archive");

    // The unrelated sibling sees no delegation exchange messages and no participant archives.
    let sibling_page = store
        .context_history_view(&ctx, &notion, Some(100), None, None)
        .await
        .expect("sibling view");
    for item in &sibling_page.items {
        assert!(
            item.is_visible_to_agent(&notion),
            "sibling view must contain only notion-visible rows: {item:?}"
        );
        if let ConversationItemContent::Message { text, .. } = &item.content {
            assert!(
                !text.contains("list tasks") && !text.contains("two in-progress tasks"),
                "sibling must not see the delegation exchange: {text}"
            );
        }
    }
    assert!(
        sibling_page.items.iter().all(|item| !matches!(
            &item.content,
            ConversationItemContent::ToolCall(tc) if tc.tool_name == "system/internal_a2a"
        )),
        "sibling must not see the coordinator's internal_a2a tool call"
    );

    let sibling_archive_read = store
        .archive_get_body(
            &ctx,
            clickup_archive,
            &ProvenanceReadPrincipal::Agent(notion.clone()),
        )
        .await
        .expect("sibling archive read");
    assert!(
        sibling_archive_read.is_none(),
        "sibling cannot read a participant's archive"
    );

    // Operator still sees every archive in the context.
    let operator_read = store
        .archive_get_body(&ctx, clickup_archive, &ProvenanceReadPrincipal::Operator)
        .await
        .expect("operator archive read");
    assert!(
        operator_read.is_some(),
        "operator still resolves the child archive"
    );
}
