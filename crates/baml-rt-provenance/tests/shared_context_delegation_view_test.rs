// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P0.1 groundwork: which agent's view carries the `system/internal_a2a` payload row,
//! and when does delegation dedup elide it?
//!
//! This pins the *scope* of `is_elided_delegation_payload` before the vertical-slice
//! regression is built. The internal_a2a call is executed by the caller, so the
//! `A2A_TASK_CALL -> WAS_EXECUTED_BY` traversal should make it visible to the caller,
//! not the delegated child.

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::{
    Outcome,
    bus::SessionStepOp,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, CallScope, DelegationLinkedSpec, ProvEvent, ProvenanceWriter, SurrealProvenanceStore,
};
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

fn tool_contents(items: &[ProvenanceConversationContextItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.content {
            ConversationItemContent::ToolCall(tc) => Some(tc.tool_name.clone()),
            ConversationItemContent::ToolResult(tr) => Some(tr.tool_name.clone()),
            ConversationItemContent::SessionStep(ss) => Some(ss.tool_name.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn internal_a2a_payload_is_caller_scoped_and_elided_for_the_caller() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9400, 1);
    let caller = agent_id("00000000-0000-0000-0000-0000000000ca");
    let target = agent_id("00000000-0000-0000-0000-0000000000b1");
    let parent_task = task("coordinator-task");
    let child_task = task("clickup-child-task");

    boot(&store, &caller, "coordinator-agent").await;
    boot(&store, &target, "clickup-agent").await;
    ensure_task(&store, &ctx, &parent_task, &caller).await;
    ensure_task(&store, &ctx, &child_task, &target).await;

    // The coordinator's internal_a2a Send, marked Success. Caller-owned.
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            parent_task.clone(),
            "system/internal_a2a".to_string(),
            None,
            serde_json::json!({"text": "list tasks in progress"}),
            serde_json::json!({"agent_id": caller.as_str(), "task_id": parent_task.as_str()}),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("internal_a2a tool call completed");

    // The relayed inbound message the child receives.
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            child_task.clone(),
            MessageId::from_external(ExternalId::new("relayed-request")),
            "ROLE_USER".to_string(),
            vec!["list tasks in progress".to_string()],
            None,
            target.clone(),
            1_900_000_000_000,
        ))
        .await
        .expect("relayed message received");

    // Before lineage: no elision, caller sees the internal_a2a payload.
    let caller_before = store
        .context_history_view(&ctx, &caller, None, None, None)
        .await
        .expect("caller view before lineage");
    let caller_tools_before = tool_contents(&caller_before.items);
    assert!(
        caller_tools_before
            .iter()
            .any(|t| t == "system/internal_a2a"),
        "caller must see its own internal_a2a call before lineage is persisted: {caller_tools_before:?}"
    );

    // The child must never see the caller's internal_a2a call (caller-executed).
    let child_before = store
        .context_history_view(&ctx, &target, None, None, None)
        .await
        .expect("child view before lineage");
    let child_tools_before = tool_contents(&child_before.items);
    assert!(
        !child_tools_before
            .iter()
            .any(|t| t == "system/internal_a2a"),
        "child must not see the caller's internal_a2a payload: {child_tools_before:?}"
    );

    // Persist delegation lineage; the relayed message already exists, so now
    // `persisted_delegations` is non-empty and the caller's payload row is elided.
    store
        .add_event(ProvEvent::delegation_linked(DelegationLinkedSpec {
            context_id: ctx.clone(),
            delegation_id: "delegation-1".to_string(),
            parent_task_id: parent_task.clone(),
            child_task_id: child_task.clone(),
            caller_agent_id: caller.clone(),
            target_agent_id: target.clone(),
            caller_name: "coordinator-agent".to_string(),
            target_name: "clickup-agent".to_string(),
            tool_session_id: "tool-session-1".to_string(),
        }))
        .await
        .expect("delegation lineage");

    let caller_after = store
        .context_history_view(&ctx, &caller, None, None, None)
        .await
        .expect("caller view after lineage");
    let caller_tools_after = tool_contents(&caller_after.items);
    assert!(
        !caller_tools_after
            .iter()
            .any(|t| t == "system/internal_a2a"),
        "once the relayed message is persisted, the caller's internal_a2a payload is elided: {caller_tools_after:?}"
    );
}

