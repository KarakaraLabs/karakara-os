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
    AgentCompactionRequest, AgentType, AuditCompactionOutcome, AuditCompactionProducer,
    AuditCompactionRequest, CURRENT_COMPACTION_PROJECTION_VERSION, CompactionPrefixInput,
    CompactionProjectionVersion, CompactionRequest, CompactionSummarizeError,
    CompactionSummarizeFailureKind, CompactionSummaryAudience, CompactionTriggerInput,
    CompactionTriggerSource, ContextCompactionSubscriber, ContextCompactionTrigger,
    ConversationCompactionSummarizer, DEFAULT_LLM_CONTEXT_ITEM_CAP, ProvEvent, ProvenanceQueryApi,
    ProvenanceWriter, finalize_compaction_summary, prepare_audit_compaction, prepare_compaction,
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
        let mut markers = Vec::new();
        if input.source_rendered.contains("agent-a") {
            markers.push("agent-a");
        }
        if input.source_rendered.contains("agent-b") {
            markers.push("agent-b");
        }
        let who = if markers.is_empty() {
            "unknown".to_string()
        } else {
            markers.join("+")
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
            &CompactionRequest::new(ctx.clone(), a.clone()),
            force_trigger(),
            "default",
        )
        .await;
    subscriber
        .evaluate_and_compact(
            &CompactionRequest::new(ctx.clone(), b.clone()),
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

/// P4 review finding 1: the compaction head is keyed by (context, agent, task), so two agents
/// compacting the *same* task must each persist an independent head. The old (context, task)
/// unique index made the second write collide.
#[tokio::test]
async fn two_agents_write_independent_task_scoped_heads_without_collision() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9801, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let b = agent_id("00000000-0000-0000-0000-0000000000bb");
    let task = TaskId::from_external(ExternalId::new("shared-task"));

    let event = |ctx: &ContextId, task: &TaskId, agent: &AgentId, text: &str| {
        ProvEvent::context_compaction_recorded_global(
            ctx.clone(),
            Some(agent.clone()),
            Some(task.clone()),
            CURRENT_COMPACTION_PROJECTION_VERSION,
            0,
            100,
            vec!["n-1".to_string()],
            text.to_string(),
            ContextCompactionTrigger::PostTurnThreshold,
            8,
            120,
            20,
            4000,
            100,
            format!("hash-{text}"),
            false,
        )
    };

    store
        .add_event(event(&ctx, &task, &a, "summary for A"))
        .await
        .expect("agent-a head write must not collide");
    store
        .add_event(event(&ctx, &task, &b, "summary for B"))
        .await
        .expect("agent-b head write must not collide");

    let head_a = store
        .latest_compaction_head(&ctx, Some(&a), Some(&task))
        .await
        .expect("read a")
        .expect("agent-a task head");
    let head_b = store
        .latest_compaction_head(&ctx, Some(&b), Some(&task))
        .await
        .expect("read b")
        .expect("agent-b task head");

    assert_eq!(head_a.summary_text, "summary for A");
    assert_eq!(head_b.summary_text, "summary for B");
    assert_ne!(
        head_a.activity_anchor, head_b.activity_anchor,
        "each agent owns a distinct task-scoped head"
    );
}

