// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Agent-scoped prompt read: `ContextHistoryView(context_id, agent_id, window)`.
//!
//! One context, several agents, several tasks. Agent B must read its own
//! participation across its tasks and must not read the caller's or a sibling's
//! events.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, ContextHistoryCursor, DEFAULT_LLM_CONTEXT_ITEM_CAP, ProvEvent, ProvenanceWriter,
    SurrealProvenanceStore,
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

async fn received(
    store: &SurrealProvenanceStore,
    ctx: &ContextId,
    task_id: &TaskId,
    message: &str,
    text: &str,
    agent: &AgentId,
    ts: u64,
) {
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new(message)),
            "ROLE_USER".to_string(),
            vec![text.to_string()],
            None,
            agent.clone(),
            ts,
        ))
        .await
        .expect("received message");
}

async fn sent(
    store: &SurrealProvenanceStore,
    ctx: &ContextId,
    task_id: &TaskId,
    message: &str,
    text: &str,
    agent: &AgentId,
    ts: u64,
) {
    store
        .add_event(ProvEvent::message_sent_task(
            ctx.clone(),
            task_id.clone(),
            MessageId::from_external(ExternalId::new(message)),
            "ROLE_AGENT".to_string(),
            vec![text.to_string()],
            None,
            agent.clone(),
            ts,
            Vec::new(),
        ))
        .await
        .expect("sent message");
}

/// Context with: A on T1 delegating twice to B (T2, T3) and a sibling D on T4.
async fn seed_two_task_delegation() -> (std::sync::Arc<SurrealProvenanceStore>, ContextId, AgentId)
{
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9100, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000a1");
    let b = agent_id("00000000-0000-0000-0000-0000000000b1");
    let d = agent_id("00000000-0000-0000-0000-0000000000d1");
    boot(&store, &a, "coordinator-agent").await;
    boot(&store, &b, "responder-agent").await;
    boot(&store, &d, "sibling-agent").await;

    let t1 = task("task-parent");
    let t2 = task("task-first-delegation");
    let t3 = task("task-second-delegation");
    let t4 = task("task-sibling");

    // Caller A opens the context.
    received(&store, &ctx, &t1, "m-a-in", "route this work", &a, 1_000).await;
    sent(&store, &ctx, &t1, "m-a-out", "delegating", &a, 1_001).await;

    // First delegation: B reads the relayed request and answers.
    received(
        &store,
        &ctx,
        &t2,
        "m-b1-in",
        "relayed request one",
        &b,
        1_010,
    )
    .await;
    sent(&store, &ctx, &t2, "m-b1-out", "answer one", &b, 1_011).await;

    // Second delegation in the same context: new task, same agent.
    received(
        &store,
        &ctx,
        &t3,
        "m-b2-in",
        "relayed request two",
        &b,
        1_020,
    )
    .await;
    sent(&store, &ctx, &t3, "m-b2-out", "answer two", &b, 1_021).await;

    // Unrelated sibling activity in the same context.
    received(
        &store,
        &ctx,
        &t4,
        "m-d-in",
        "sibling private input",
        &d,
        1_030,
    )
    .await;
    sent(
        &store,
        &ctx,
        &t4,
        "m-d-out",
        "sibling private output",
        &d,
        1_031,
    )
    .await;

    (store, ctx, b)
}