/// The step-executor failure mechanism: `context_history_view` applies its window to **raw**
/// rows (Message/ToolCall/SessionStep), then projects/filters. A filtered raw row (here a
/// SessionStep `Open`) can therefore evict a *visible* row from the projection when the window
/// is at capacity, producing `p_after = p_before − {one visible row}` with no new line.
#[tokio::test]
async fn filtered_raw_row_evicts_visible_row_when_window_is_full() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9401, 1);
    let agent = agent_id("00000000-0000-0000-0000-0000000000d1");
    let task_id = task("child-task");
    boot(&store, &agent, "clickup-agent").await;
    ensure_task(&store, &ctx, &task_id, &agent).await;

    // 41 visible messages: one more than DEFAULT_LLM_CONTEXT_ITEM_CAP (40).
    let cap = baml_rt_provenance::DEFAULT_LLM_CONTEXT_ITEM_CAP;
    let total = cap + 1;
    for i in 0..total {
        store
            .add_event(ProvEvent::message_received_task(
                ctx.clone(),
                task_id.clone(),
                MessageId::from_external(ExternalId::new(format!("msg-{i}"))),
                "ROLE_USER".to_string(),
                vec![format!("row {i}")],
                None,
                agent.clone(),
                1_900_000_100_000 + i as u64,
            ))
            .await
            .expect("message received");
    }

    let before = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("view before filtered raw row");
    let before_anchors: Vec<String> = before
        .items
        .iter()
        .map(|item| item.activity_anchor.to_string())
        .collect();
    assert_eq!(
        before_anchors.len(),
        cap,
        "before: exactly the capped window is visible"
    );

    // The precise trigger: a session-phase ToolCall `Send` with **empty args** (ClickUp's
    // `list_teams` / `list_spaces` / `list_lists` all have empty input structs). It is fetched
    // (outcome Success) but `include_call` is false for `Send` with empty args, so the whole
    // ToolCall + ToolResult pair projects to nothing and evicts a visible row.
    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            task_id.clone(),
            "support/clickup".to_string(),
            None,
            serde_json::json!({}),
            serde_json::json!({
                "agent_id": agent.as_str(),
                "task_id": task_id.as_str(),
                "phase": "send"
            }),
            100,
            Outcome::Success,
            None,
        ))
        .await
        .expect("empty-args send tool call completed");

    let after = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("view after filtered raw row");
    let after_anchors: Vec<String> = after
        .items
        .iter()
        .map(|item| item.activity_anchor.to_string())
        .collect();

    // The raw window rotated: the oldest visible message was evicted, and the empty-args
    // Send ToolCall projected to nothing. This is exactly the "reordered or suppressed only"
    // shape that `hop_lines_from_provider_delta` rejects.
    assert_eq!(
        after_anchors.len(),
        cap - 1,
        "a filtered raw row evicts a visible row instead of appending: before={before_anchors:?} after={after_anchors:?}"
    );
    for anchor in &after_anchors {
        assert!(
            before_anchors.contains(anchor),
            "no new visible row may appear: {anchor}"
        );
    }
}