/// P4 review finding 2: a compacted prompt is the summary plus a *bounded* newest tail. A
/// deferred compaction that lets the tail grow must not produce an unbounded prompt.
#[tokio::test]
async fn compacted_prompt_tail_is_bounded() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9802, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let task = TaskId::from_external(ExternalId::new("task-bounded"));
    boot(&store, &a, "clickup-agent").await;
    ensure_task(&store, &ctx, &task, &a).await;

    let count = DEFAULT_LLM_CONTEXT_ITEM_CAP + 20;
    seed_messages(&store, &ctx, &task, &a, "agent-a", count, 1_900_000_200_000).await;

    let subscriber = subscriber(Arc::clone(&store));
    subscriber
        .evaluate_and_compact(
            &CompactionRequest::new(ctx.clone(), a.clone()),
            force_trigger(),
            "default",
        )
        .await;

    let head = store
        .latest_compaction_head(&ctx, Some(&a), None)
        .await
        .expect("head")
        .expect("agent-a compaction head");
    assert!(
        head.covered_event_order_end > 0,
        "compaction must cover a non-empty prefix"
    );

    // Read with a small bound: the result must respect it regardless of tail size.
    let limit = 5usize;
    let prompt = store
        .conversation_context_for_agent_prompt(&ctx, &a, Some(limit), None)
        .await
        .expect("prompt");
    assert!(
        prompt.len() <= limit,
        "compacted prompt must respect the limit: got {} items for limit {limit}",
        prompt.len()
    );
    assert!(
        matches!(
            prompt[0].content,
            ConversationItemContent::CompactionSummary { .. }
        ),
        "compacted prompt leads with the summary"
    );
}

fn audit_producer(
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    summarizer: Arc<dyn ConversationCompactionSummarizer>,
) -> AuditCompactionProducer {
    let writer: Arc<dyn ProvenanceWriter> = Arc::clone(&store) as Arc<dyn ProvenanceWriter>;
    AuditCompactionProducer::new(
        store,
        writer,
        Arc::new(LlmClientConfig::sensible_default()),
        summarizer,
        Arc::new(ToolRegistry::new()),
    )
}

struct FailingAuditSummarizer;

#[async_trait]
impl ConversationCompactionSummarizer for FailingAuditSummarizer {
    fn backend_label(&self) -> &'static str {
        "always-fail-audit"
    }

    async fn summarize_prefix_attempt(
        &self,
        _scope: &RuntimeScope,
        _input: &CompactionPrefixInput,
        _validation_feedback: Option<String>,
    ) -> Result<String, CompactionSummarizeError> {
        Err(CompactionSummarizeError::LlmInvoke(
            "simulated operator summarizer failure".into(),
        ))
    }
}

