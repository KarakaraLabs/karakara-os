// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P4.9: agent-prompt compaction summaries are private to the reading agent, while the
//! operator still reads the complete context. Two agents in one context receive different
//! summaries derived from their own views, and neither summary crosses those APIs.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::ConversationItemContent;
use baml_rt_core::{
    context::RuntimeScope,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId},
};
use baml_rt_llm_config::LlmClientConfig;
use baml_rt_provenance::{
    AgentType, CompactionPrefixInput, CompactionRequest, CompactionSummarizeError,
    CompactionTriggerInput, CompactionTriggerSource, ContextCompactionSubscriber,
    ConversationCompactionSummarizer, DEFAULT_LLM_CONTEXT_ITEM_CAP, ProvEvent, ProvenanceQueryApi,
    ProvenanceWriter, finalize_compaction_summary,
};
use baml_rt_tools::tools::ToolRegistry;
use test_support::testing::provenance_fixtures::{build_isolated_store, wall_clock_tick};

fn agent_id(seed: &str) -> AgentId {
    AgentId::from_uuid(baml_rt_core::ids::UuidId::parse_str(seed).expect("valid uuid"))
}

async fn boot(store: &baml_rt_provenance::SurrealProvenanceStore, agent: &AgentId, package: &str) {
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
    store: &baml_rt_provenance::SurrealProvenanceStore,
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

async fn seed_messages(
    store: &baml_rt_provenance::SurrealProvenanceStore,
    ctx: &ContextId,
    task_id: &TaskId,
    agent: &AgentId,
    label: &str,
    count: usize,
    base_ts: u64,
) {
    for i in 0..count {
        store
            .add_event(ProvEvent::message_received_task(
                ctx.clone(),
                task_id.clone(),
                MessageId::from_external(ExternalId::new(format!("{label}-msg-{i}"))),
                "ROLE_USER".to_string(),
                vec![format!("{label} ping {i}")],
                None,
                agent.clone(),
                base_ts + i as u64,
            ))
            .await
            .expect("seed message");
        wall_clock_tick().await;
    }
}

/// Returns a summary naming the marker found in the rendered source, so the test can prove the
/// summary was derived from exactly that agent's view.
struct SourceEchoSummarizer;

#[async_trait]
impl ConversationCompactionSummarizer for SourceEchoSummarizer {
    fn backend_label(&self) -> &'static str {
        "source-echo"
    }

    async fn summarize_prefix_attempt(
        &self,
        _scope: &RuntimeScope,
        input: &CompactionPrefixInput,
        _validation_feedback: Option<String>,
    ) -> Result<String, CompactionSummarizeError> {
        let who = if input.source_rendered.contains("agent-a") {
            "agent-a"
        } else if input.source_rendered.contains("agent-b") {
            "agent-b"
        } else {
            "unknown"
        };
        finalize_compaction_summary(&format!("summary derived from {who}"), &input.ref_table)
            .map_err(|e| CompactionSummarizeError::Validation(e.to_string()))
    }
}

fn subscriber(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
) -> ContextCompactionSubscriber {
    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    ContextCompactionSubscriber::new(
        store,
        writer,
        Arc::new(LlmClientConfig::sensible_default()),
        None,
        Arc::new(SourceEchoSummarizer),
        Arc::new(ToolRegistry::new()),
    )
}

fn force_trigger() -> CompactionTriggerInput {
    CompactionTriggerInput {
        source: CompactionTriggerSource::Manual,
        item_count: 0,
        prompt_bytes: 0,
        safety: Default::default(),
        force: true,
    }
}

