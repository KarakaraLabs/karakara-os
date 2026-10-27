// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `system/introspection` `context_history`: the retrieval side of the
//! agent-scoped prompt view.
//!
//! One successful A→B delegation plus one failed delegation. The paged view must
//! reconstruct exactly what the prompt showed for the same window, including the
//! delegation dedup: the successful `system/internal_a2a` payload is hidden from
//! both agents, while the operator view keeps every durable record and the failed
//! delegation keeps its tool error.

use std::sync::Arc;

use baml_rt_conversation::view::{
    ConversationItemContent, ProvenanceConversationContextItem, ToolOutcome,
};
use baml_rt_core::{
    A2aRequestHandler, A2aStreamChunk, A2aWireRequest, AgentDiscoveryEntry, AgentLister,
    BamlRtError, BusStream, Outcome, Result,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, DelegationLinkedSpec, ProvEvent, ProvenanceQueryApi, ProvenanceWriter,
    SurrealProvenanceStore,
};
use baml_rt_tools::{ToolRegistry, ToolSessionId, ToolStep};
use baml_tools_system::SystemBundle;
use futures_util::stream;
use serde_json::{Value, json};
use test_support::testing::provenance_fixtures::build_isolated_store;

struct MockA2aHandler;

#[async_trait::async_trait]
impl A2aRequestHandler for MockA2aHandler {
    async fn handle_a2a_stream(
        &self,
        _request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        Ok(Box::pin(stream::empty::<A2aStreamChunk>()))
    }
}

struct EmptyAgentList;

impl AgentLister for EmptyAgentList {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        Vec::new()
    }
}

fn agent(seed: &str) -> AgentId {
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
            format!("{package}@1.0.0"),
        ))
        .await
        .expect("boot agent");
}

fn delegation(
    ctx: &ContextId,
    parent: &TaskId,
    child: &TaskId,
    caller: &AgentId,
    target: &AgentId,
    id: &str,
) -> ProvEvent {
    ProvEvent::delegation_linked(DelegationLinkedSpec {
        context_id: ctx.clone(),
        delegation_id: id.to_string(),
        parent_task_id: parent.clone(),
        child_task_id: child.clone(),
        caller_agent_id: caller.clone(),
        target_agent_id: target.clone(),
        caller_name: "coordinator-agent".to_string(),
        target_name: "responder-agent".to_string(),
        tool_session_id: format!("tool-session-{id}"),
    })
}

fn internal_a2a_call(
    ctx: &ContextId,
    task_id: &TaskId,
    caller: &AgentId,
    request: &str,
    result: Value,
    outcome: Outcome,
) -> ProvEvent {
    let mut metadata = json!({
        "agent_id": caller.as_str(),
        "args": { "step": { "op": "Send", "input": { "text": request } } },
    });
    if matches!(outcome, Outcome::Success) {
        metadata["result"] = result;
    } else {
        metadata["error"] = result;
    }
    ProvEvent::tool_call_completed_task(
        ctx.clone(),
        task_id.clone(),
        "system/internal_a2a".to_string(),
        Some("open".to_string()),
        json!({ "step": { "op": "Send", "input": { "text": request } } }),
        metadata,
        5,
        outcome,
        Some("responder-agent".to_string()),
    )
}

fn message_received(
    ctx: &ContextId,
    task_id: &TaskId,
    message: &str,
    text: &str,
    owner: &AgentId,
    timestamp_ms: u64,
) -> ProvEvent {
    ProvEvent::message_received_task(
        ctx.clone(),
        task_id.clone(),
        MessageId::from_external(ExternalId::new(message)),
        "ROLE_USER".to_string(),
        vec![text.to_string()],
        None,
        owner.clone(),
        timestamp_ms,
    )
}

fn message_sent(
    ctx: &ContextId,
    task_id: &TaskId,
    message: &str,
    text: &str,
    owner: &AgentId,
    timestamp_ms: u64,
) -> ProvEvent {
    ProvEvent::message_sent_task(
        ctx.clone(),
        task_id.clone(),
        MessageId::from_external(ExternalId::new(message)),
        "ROLE_AGENT".to_string(),
        vec![text.to_string()],
        None,
        owner.clone(),
        timestamp_ms,
        Vec::new(),
    )
}