/// Issue #9: explicit operator audit compaction covers all sibling agents in the context,
/// persists strictly under `agent_id = None`, and never leaks into agent prompt reads.
#[tokio::test]
async fn operator_audit_compaction_covers_all_agents_and_stays_isolated_from_agent_prompts() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9810, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let b = agent_id("00000000-0000-0000-0000-0000000000bb");
    let task_a = TaskId::from_external(ExternalId::new("audit-task-a"));
    let task_b = TaskId::from_external(ExternalId::new("audit-task-b"));
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
        1_900_100_000_000,
    )
    .await;
    seed_messages(
        &store,
        &ctx,
        &task_b,
        &b,
        "agent-b",
        count,
        1_900_100_100_000,
    )
    .await;

    // Produce per-agent summaries first.
    let sub = subscriber(Arc::clone(&store));
    sub.evaluate_and_compact(
        &AgentCompactionRequest::new(ctx.clone(), a.clone()),
        force_trigger(),
        "default",
    )
    .await;
    sub.evaluate_and_compact(
        &AgentCompactionRequest::new(ctx.clone(), b.clone()),
        force_trigger(),
        "default",
    )
    .await;

    // Agent compaction must never populate the audit head.
    assert!(
        store
            .latest_audit_compaction_head(&ctx, None, CURRENT_COMPACTION_PROJECTION_VERSION)
            .await
            .expect("audit head lookup")
            .is_none(),
        "agent compaction must not write the operator/audit head"
    );

    let head_a_before = store
        .latest_agent_compaction_head(&ctx, &a, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("head a")
        .expect("agent-a head");
    let head_b_before = store
        .latest_agent_compaction_head(&ctx, &b, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("head b")
        .expect("agent-b head");

    // Now run the explicit operator audit compaction producer.
    let producer = audit_producer(Arc::clone(&store), Arc::new(SourceEchoSummarizer));
    let outcome = producer
        .compact_audit(&AuditCompactionRequest::new(ctx.clone()), true, false)
        .await;
    assert!(
        matches!(outcome, AuditCompactionOutcome::Succeeded { covered_rows, .. } if covered_rows > 0),
        "operator audit compaction must succeed: {outcome:?}"
    );

    let audit_head = store
        .latest_audit_compaction_head(&ctx, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("audit head")
        .expect("operator audit head must exist");
    assert_eq!(audit_head.agent_id, None, "audit head owns agent_id = None");
    assert_eq!(
        audit_head.projection_version,
        CURRENT_COMPACTION_PROJECTION_VERSION
    );
    assert!(
        audit_head.summary_text.contains("agent-a") && audit_head.summary_text.contains("agent-b"),
        "audit summary must include rows from both sibling agents: {}",
        audit_head.summary_text
    );

    // Agent heads remain untouched and private.
    let head_a_after = store
        .latest_agent_compaction_head(&ctx, &a, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("head a after")
        .expect("agent-a head after");
    let head_b_after = store
        .latest_agent_compaction_head(&ctx, &b, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("head b after")
        .expect("agent-b head after");
    assert_eq!(head_a_after.activity_anchor, head_a_before.activity_anchor);
    assert_eq!(head_b_after.activity_anchor, head_b_before.activity_anchor);
    assert!(
        head_a_after.summary_text.contains("agent-a")
            && !head_a_after.summary_text.contains("agent-b")
    );
    assert!(
        head_b_after.summary_text.contains("agent-b")
            && !head_b_after.summary_text.contains("agent-a")
    );

    // Agent prompt reads never return the audit summary.
    let prompt_a = store
        .conversation_context_for_agent_prompt(&ctx, &a, None, None)
        .await
        .expect("prompt a");
    let prompt_b = store
        .conversation_context_for_agent_prompt(&ctx, &b, None, None)
        .await
        .expect("prompt b");
    assert!(
        prompt_a
            .iter()
            .all(|item| item.activity_anchor != audit_head.activity_anchor),
        "agent-a prompt must never contain the audit head"
    );
    assert!(
        prompt_b
            .iter()
            .all(|item| item.activity_anchor != audit_head.activity_anchor),
        "agent-b prompt must never contain the audit head"
    );

    // Operator timeline picks up the audit compaction marker (never an agent head).
    let operator_timeline = store
        .query_conversation_context(&ctx, Some(count * 2), None, None)
        .await
        .expect("operator timeline");
    assert!(
        operator_timeline
            .iter()
            .any(|item| item.activity_anchor == audit_head.activity_anchor),
        "operator timeline should surface the audit compaction marker"
    );
    assert!(
        operator_timeline.iter().all(|item| {
            item.activity_anchor != head_a_after.activity_anchor
                && item.activity_anchor != head_b_after.activity_anchor
        }),
        "operator timeline must never surface an agent head"
    );

    // Full replay remains complete and ignores all compaction heads.
    let replay = store
        .conversation_context_for_operator(&ctx, None)
        .await
        .expect("full operator replay");
    let message_count = replay
        .iter()
        .filter(|item| matches!(item.content, ConversationItemContent::Message { .. }))
        .count();
    assert_eq!(
        message_count,
        count * 2,
        "raw provenance remains immutable and complete in full replay"
    );
}

/// Issue #9: agent lookups never return an audit head, and operator lookups never return an
/// agent head, even when only one of the two contracts has been compacted.
#[tokio::test]
async fn agent_lookup_never_returns_audit_head_and_operator_lookup_never_returns_agent_head() {
    let store = Arc::new(build_isolated_store().await);
    let ctx_audit_only = ContextId::new(9811, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let b = agent_id("00000000-0000-0000-0000-0000000000bb");
    let task_a = TaskId::from_external(ExternalId::new("audit-only-a"));
    let task_b = TaskId::from_external(ExternalId::new("audit-only-b"));
    boot(&store, &a, "clickup-agent").await;
    boot(&store, &b, "notion-agent").await;
    ensure_task(&store, &ctx_audit_only, &task_a, &a).await;
    ensure_task(&store, &ctx_audit_only, &task_b, &b).await;

    let count = DEFAULT_LLM_CONTEXT_ITEM_CAP + 5;
    seed_messages(
        &store,
        &ctx_audit_only,
        &task_a,
        &a,
        "agent-a",
        count,
        1_900_200_000_000,
    )
    .await;
    seed_messages(
        &store,
        &ctx_audit_only,
        &task_b,
        &b,
        "agent-b",
        count,
        1_900_200_100_000,
    )
    .await;

    // Only run operator audit compaction.
    let producer = audit_producer(Arc::clone(&store), Arc::new(SourceEchoSummarizer));
    let outcome = producer
        .compact_audit(
            &AuditCompactionRequest::new(ctx_audit_only.clone()),
            true,
            false,
        )
        .await;
    assert!(matches!(outcome, AuditCompactionOutcome::Succeeded { .. }));

    assert!(
        store
            .latest_audit_compaction_head(
                &ctx_audit_only,
                None,
                CURRENT_COMPACTION_PROJECTION_VERSION,
            )
            .await
            .expect("audit head")
            .is_some()
    );
    assert!(
        store
            .latest_agent_compaction_head(
                &ctx_audit_only,
                &a,
                None,
                CURRENT_COMPACTION_PROJECTION_VERSION,
            )
            .await
            .expect("agent a head")
            .is_none(),
        "agent-a lookup must never return the audit head"
    );
    assert!(
        store
            .latest_agent_compaction_head(
                &ctx_audit_only,
                &b,
                None,
                CURRENT_COMPACTION_PROJECTION_VERSION,
            )
            .await
            .expect("agent b head")
            .is_none(),
        "agent-b lookup must never return the audit head"
    );

    let prompt_a = store
        .conversation_context_for_agent_prompt(&ctx_audit_only, &a, None, None)
        .await
        .expect("prompt a");
    assert!(
        prompt_a.iter().all(|item| !matches!(
            item.content,
            ConversationItemContent::CompactionSummary { .. }
        )),
        "agent prompt must not receive the operator audit summary"
    );
}

/// Issue #9 invariants 10 & 12: audit preparation hydrates archive refs with Operator authority
/// across all agents (whereas agent preparation hydrates only its own refs), and a failed audit
/// summary attempt leaves the previous audit head untouched.
#[tokio::test]
async fn audit_compaction_hydrates_operator_refs_and_preserves_head_on_failure() {
    use baml_rt_tools::{
        archive_read::render_to_lines,
        archive_refs::ArchiveEntry,
        citations::{ParsedCitation, ResolvedCitation},
    };

    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9812, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let b = agent_id("00000000-0000-0000-0000-0000000000bb");
    let task_a = TaskId::from_external(ExternalId::new("refs-task-a"));
    let task_b = TaskId::from_external(ExternalId::new("refs-task-b"));
    boot(&store, &a, "clickup-agent").await;
    boot(&store, &b, "notion-agent").await;
    ensure_task(&store, &ctx, &task_a, &a).await;
    ensure_task(&store, &ctx, &task_b, &b).await;

    let entry = |anchor: &str| {
        ArchiveEntry::new(
            render_to_lines(&serde_json::json!({"anchor": anchor})),
            "support/tool".to_string(),
            None,
            anchor.to_string(),
            "tool_result".to_string(),
        )
    };
    let ref_a = store
        .archive_allocate_and_put(&ctx, &a, "anchor-a", entry("anchor-a"))
        .await
        .expect("archive a");
    let ref_b = store
        .archive_allocate_and_put(&ctx, &b, "anchor-b", entry("anchor-b"))
        .await
        .expect("archive b");

    // Seed enough messages mentioning each agent's archive ref so the prefix includes them.
    let count = DEFAULT_LLM_CONTEXT_ITEM_CAP + 5;
    let short_a = ref_a.to_string();
    let short_b = ref_b.to_string();
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_a.clone(),
            MessageId::from_external(ExternalId::new("ref-a-msg")),
            "ROLE_USER".to_string(),
            vec![format!("agent-a saw {short_a}")],
            None,
            a.clone(),
            1_900_300_000_000,
        ))
        .await
        .expect("msg a");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::message_received_task(
            ctx.clone(),
            task_b.clone(),
            MessageId::from_external(ExternalId::new("ref-b-msg")),
            "ROLE_USER".to_string(),
            vec![format!("agent-b saw {short_b}")],
            None,
            b.clone(),
            1_900_300_000_010,
        ))
        .await
        .expect("msg b");
    wall_clock_tick().await;
    seed_messages(
        &store,
        &ctx,
        &task_a,
        &a,
        "agent-a",
        count,
        1_900_300_001_000,
    )
    .await;
    seed_messages(
        &store,
        &ctx,
        &task_b,
        &b,
        "agent-b",
        count,
        1_900_300_100_000,
    )
    .await;

    let registry = ToolRegistry::new();
    let producer = audit_producer(Arc::clone(&store), Arc::new(SourceEchoSummarizer));
    let policy = producer.trigger_policy_for("default");

    let prepared_audit = prepare_audit_compaction(
        store.as_ref(),
        &registry,
        &policy,
        &AuditCompactionRequest::new(ctx.clone()),
        ContextCompactionTrigger::ManualOperator,
    )
    .await
    .expect("prepare audit")
    .expect("prepared audit");
    assert_eq!(prepared_audit.record.agent_id, None);
    assert_eq!(
        prepared_audit.input.audience,
        CompactionSummaryAudience::OperatorAudit
    );

    let parsed_a = ParsedCitation::parse(&short_a).expect("parse a");
    let parsed_b = ParsedCitation::parse(&short_b).expect("parse b");
    assert!(
        ResolvedCitation::resolve(&parsed_a, &prepared_audit.input.ref_table).is_some(),
        "operator audit preparation must hydrate agent-a archive ref {short_a}"
    );
    assert!(
        ResolvedCitation::resolve(&parsed_b, &prepared_audit.input.ref_table).is_some(),
        "operator audit preparation must hydrate agent-b archive ref {short_b}"
    );

    let prepared_agent_a = prepare_compaction(
        store.as_ref(),
        &registry,
        &policy,
        &AgentCompactionRequest::new(ctx.clone(), a.clone()),
        ContextCompactionTrigger::ManualOperator,
    )
    .await
    .expect("prepare agent a")
    .expect("prepared agent a");
    assert_eq!(prepared_agent_a.record.agent_id, Some(a.clone()));
    assert_eq!(
        prepared_agent_a.input.audience,
        CompactionSummaryAudience::AgentSelf
    );
    assert!(
        ResolvedCitation::resolve(&parsed_a, &prepared_agent_a.input.ref_table).is_some(),
        "agent-a preparation hydrates its own ref {short_a}"
    );
    assert!(
        ResolvedCitation::resolve(&parsed_b, &prepared_agent_a.input.ref_table).is_none(),
        "agent-a preparation must not hydrate sibling agent-b ref {short_b}"
    );

    // First audit compaction succeeds and establishes a head.
    let first = producer
        .compact_audit(&AuditCompactionRequest::new(ctx.clone()), true, false)
        .await;
    assert!(matches!(first, AuditCompactionOutcome::Succeeded { .. }));
    let original_head = store
        .latest_audit_compaction_head(&ctx, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("original head")
        .expect("audit head");

    // Second audit compaction fails during summarization: previous head must remain active.
    let failing_producer = audit_producer(Arc::clone(&store), Arc::new(FailingAuditSummarizer));
    let failed = failing_producer
        .compact_audit(&AuditCompactionRequest::new(ctx.clone()), true, false)
        .await;
    assert!(
        matches!(
            failed,
            AuditCompactionOutcome::SummarizeFailed {
                reason: CompactionSummarizeFailureKind::LlmInvoke
            }
        ),
        "failed audit compaction must report SummarizeFailed: {failed:?}"
    );
    let head_after_failure = store
        .latest_audit_compaction_head(&ctx, None, CURRENT_COMPACTION_PROJECTION_VERSION)
        .await
        .expect("head after failure")
        .expect("previous audit head preserved");
    assert_eq!(
        head_after_failure.activity_anchor, original_head.activity_anchor,
        "failed audit compaction must leave previous head active"
    );
}

/// Issue #9 invariant 14: projection_version belongs in the compaction head identity so a summary
/// created under one visibility/projection contract is never reused after that contract changes.
#[tokio::test]
async fn projection_version_isolates_agent_and_audit_compaction_heads() {
    let store = Arc::new(build_isolated_store().await);
    let ctx = ContextId::new(9813, 1);
    let a = agent_id("00000000-0000-0000-0000-0000000000aa");
    let task_a = TaskId::from_external(ExternalId::new("ver-task-a"));
    boot(&store, &a, "clickup-agent").await;
    ensure_task(&store, &ctx, &task_a, &a).await;

    let count = DEFAULT_LLM_CONTEXT_ITEM_CAP + 5;
    seed_messages(
        &store,
        &ctx,
        &task_a,
        &a,
        "agent-a",
        count,
        1_900_400_000_000,
    )
    .await;

    let producer = audit_producer(Arc::clone(&store), Arc::new(SourceEchoSummarizer));
    let sub = subscriber(Arc::clone(&store));

    let v1 = CURRENT_COMPACTION_PROJECTION_VERSION;
    let v2 = CompactionProjectionVersion::new(v1.as_u32() + 1);

    // Produce v1 heads for both agent and audit.
    sub.evaluate_and_compact(
        &AgentCompactionRequest::new(ctx.clone(), a.clone()).with_projection_version(v1),
        force_trigger(),
        "default",
    )
    .await;
    assert!(matches!(
        producer
            .compact_audit(
                &AuditCompactionRequest::new(ctx.clone()).with_projection_version(v1),
                true,
                false,
            )
            .await,
        AuditCompactionOutcome::Succeeded { .. }
    ));

    // A v2 reader must NOT reuse v1 heads.
    assert!(
        store
            .latest_agent_compaction_head(&ctx, &a, None, v2)
            .await
            .expect("v2 agent lookup")
            .is_none(),
        "v2 agent lookup must not reuse v1 head"
    );
    assert!(
        store
            .latest_audit_compaction_head(&ctx, None, v2)
            .await
            .expect("v2 audit lookup")
            .is_none(),
        "v2 audit lookup must not reuse v1 head"
    );

    // Writing v2 heads must not overwrite v1 heads (both coexist cleanly).
    sub.evaluate_and_compact(
        &AgentCompactionRequest::new(ctx.clone(), a.clone()).with_projection_version(v2),
        force_trigger(),
        "default",
    )
    .await;
    assert!(matches!(
        producer
            .compact_audit(
                &AuditCompactionRequest::new(ctx.clone()).with_projection_version(v2),
                true,
                false,
            )
            .await,
        AuditCompactionOutcome::Succeeded { .. }
    ));

    let agent_v1 = store
        .latest_agent_compaction_head(&ctx, &a, None, v1)
        .await
        .expect("v1 agent")
        .expect("v1 agent head");
    let agent_v2 = store
        .latest_agent_compaction_head(&ctx, &a, None, v2)
        .await
        .expect("v2 agent")
        .expect("v2 agent head");
    let audit_v1 = store
        .latest_audit_compaction_head(&ctx, None, v1)
        .await
        .expect("v1 audit")
        .expect("v1 audit head");
    let audit_v2 = store
        .latest_audit_compaction_head(&ctx, None, v2)
        .await
        .expect("v2 audit")
        .expect("v2 audit head");

    assert_eq!(agent_v1.projection_version, v1);
    assert_eq!(agent_v2.projection_version, v2);
    assert_ne!(agent_v1.activity_anchor, agent_v2.activity_anchor);
    assert_eq!(audit_v1.projection_version, v1);
    assert_eq!(audit_v2.projection_version, v2);
    assert_ne!(audit_v1.activity_anchor, audit_v2.activity_anchor);
}