#[tokio::test]
async fn two_agents_receive_private_summaries_while_operator_reads_full_context() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9800, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let b = agent_id("00000000-0000-0000-0000-0000000000bb");
    let task_a = TaskId::from_external(ExternalId::new("task-a"));
    let task_b = TaskId::from_external(ExternalId::new("task-b"));
    boot(&store, &a, "clickup-agent").await;
    boot(&store, &b, "notion-agent").await;
    ensure_task(&store, &ctx, &task_a, &a).await;
    ensure_task(&store, &ctx, &task_b, &b).await;

    let count = DEFAULT_LLM_CONTEXT_ITEM_CAP + 5;
    seed_messages(
        &store,
        &ctx,
        &task_a,
        &a,
        "agent-a",
        count,
        1_900_000_000_000,
    )
    .await;
    seed_messages(
        &store,
        &ctx,
        &task_b,
        &b,
        "agent-b",
        count,
        1_900_000_100_000,
    )
    .await;

    let subscriber = subscriber(Arc::clone(&store));
    subscriber
        .evaluate_and_compact(
            &CompactionRequest {
                context_id: ctx.clone(),
                agent_id: a.clone(),
            },
            force_trigger(),
            "default",
        )
        .await;
    subscriber
        .evaluate_and_compact(
            &CompactionRequest {
                context_id: ctx.clone(),
                agent_id: b.clone(),
            },
            force_trigger(),
            "default",
        )
        .await;

    let head_a = store
        .latest_compaction_head(&ctx, Some(&a), None)
        .await
        .expect("head a")
        .expect("agent-a compaction head");
    let head_b = store
        .latest_compaction_head(&ctx, Some(&b), None)
        .await
        .expect("head b")
        .expect("agent-b compaction head");

    assert_ne!(
        head_a.activity_anchor, head_b.activity_anchor,
        "each agent owns a distinct compaction head"
    );
    assert!(
        head_a.summary_text.contains("agent-a"),
        "agent-a summary must derive from agent-a rows: {}",
        head_a.summary_text
    );
    assert!(
        !head_a.summary_text.contains("agent-b"),
        "agent-a summary must not include agent-b rows: {}",
        head_a.summary_text
    );
    assert!(
        head_b.summary_text.contains("agent-b"),
        "agent-b summary must derive from agent-b rows: {}",
        head_b.summary_text
    );
    assert!(
        !head_b.summary_text.contains("agent-a"),
        "agent-b summary must not include agent-a rows: {}",
        head_b.summary_text
    );

    // The audit/operator summary slot stays empty: no context-wide summary exists.
    assert!(
        store
            .latest_compaction_head(&ctx, None, None)
            .await
            .expect("audit head")
            .is_none(),
        "no operator/audit summary should be written by agent-prompt compaction"
    );

    let prompt_a = store
        .conversation_context_for_agent_prompt(&ctx, &a, None, None)
        .await
        .expect("agent-a prompt");
    let prompt_b = store
        .conversation_context_for_agent_prompt(&ctx, &b, None, None)
        .await
        .expect("agent-b prompt");

    let summaries = |items: &[baml_rt_conversation::view::ProvenanceConversationContextItem]| {
        items
            .iter()
            .filter_map(|item| match &item.content {
                ConversationItemContent::CompactionSummary { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let summaries_a = summaries(&prompt_a);
    let summaries_b = summaries(&prompt_b);
    assert_eq!(summaries_a.len(), 1, "agent-a prompt has one summary");
    assert_eq!(summaries_b.len(), 1, "agent-b prompt has one summary");
    assert!(
        summaries_a[0].contains("agent-a") && !summaries_a[0].contains("agent-b"),
        "agent-a prompt carries only its own summary: {:?}",
        summaries_a
    );
    assert!(
        summaries_b[0].contains("agent-b") && !summaries_b[0].contains("agent-a"),
        "agent-b prompt carries only its own summary: {:?}",
        summaries_b
    );

    // Operator reads the complete context and never sees an agent summary.
    let operator = store
        .query_conversation_context(&ctx, Some(count * 2), None, None)
        .await
        .expect("operator query");
    assert!(
        operator.iter().all(|item| !matches!(
            item.content,
            ConversationItemContent::CompactionSummary { .. }
        )),
        "operator query must never carry an agent-prompt summary"
    );
    let has_a = operator.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::Message { text, .. } if text.contains("agent-a")
        )
    });
    let has_b = operator.iter().any(|item| {
        matches!(
            &item.content,
            ConversationItemContent::Message { text, .. } if text.contains("agent-b")
        )
    });
    assert!(has_a && has_b, "operator sees both agents' rows");
}
