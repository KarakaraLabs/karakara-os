// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Prepare compaction inputs from store reads (pure orchestration split from subscriber).

use std::sync::Arc;

use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::ids::{AgentId, ContextId};
use baml_rt_llm_config::CompactionTriggerPolicy;
use baml_rt_tools::tools::ToolRegistry;
use sha2::{Digest, Sha256};

use super::{
    digest::format_planning_digest,
    partition::partition_items_for_compaction,
    range::select_compactable_range,
    render::prepare_render_context,
    types::{
        AgentCompactionRequest, AuditCompactionRequest, CompactionPrefixInput,
        CompactionProjectionVersion, CompactionSummaryAudience, ContextCompactionRecord,
        ContextCompactionTrigger,
    },
};
use crate::{
    ProvenanceReadPrincipal, error::ProvenanceError, surreal_store::SurrealProvenanceStore,
};

/// Inputs ready for summarization and provenance record emission.
pub struct PreparedCompaction {
    pub input: CompactionPrefixInput,
    pub record: ContextCompactionRecord,
    pub source_rendered: String,
    pub pre_prompt_bytes: u64,
    pub covered_rows: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionPrepareError {
    #[error("store/projection read failed: {0}")]
    Store(#[from] ProvenanceError),
}

struct CompactionPrepareTarget<'a> {
    context_id: &'a ContextId,
    principal: &'a ProvenanceReadPrincipal,
    record_agent_id: Option<AgentId>,
    projection_version: CompactionProjectionVersion,
    audience: CompactionSummaryAudience,
    trigger: ContextCompactionTrigger,
}

/// Load an agent's participation view, select range, partition items, and render prefix.
///
/// Always reads with [`ProvenanceReadPrincipal::Agent`] and produces a record owned by
/// `Some(request.agent_id)`. Cannot populate an operator/audit head.
pub async fn prepare_compaction(
    store: &SurrealProvenanceStore,
    tool_registry: &ToolRegistry,
    policy: &CompactionTriggerPolicy,
    request: &AgentCompactionRequest,
    trigger: ContextCompactionTrigger,
) -> Result<Option<PreparedCompaction>, CompactionPrepareError> {
    // Agent-prompt compaction is derived from exactly this agent's own view:
    // sibling rows must not change the covered prefix or the rendered summary.
    let items = store
        .conversation_context_for_agent(&request.context_id, &request.agent_id, None)
        .await?;
    let principal = ProvenanceReadPrincipal::Agent(request.agent_id.clone());
    prepare_compaction_from_items(
        store,
        tool_registry,
        policy,
        CompactionPrepareTarget {
            context_id: &request.context_id,
            principal: &principal,
            record_agent_id: Some(request.agent_id.clone()),
            projection_version: request.projection_version,
            audience: CompactionSummaryAudience::AgentSelf,
            trigger,
        },
        items,
    )
    .await
}

/// Load the complete operator timeline, hydrate refs with [`ProvenanceReadPrincipal::Operator`],
/// select range, partition items, and render an operator/audit compaction prefix.
///
/// Always produces a record owned by the operator slot (`agent_id = None`). Cannot populate
/// an agent-owned compaction head.
pub async fn prepare_audit_compaction(
    store: &SurrealProvenanceStore,
    tool_registry: &ToolRegistry,
    policy: &CompactionTriggerPolicy,
    request: &AuditCompactionRequest,
    trigger: ContextCompactionTrigger,
) -> Result<Option<PreparedCompaction>, CompactionPrepareError> {
    let items = store
        .conversation_context_for_operator(&request.context_id, None)
        .await?;
    prepare_audit_compaction_from_items(store, tool_registry, policy, request, items, trigger).await
}

/// Prepare operator/audit compaction from an already-loaded operator timeline snapshot so
/// trigger evaluation and prefix rendering share one consistent store read.
pub(crate) async fn prepare_audit_compaction_from_items(
    store: &SurrealProvenanceStore,
    tool_registry: &ToolRegistry,
    policy: &CompactionTriggerPolicy,
    request: &AuditCompactionRequest,
    items: Vec<ProvenanceConversationContextItem>,
    trigger: ContextCompactionTrigger,
) -> Result<Option<PreparedCompaction>, CompactionPrepareError> {
    prepare_compaction_from_items(
        store,
        tool_registry,
        policy,
        CompactionPrepareTarget {
            context_id: &request.context_id,
            principal: &ProvenanceReadPrincipal::Operator,
            record_agent_id: None,
            projection_version: request.projection_version,
            audience: CompactionSummaryAudience::OperatorAudit,
            trigger,
        },
        items,
    )
    .await
}

async fn prepare_compaction_from_items(
    store: &SurrealProvenanceStore,
    tool_registry: &ToolRegistry,
    policy: &CompactionTriggerPolicy,
    target: CompactionPrepareTarget<'_>,
    items: Vec<ProvenanceConversationContextItem>,
) -> Result<Option<PreparedCompaction>, CompactionPrepareError> {
    let Some(range) = select_compactable_range(&items, policy) else {
        return Ok(None);
    };
    let pre_row_count = items.len() as u64;
    let (prefix_items, tail_items) = partition_items_for_compaction(&items, &range);
    if prefix_items.is_empty() {
        return Ok(None);
    }

    let Some(render_context) = prepare_render_context(
        store,
        target.context_id,
        target.principal,
        &items,
        tool_registry,
    )
    .await?
    else {
        return Ok(None);
    };
    let source_rendered = render_context.render_items(&prefix_items, tool_registry);
    if source_rendered.trim().is_empty() {
        return Ok(None);
    }
    let pre_prompt_bytes = source_rendered.len() as u64;

    let tail_preview = if tail_items.is_empty() {
        None
    } else {
        Some(render_context.render_items(&tail_items, tool_registry))
            .filter(|s| !s.trim().is_empty())
    };

    let input = CompactionPrefixInput {
        source_rendered: source_rendered.clone(),
        active_planning_digest: format_planning_digest(&prefix_items),
        recent_tail_preview: tail_preview,
        ref_table: Arc::clone(&render_context.ref_table),
        audience: target.audience,
    };

    let mut hasher = Sha256::new();
    hasher.update(source_rendered.as_bytes());
    let source_render_hash = format!("{:x}", hasher.finalize());
    let tail_count = pre_row_count.saturating_sub(prefix_items.len() as u64);
    let covered_node_ids: Vec<_> = prefix_items
        .iter()
        .map(|item| item.activity_anchor.as_str().to_string())
        .collect();

    let record = ContextCompactionRecord {
        context_id: target.context_id.clone(),
        agent_id: target.record_agent_id,
        task_id: None,
        projection_version: target.projection_version,
        covered_event_order_start: range.covered_event_order_start,
        covered_event_order_end: range.covered_event_order_end,
        covered_node_ids,
        summary_text: String::new(),
        trigger: target.trigger,
        recent_tail_retention: policy.recent_tail_retention,
        pre_row_count,
        post_row_count: tail_count + 1,
        pre_prompt_bytes,
        post_prompt_bytes: 0,
        source_render_hash,
        excluded_unresolved: false,
    };

    Ok(Some(PreparedCompaction {
        input,
        record,
        source_rendered,
        pre_prompt_bytes,
        covered_rows: prefix_items.len() as u64,
    }))
}
