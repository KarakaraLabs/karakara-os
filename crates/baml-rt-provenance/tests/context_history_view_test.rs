// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent-scoped prompt read: `ContextHistoryView(context_id, agent_id, window)`.
//!
//! One context, several agents, several tasks. Agent B must read its own
//! participation across its tasks and must not read the caller's or a sibling's
//! events.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, ContextHistoryCursor, ProvEvent, ProvenanceWriter, SurrealProvenanceStore,
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
