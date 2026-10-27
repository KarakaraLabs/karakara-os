// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P2 isolation: archive bodies, pending receipts, prefixes, and `#N` history refs are scoped to
//! the reading agent. Operators keep context-wide access.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceContextReader, ProvenanceReadPrincipal, ProvenanceWriter,
    SurrealProvenanceStore, hydrate_ref_table,
};
use baml_rt_tools::{
    archive_read::render_to_lines,
    archive_refs::ArchiveEntry,
    citations::{ParsedCitation, ResolvedCitation},
};
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
        render_to_lines(&json!({"k": "v"})),
        "support/clickup".to_string(),
        None,
        anchor.to_string(),
        "tool_result".to_string(),
    )
}

#[tokio::test]
async fn archive_body_reads_are_agent_scoped() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9500, 1);
    let owner = agent_id("00000000-0000-0000-0000-0000000000a1");
    let sibling = agent_id("00000000-0000-0000-0000-0000000000b1");
    boot(&store, &owner, "clickup-agent").await;
    boot(&store, &sibling, "notion-agent").await;

    let short = store
        .archive_allocate_and_put(&ctx, &owner, "owner-anchor", sample_entry("owner-anchor"))
        .await
        .expect("allocate archive");

    let owner_read = store
        .archive_get_body(&ctx, short, &ProvenanceReadPrincipal::Agent(owner.clone()))
        .await
        .expect("owner read");
    assert!(owner_read.is_some(), "owner must read its own archive body");

    let sibling_read = store
        .archive_get_body(
            &ctx,
            short,
            &ProvenanceReadPrincipal::Agent(sibling.clone()),
        )
        .await
        .expect("sibling read");
    assert!(
        sibling_read.is_none(),
        "sibling must not read a foreign body"
    );

    let operator_read = store
        .archive_get_body(&ctx, short, &ProvenanceReadPrincipal::Operator)
        .await
        .expect("operator read");
    assert!(operator_read.is_some(), "operator reads every body");
}

#[tokio::test]
async fn archive_consume_is_owner_scoped() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9501, 1);
    let owner = agent_id("00000000-0000-0000-0000-0000000000a2");
    let sibling = agent_id("00000000-0000-0000-0000-0000000000b2");
    boot(&store, &owner, "clickup-agent").await;
    boot(&store, &sibling, "notion-agent").await;

    let entry = sample_entry("consume-anchor")
        .with_send_signature(Some("support/clickup\u{0}{\"query\":\"x\"}".to_string()));
    let short = store
        .archive_allocate_and_put(&ctx, &owner, "consume-anchor", entry)
        .await
        .expect("allocate archive");

    store
        .archive_mark_send_consumed(&ctx, short, &sibling)
        .await
        .expect("sibling consume attempt");
    let after_sibling = store
        .archive_get_body(&ctx, short, &ProvenanceReadPrincipal::Operator)
        .await
        .expect("read after sibling consume")
        .expect("body present");
    assert!(
        after_sibling.pending_send,
        "a sibling cannot clear the owner's pending receipt"
    );

    store
        .archive_mark_send_consumed(&ctx, short, &owner)
        .await
        .expect("owner consume");
    let after_owner = store
        .archive_get_body(&ctx, short, &ProvenanceReadPrincipal::Operator)
        .await
        .expect("read after owner consume")
        .expect("body present");
    assert!(
        !after_owner.pending_send,
        "the owner clears its own pending receipt"
    );
}

#[tokio::test]
async fn archive_prefix_allocation_is_unique_and_stable() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9502, 1);
    let agents: Vec<AgentId> = (0..8)
        .map(|i| agent_id(&format!("00000000-0000-0000-0000-{i:012x}")))
        .collect();

    // Concurrent allocation: each agent mints its own prefix.
    let mut handles = Vec::new();
    for agent in &agents {
        let store = Arc::clone(&store);
        let ctx = ctx.clone();
        let agent = agent.clone();
        handles.push(tokio::spawn(async move {
            store.archive_ensure_prefix(&ctx, &agent).await
        }));
    }
    let mut prefixes = Vec::with_capacity(agents.len());
    for handle in handles {
        prefixes.push(handle.await.expect("join").expect("ensure prefix"));
    }

    let mut sorted = prefixes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        agents.len(),
        "concurrent allocation must yield unique prefixes: {prefixes:?}"
    );

    // Stable re-read: the same agent always gets the same prefix.
    for (agent, expected) in agents.iter().zip(&prefixes) {
        let again = store
            .archive_ensure_prefix(&ctx, agent)
            .await
            .expect("re-read prefix");
        assert_eq!(again, *expected, "prefix must be stable per agent");
    }
}

#[tokio::test]
async fn agent_history_hydration_excludes_sibling_refs() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9503, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000a3");
    let b = agent_id("00000000-0000-0000-0000-0000000000b3");
    let task_a = task("task-a");
    let task_b = task("task-b");
    boot(&store, &a, "clickup-agent").await;
    boot(&store, &b, "notion-agent").await;
    ensure_task(&store, &ctx, &task_a, &a).await;
    ensure_task(&store, &ctx, &task_b, &b).await;

    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_a.clone(),
            MessageId::from_external(ExternalId::new("msg-a")),
            "ROLE_USER".to_string(),
            vec!["clickup visible row".to_string()],
            None,
            a.clone(),
            1_900_000_500_000,
        ))
        .await
        .expect("message a");
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_b,
            MessageId::from_external(ExternalId::new("msg-b")),
            "ROLE_USER".to_string(),
            vec!["notion visible row".to_string()],
            None,
            b.clone(),
            1_900_000_500_001,
        ))
        .await
        .expect("message b");

    let items = store
        .conversation_context(&ctx, None)
        .await
        .expect("conversation context");
    let anchor_a = items
        .iter()
        .find(|i| {
            matches!(
                &i.content,
                baml_rt_conversation::view::ConversationItemContent::Message { text, .. }
                    if text == "clickup visible row"
            )
        })
        .expect("anchor a")
        .activity_anchor
        .as_str()
        .to_string();
    let anchor_b = items
        .iter()
        .find(|i| {
            matches!(
                &i.content,
                baml_rt_conversation::view::ConversationItemContent::Message { text, .. }
                    if text == "notion visible row"
            )
        })
        .expect("anchor b")
        .activity_anchor
        .as_str()
        .to_string();

    let n_a = store
        .history_ref_ensure(&ctx, &anchor_a, "message")
        .await
        .expect("ensure a");
    let n_b = store
        .history_ref_ensure(&ctx, &anchor_b, "message")
        .await
        .expect("ensure b");
    assert_ne!(n_a, n_b, "global #N assignment stays unique");

    let mut known_a = HashMap::new();
    known_a.insert(anchor_a.clone(), "clickup visible row".to_string());
    let table_a = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Agent(a.clone()),
        &known_a,
    )
    .await
    .expect("hydrate agent a");

    let parsed_a = ParsedCitation::parse(&format!("#{n_a}")).expect("parse a");
    let parsed_b = ParsedCitation::parse(&format!("#{n_b}")).expect("parse b");
    assert!(
        ResolvedCitation::resolve(&parsed_a, &table_a).is_some(),
        "the reader resolves its own #N"
    );
    assert!(
        ResolvedCitation::resolve(&parsed_b, &table_a).is_none(),
        "the reader must not resolve a sibling's #N"
    );

    let table_op = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Operator,
        &HashMap::new(),
    )
    .await
    .expect("hydrate operator");
    assert!(
        ResolvedCitation::resolve(&parsed_b, &table_op).is_some(),
        "the operator resolves every #N"
    );
}