async fn page(registry: &ToolRegistry, session: &ToolSessionId, send: Value) -> Value {
    registry
        .session_send(session, send)
        .await
        .expect("context history send");
    match registry
        .session_read(session, Value::Null)
        .await
        .expect("context history read")
    {
        ToolStep::Done {
            output: Some(output),
        } => output.get("contextHistory").cloned().unwrap_or_else(|| {
            panic!("context_history Send must return a contextHistory page, got {output}")
        }),
        other => panic!("expected Done(Some(output)), got {other:?}"),
    }
}

fn page_items(page: &Value) -> Vec<Value> {
    page.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn texts(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| {
            item.get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn count_text(items: &[Value], needle: &str) -> usize {
    texts(items).iter().filter(|text| *text == needle).count()
}

fn stored_row_key(item: &ProvenanceConversationContextItem) -> (String, String) {
    let kind = match &item.content {
        ConversationItemContent::Message { .. } => "message",
        ConversationItemContent::ToolCall(_) => "tool_call",
        ConversationItemContent::ToolResult(result) => match result.outcome {
            ToolOutcome::Error(_) => "tool_error",
            _ => "tool_result",
        },
        ConversationItemContent::SessionStep(_) => "session_step",
        ConversationItemContent::Planning(_) => "planning",
        ConversationItemContent::CompactionSummary { .. } => "compaction_summary",
        ConversationItemContent::Operational(_) => "operational",
    };
    (item.activity_anchor.to_string(), kind.to_string())
}

fn internal_a2a_rows(items: &[Value]) -> Vec<&Value> {
    items
        .iter()
        .filter(|item| {
            item.get("content")
                .and_then(|content| content.get("tool_name"))
                .and_then(Value::as_str)
                == Some("system/internal_a2a")
        })
        .collect()
}

/// Row identity for paging comparisons. One stored activity projects to a call
/// item and a result item, so the anchor alone is not unique.
fn row_keys(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|item| {
            let anchor = item
                .get("activityAnchor")
                .and_then(Value::as_str)?
                .to_string();
            let kind = item
                .get("content")
                .and_then(|content| content.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some((anchor, kind))
        })
        .collect()
}

/// Walk the opaque cursor to exhaustion, returning every page and every row.
async fn drain(
    registry: &ToolRegistry,
    session: &ToolSessionId,
    page_size: u32,
) -> (Vec<Value>, Vec<Value>) {
    let mut pages = Vec::new();
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut send = json!({
            "resource": "context_history",
            "pageSize": page_size,
        });
        if let Some(cursor) = &cursor {
            send["cursor"] = json!(cursor);
        }
        let page = page(registry, session, send).await;
        if let Some(next) = page.get("cursor").and_then(Value::as_str) {
            cursor = Some(next.to_string());
        }
        let has_more = page
            .get("hasMore")
            .and_then(Value::as_bool)
            .expect("hasMore is always present");
        let rows = page_items(&page);
        items.extend(rows.iter().cloned());
        if !has_more {
            pages.push(page);
            break;
        }
        assert!(
            cursor.is_some(),
            "a page with older rows must carry a cursor: {page}"
        );
        pages.push(page);
    }
    (pages, items)
}

#[tokio::test]
async fn context_history_pages_the_same_agent_view_the_prompt_uses() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9300, 1);
    let a = agent("00000000-0000-0000-0000-0000000000a1");
    let b = agent("00000000-0000-0000-0000-0000000000b1");
    boot(&store, &a, "coordinator-agent").await;
    boot(&store, &b, "responder-agent").await;

    let parent = task("task-parent");
    let child = task("task-child");
    let failed_child = task("task-failed-child");

    // An older caller row, so the elided delegation payload is not the oldest
    // window: a page that projects to nothing must be skipped, not returned.
    store
        .add_event(message_received(
            &ctx,
            &parent,
            "m-a-note",
            "earlier note",
            &a,
            1_000,
        ))
        .await
        .expect("earlier caller row");

    // Four durable records for the successful delegation, in durable order:
    // caller tool call, target's relayed request, target's reply, caller result.
    store
        .add_event(internal_a2a_call(
            &ctx,
            &parent,
            &a,
            "relayed request one",
            json!({ "text": "answer one" }),
            Outcome::Success,
        ))
        .await
        .expect("successful delegation tool call");
    store
        .add_event(message_received(
            &ctx,
            &child,
            "m-b-request",
            "relayed request one",
            &b,
            1_010,
        ))
        .await
        .expect("target relayed request");
    store
        .add_event(message_sent(
            &ctx,
            &child,
            "m-b-reply",
            "answer one",
            &b,
            1_011,
        ))
        .await
        .expect("target reply");
    // The caller receives that reply in its own task; this is the row A can read.
    store
        .add_event(message_received(
            &ctx,
            &parent,
            "m-a-reply",
            "answer one",
            &a,
            1_012,
        ))
        .await
        .expect("caller receives reply");
    store
        .add_event(delegation(&ctx, &parent, &child, &a, &b, "delegation-ok"))
        .await
        .expect("delegation lineage");

    // Failed delegation: lineage exists, the child never persisted a relay, so
    // the tool error must stay visible in A's view.
    store
        .add_event(internal_a2a_call(
            &ctx,
            &parent,
            &a,
            "relayed request two",
            json!({ "code": "a2a_delegation_failed", "message": "delegated agent failed" }),
            Outcome::Failure,
        ))
        .await
        .expect("failed delegation tool call");
    store
        .add_event(delegation(
            &ctx,
            &parent,
            &failed_child,
            &a,
            &b,
            "delegation-failed",
        ))
        .await
        .expect("failed delegation lineage");

    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_bundle(SystemBundle::new_with_provenance(
            Arc::new(EmptyAgentList),
            registry.clone(),
            Arc::new(MockA2aHandler),
            store.clone(),
        ))
        .expect("register system bundle");

    // --- Agent B: its own relayed request once, no caller tool payload. ---
    let b_session = registry
        .open_session("system/introspection", json!({}), &ctx, &b)
        .await
        .expect("open B session");
    let (b_pages, b_items) = drain(&registry, &b_session, 1).await;
    assert_eq!(
        count_text(&b_items, "relayed request one"),
        1,
        "B must see its relayed request exactly once, got {:?}",
        texts(&b_items)
    );
    assert!(
        internal_a2a_rows(&b_items).is_empty(),
        "the caller's delegation payload is hidden from B: {b_items:?}"
    );
    assert!(
        b_items.iter().all(|item| {
            item.get("target")
                .and_then(|target| target.get("agentId"))
                .and_then(Value::as_str)
                .is_some_and(|id| id == b.as_str())
                || item
                    .get("actor")
                    .and_then(|actor| actor.get("agentId"))
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == b.as_str())
        }),
        "every paged row is B's own participation: {b_items:?}"
    );

    // --- Agent A: B's reply once, the successful payload hidden, failure kept. ---
    let a_session = registry
        .open_session("system/introspection", json!({}), &ctx, &a)
        .await
        .expect("open A session");
    let (a_pages, a_items) = drain(&registry, &a_session, 1).await;
    assert_eq!(
        count_text(&a_items, "answer one"),
        1,
        "A must see B's reply exactly once, got {:?}",
        texts(&a_items)
    );
    let visible_tool_rows = internal_a2a_rows(&a_items);
    assert_eq!(
        visible_tool_rows.len(),
        2,
        "only the failed delegation's call and error stay visible, got {visible_tool_rows:?}"
    );
    assert!(
        visible_tool_rows.iter().all(|row| {
            row.get("content")
                .and_then(|content| content.get("kind"))
                .and_then(Value::as_str)
                != Some("tool_result")
        }),
        "the successful tool result is deduplicated: {visible_tool_rows:?}"
    );
    let failed_row = a_items
        .iter()
        .find(|item| {
            item.get("content")
                .and_then(|content| content.get("kind"))
                .and_then(Value::as_str)
                == Some("tool_error")
        })
        .expect("failed delegation keeps a visible tool error");
    assert_eq!(
        failed_row
            .get("content")
            .and_then(|content| content.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str),
        Some("delegated agent failed"),
        "the failure evidence itself stays readable"
    );
    assert_eq!(
        failed_row.get("eventKind").and_then(Value::as_str),
        Some("tool_operation"),
        "a paged row carries its stored event kind"
    );
    assert_eq!(
        failed_row
            .get("actor")
            .and_then(|actor| actor.get("agentId"))
            .and_then(Value::as_str),
        Some(a.as_str()),
        "a paged row carries its actor"
    );
    assert_eq!(
        failed_row.get("taskId").and_then(Value::as_str),
        Some(parent.as_str()),
        "a paged row carries its task"
    );
    assert!(
        failed_row
            .get("timestampMs")
            .and_then(Value::as_u64)
            .is_some_and(|order| order > 0),
        "a paged row carries its durable order"
    );

    // Paging must reconstruct the agent view exactly: same rows, no gap, no repeat.
    let full_a = store
        .context_history_view(&ctx, &a, None, None, None)
        .await
        .expect("full agent view");
    let mut expected = full_a.items.iter().map(stored_row_key).collect::<Vec<_>>();
    let mut paged = row_keys(&a_items);
    expected.sort();
    paged.sort();
    let before = paged.len();
    paged.dedup();
    assert_eq!(
        before,
        paged.len(),
        "paging must not repeat rows: {:?}",
        row_keys(&a_items)
    );
    assert_eq!(
        paged, expected,
        "concatenated pages reconstruct the agent view with no gaps"
    );
    assert_eq!(
        a_pages
            .last()
            .and_then(|page| page.get("hasMore"))
            .and_then(Value::as_bool),
        Some(false),
        "the last page reports exhaustion"
    );

    assert!(
        a_pages[..a_pages.len().saturating_sub(1)]
            .iter()
            .all(|page| !page_items(page).is_empty()),
        "a window that dedup emptied must not surface as an empty mid-history page: {a_pages:#?}"
    );
    // An exhausted cursor is terminal, not an error: an empty items array.
    let exhausted_cursor = a_pages
        .iter()
        .rev()
        .find(|page| !page_items(page).is_empty())
        .and_then(|page| page.get("cursor"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .expect("the last non-empty page carries its oldest-row cursor");
    let terminal = page(
        &registry,
        &a_session,
        json!({
            "resource": "context_history",
            "cursor": exhausted_cursor,
        }),
    )
    .await;
    assert_eq!(
        page_items(&terminal),
        Vec::<Value>::new(),
        "an exhausted page is an empty terminal page"
    );
    assert_eq!(
        terminal.get("hasMore").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        terminal.get("cursor").and_then(Value::as_str),
        None,
        "a terminal page has no next cursor"
    );

    // A malformed cursor is a typed error, never an empty page that reads as "done".
    let error = registry
        .session_send(
            &a_session,
            json!({
                "resource": "context_history",
                "cursor": "not-a-cursor",
            }),
        )
        .await
        .expect_err("a malformed context_history cursor must be rejected");
    match error {
        BamlRtError::ToolClassified(failure) => {
            assert_eq!(failure.code, "invalid_input");
            assert!(
                failure.message.contains("cursor"),
                "the typed error names the cursor: {failure:?}"
            );
        }
        other => panic!("expected a typed invalid_input error, got {other:?}"),
    }

    // A cursor minted for another agent's view is stale, not an empty page.
    let foreign_cursor = b_pages
        .iter()
        .find_map(|page| page.get("cursor").and_then(Value::as_str))
        .expect("B's first page carries a cursor");
    let error = registry
        .session_send(
            &a_session,
            json!({
                "resource": "context_history",
                "cursor": foreign_cursor,
            }),
        )
        .await
        .expect_err("another agent's cursor must be rejected");
    match error {
        BamlRtError::ToolClassified(failure) => {
            assert_eq!(failure.code, "invalid_input");
            assert!(
                failure.message.contains("stale"),
                "the typed error names the stale scope: {failure:?}"
            );
        }
        other => panic!("expected a typed invalid_input error, got {other:?}"),
    }

    // The operator read is not agent-scoped and still keeps every durable record.
    let operator_items = store
        .query_conversation_context(&ctx, None, None, None)
        .await
        .expect("operator full-context read");
    let mut operator_has_call = false;
    let mut operator_has_result = false;
    let mut operator_texts = Vec::new();
    for item in &operator_items {
        match &item.content {
            ConversationItemContent::ToolCall(call) => {
                operator_has_call |= call.tool_name == "system/internal_a2a"
                    && serde_json::to_string(&call.args)
                        .is_ok_and(|args| args.contains("relayed request one"));
            }
            ConversationItemContent::ToolResult(result) => {
                if result.tool_name == "system/internal_a2a"
                    && let ToolOutcome::Result(value) = &result.outcome
                {
                    operator_has_result |= serde_json::to_string(value)
                        .is_ok_and(|value| value.contains("answer one"));
                }
            }
            ConversationItemContent::Message { text, .. } => operator_texts.push(text.clone()),
            _ => {}
        }
    }
    assert!(
        operator_has_call && operator_has_result,
        "operator view must keep the successful delegation tool call and result"
    );
    for expected in ["relayed request one", "answer one"] {
        assert!(
            operator_texts.iter().any(|text| text == expected),
            "operator view must keep {expected:?}: {operator_texts:?}"
        );
    }
}