fn texts(items: &[baml_rt_conversation::view::ProvenanceConversationContextItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match &item.content {
            baml_rt_conversation::view::ConversationItemContent::Message { text, .. } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn agent_reads_its_own_participation_across_tasks_and_no_siblings() {
    let (store, ctx, b) = seed_two_task_delegation().await;

    let page = store
        .context_history_view(&ctx, &b, None, None, None)
        .await
        .expect("agent-scoped read");

    let seen = texts(&page.items);
    for expected in [
        "relayed request one",
        "answer one",
        "relayed request two",
        "answer two",
    ] {
        assert!(
            seen.iter().any(|text| text == expected),
            "agent B must read its own row {expected:?} across both tasks; saw {seen:?}"
        );
    }
    for foreign in [
        "route this work",
        "delegating",
        "sibling private input",
        "sibling private output",
    ] {
        assert!(
            !seen.iter().any(|text| text == foreign),
            "agent B must not read another agent's row {foreign:?}; saw {seen:?}"
        );
    }
    assert!(
        !page.truncated,
        "no window was applied, so nothing is hidden"
    );
    assert!(
        page.cursor.is_some(),
        "a window always exposes a cursor for older-ward paging"
    );
    assert!(
        page.items.iter().all(|item| item.is_visible_to_agent(&b)),
        "every returned row is visible to the reading agent"
    );
}

#[tokio::test]
async fn call_message_placeholders_do_not_overwrite_transcript_order() {
    let (store, ctx, agent) = seed_two_task_delegation().await;
    let index_order = async || {
        let mut response = store
            .db()
            .query(
                "SELECT VALUE event_order FROM context_transcript_index \
                 WHERE context_id = $context_id AND node_id IN (\
                   SELECT VALUE node_id FROM prov_node \
                   WHERE label = 'Message' AND props.a2a_message_id = 'm-b1-in'\
                 )",
            )
            .bind(("context_id", ctx.as_str().to_string()))
            .await
            .expect("query message index order");
        let rows: Vec<u64> = response.take(0).expect("take message index order");
        rows.into_iter().next().expect("indexed message")
    };
    let before = index_order().await;

    store
        .add_event(ProvEvent::tool_call_completed_task(
            ctx.clone(),
            task("task-first-delegation"),
            "support/test".to_string(),
            None,
            serde_json::json!({}),
            serde_json::json!({
                "agent_id": agent.as_str(),
                "message_id": "m-b1-in",
                "task_id": "task-first-delegation",
            }),
            1,
            baml_rt_core::Outcome::Success,
            None,
        ))
        .await
        .expect("message-scoped tool call");

    assert_eq!(index_order().await, before);
}

#[tokio::test]
async fn agent_transcript_index_records_ownership_and_legacy_rows_fall_back() {
    let (store, ctx, agent) = seed_two_task_delegation().await;
    let agent_node_id = baml_rt_provenance::id_semantics::agent_node_id_string(&agent);
    let mut response = store
        .db()
        .query(
            "SELECT node_id, agent_node_id, projection_visible, agent_scope_indexed \
             FROM context_transcript_index \
             WHERE context_id = $context_id AND agent_node_id = $agent_node_id",
        )
        .bind(("context_id", ctx.as_str().to_string()))
        .bind(("agent_node_id", agent_node_id.clone()))
        .await
        .expect("query transcript index");
    let rows: Vec<serde_json::Value> = response.take(0).expect("take transcript index rows");
    assert!(
        !rows.is_empty(),
        "agent-owned transcript rows must be indexed"
    );
    assert!(rows.iter().all(|row| {
        row.get("agent_node_id").and_then(serde_json::Value::as_str) == Some(agent_node_id.as_str())
            && row
                .get("projection_visible")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && row
                .get("agent_scope_indexed")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));

    let indexed = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("index-backed view");
    store
        .db()
        .query(
            "UPDATE context_transcript_index SET agent_scope_indexed = NONE \
             WHERE context_id = $context_id",
        )
        .bind(("context_id", ctx.as_str().to_string()))
        .await
        .expect("mark rows as legacy")
        .check()
        .expect("legacy update");
    let legacy = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("legacy graph-backed view");

    assert_eq!(
        serde_json::to_value(&indexed.items).expect("serialize indexed items"),
        serde_json::to_value(&legacy.items).expect("serialize legacy items"),
        "legacy fallback must preserve the index-backed agent view"
    );
    assert_eq!(indexed.cursor, legacy.cursor);
    assert_eq!(indexed.truncated, legacy.truncated);
}

#[tokio::test]
async fn window_bounds_the_tail_and_reports_truncation() {
    let (store, ctx, b) = seed_two_task_delegation().await;

    let full = store
        .context_history_view(&ctx, &b, None, None, None)
        .await
        .expect("full read");
    let bounded = store
        .context_history_view(&ctx, &b, Some(2), None, None)
        .await
        .expect("bounded read");

    assert_eq!(
        bounded.items.len(),
        2,
        "the window bounds the automatic read"
    );
    assert!(
        bounded.truncated,
        "older visible rows exist outside the window"
    );
    let full_tail: Vec<_> = full
        .items
        .iter()
        .rev()
        .take(2)
        .map(|item| item.activity_anchor.clone())
        .collect();
    let bounded_anchors: Vec<_> = bounded
        .items
        .iter()
        .rev()
        .map(|item| item.activity_anchor.clone())
        .collect();
    assert_eq!(
        full_tail, bounded_anchors,
        "the window is the newest rows by the durable ordering key"
    );
    assert_ne!(
        bounded.cursor, full.cursor,
        "the cursor identifies the oldest row of the offered window"
    );

    // Paging older-ward from the cursor must reproduce exactly the full view: no
    // gaps, no duplicates, exhaustion reported as `truncated == false`.
    let mut paged: Vec<String> = Vec::new();
    let mut cursor = bounded
        .cursor
        .as_deref()
        .and_then(ContextHistoryCursor::decode);
    paged.extend(bounded.items.iter().map(|i| i.activity_anchor.to_string()));
    while let Some(ref from) = cursor {
        let page = store
            .context_history_view(&ctx, &b, Some(2), Some(from), None)
            .await
            .expect("paged read");
        paged.extend(page.items.iter().map(|i| i.activity_anchor.to_string()));
        if !page.truncated {
            break;
        }
        cursor = page
            .cursor
            .as_deref()
            .and_then(ContextHistoryCursor::decode);
    }
    let mut paged_sorted = paged.clone();
    paged_sorted.sort();
    let before = paged_sorted.len();
    paged_sorted.dedup();
    assert_eq!(
        before,
        paged_sorted.len(),
        "paging must not repeat rows across the cursor boundary: {paged:?}"
    );
    let mut full_sorted: Vec<String> = full
        .items
        .iter()
        .map(|i| i.activity_anchor.to_string())
        .collect();
    full_sorted.sort();
    assert_eq!(
        paged_sorted, full_sorted,
        "concatenated pages reconstruct the agent view with no gaps"
    );
}

/// `context_history_view` treats `window: None` as [`DEFAULT_LLM_CONTEXT_ITEM_CAP`], not as
/// "uncapped". The step-executor intra-turn read
/// (`conversation_history_json_for_intra_dedup` -> `project_conversation_to_json(scope, None)`)
/// passes `None` expecting an uncapped read, so it silently receives only the newest 40 rows.
///
/// This pins the semantic gap behind P0.3 of
/// `shared-context-agent-view-stability-and-isolation-plan.md`: an explicit large window is
/// uncapped, but `None` is not.
#[tokio::test]
async fn none_window_means_default_cap_not_uncapped() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9300, 1);
    let agent = agent_id("00000000-0000-0000-0000-0000000000c1");
    let task_id = task("cap-task");
    boot(&store, &agent, "clickup-agent").await;
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

    let total = DEFAULT_LLM_CONTEXT_ITEM_CAP + 5;
    for i in 0..total {
        received(
            &store,
            &ctx,
            &task_id,
            &format!("m-{i}"),
            &format!("row {i}"),
            &agent,
            1_900_000_000_000 + i as u64,
        )
        .await;
    }

    let default_read = store
        .context_history_view(&ctx, &agent, None, None, None)
        .await
        .expect("default (None) read");
    let explicit_large_read = store
        .context_history_view(&ctx, &agent, Some(total + 10), None, None)
        .await
        .expect("explicit large read");

    assert_eq!(
        default_read.items.len(),
        DEFAULT_LLM_CONTEXT_ITEM_CAP,
        "None is interpreted as the default cap, not uncapped"
    );
    assert!(
        default_read.truncated,
        "the default read hides older rows outside its window"
    );
    assert_eq!(
        explicit_large_read.items.len(),
        total,
        "an explicit window larger than the context is effectively uncapped"
    );
    assert!(
        !explicit_large_read.truncated,
        "the explicit large read leaves nothing hidden"
    );
}