/// SessionStep rows (Open/SendDone/SearchRead/PageRead) must be visible in the agent's own
/// view. They are linked to the task entity (`A2A_TASK_SESSION_STEP`) and the task's
/// `WAS_LAST_EXECUTED_BY` head pointer, not the `A2A_TASK_CALL` path used by ToolCall rows.
/// This is the regression that broke the coordinator discovery loop and the ClickUp flow.
#[tokio::test]
async fn session_step_rows_are_visible_to_the_agent() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9402, 1);
    let agent = agent_id("00000000-0000-0000-0000-0000000000e1");
    let task_id = task("clickup-child-task");
    boot(&store, &agent, "clickup-agent").await;
    ensure_task(&store, &ctx, &task_id, &agent).await;

    // The step executor's archive read: a SessionStep PageRead that must appear in the
    // agent's own view.
    store
        .add_event(ProvEvent::tool_session_step(
            ctx.clone(),
            CallScope::Task {
                task_id: task_id.clone(),
            },
            "support/clickup".to_string(),
            "session-1".to_string(),
            &SessionStepOp::PageRead {
                archive_ref: "@1".to_string(),
                offset: 0,
                limit: 10,
            },
        ))
        .await
        .expect("session step page read");

    let page = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("agent view");

    let has_session_step = page
        .items
        .iter()
        .any(|item| matches!(&item.content, ConversationItemContent::SessionStep(_)));
    assert!(
        has_session_step,
        "agent view must include its own SessionStep rows; got: {:?}",
        page.items
    );
}

/// Appending coordinator and Notion rows to a shared context must not change any row already
/// returned to ClickUp. This pins both sibling exclusion and stable identity, order, and rendering
/// across consecutive agent-scoped reads.
#[tokio::test]
async fn sibling_activity_between_clickup_reads_does_not_change_clickup_view() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9403, 1);
    let clickup = agent_id("00000000-0000-0000-0000-0000000000c1");
    let coordinator = agent_id("00000000-0000-0000-0000-0000000000c2");
    let notion = agent_id("00000000-0000-0000-0000-0000000000c3");
    let clickup_task = task("clickup-task");
    let coordinator_task = task("coordinator-task");
    let notion_task = task("notion-task");

    boot(&store, &clickup, "clickup-agent").await;
    boot(&store, &coordinator, "coordinator-agent").await;
    boot(&store, &notion, "notion-agent").await;
    ensure_task(&store, &ctx, &clickup_task, &clickup).await;
    ensure_task(&store, &ctx, &coordinator_task, &coordinator).await;
    ensure_task(&store, &ctx, &notion_task, &notion).await;

    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            clickup_task.clone(),
            MessageId::from_external(ExternalId::new("clickup-request")),
            "ROLE_USER".to_string(),
            vec!["list tasks in progress".to_string()],
            None,
            clickup.clone(),
            1_900_000_300_000,
        ))
        .await
        .expect("ClickUp request");
    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            clickup_task.clone(),
            MessageId::from_external(ExternalId::new("clickup-response")),
            "ROLE_AGENT".to_string(),
            vec!["two tasks are in progress".to_string()],
            None,
            clickup.clone(),
            1_900_000_300_001,
            Vec::new(),
        ))
        .await
        .expect("ClickUp response");

    let before = store
        .context_history_view(&ctx, &clickup, Some(100), None, None)
        .await
        .expect("ClickUp view before sibling activity");
    assert!(!before.truncated, "ClickUp fixture must fit in one page");
    let before_items = serde_json::to_value(&before.items).expect("serialize ClickUp view");

    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            coordinator_task,
            MessageId::from_external(ExternalId::new("coordinator-update")),
            "ROLE_AGENT".to_string(),
            vec!["coordinator private update".to_string()],
            None,
            coordinator,
            1_900_000_300_002,
            Vec::new(),
        ))
        .await
        .expect("coordinator sibling activity");
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            notion_task,
            MessageId::from_external(ExternalId::new("notion-request")),
            "ROLE_USER".to_string(),
            vec!["notion private request".to_string()],
            None,
            notion,
            1_900_000_300_003,
        ))
        .await
        .expect("Notion sibling activity");

    let after = store
        .context_history_view(&ctx, &clickup, Some(100), None, None)
        .await
        .expect("ClickUp view after sibling activity");
    assert!(!after.truncated, "ClickUp fixture must fit in one page");
    let after_items = serde_json::to_value(&after.items).expect("serialize ClickUp view");

    assert_eq!(
        after_items, before_items,
        "sibling appends must not change ClickUp row identity, order, or rendering"
    );
    assert_eq!(
        after.cursor, before.cursor,
        "sibling appends must not move ClickUp's paging cursor"
    );
}
