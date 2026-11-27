// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Types for host-owned context/history compaction provenance events.

use std::sync::Arc;

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, TaskId};
use baml_rt_tools::archive_refs::RefTable;
use serde::{Deserialize, Serialize};

pub use crate::events::ContextCompactionTrigger;

/// Typed projection/visibility contract version embedded in compaction identity.
///
/// Increment [`Self::CURRENT`] whenever visibility or projection semantics change so summaries
/// produced under an older contract are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompactionProjectionVersion(pub u32);

impl CompactionProjectionVersion {
    pub const CURRENT: Self = Self(1);

    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl Default for CompactionProjectionVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl std::fmt::Display for CompactionProjectionVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Current projection/visibility contract version constant.
pub const CURRENT_COMPACTION_PROJECTION_VERSION: CompactionProjectionVersion =
    CompactionProjectionVersion::CURRENT;

/// Target reader contract for a compaction summary, used to frame LLM summarization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionSummaryAudience {
    /// Single-agent continuation summary for that agent's future prompt turns.
    #[default]
    AgentSelf,
    /// Third-person, actor-attributed audit summary across all agents in the context.
    OperatorAudit,
}

/// Identity for an agent-prompt compaction attempt (routing + provenance scope).
///
/// Always carries a concrete [`AgentId`]; cannot represent an operator/audit request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCompactionRequest {
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub projection_version: CompactionProjectionVersion,
}

impl AgentCompactionRequest {
    #[must_use]
    pub fn new(context_id: ContextId, agent_id: AgentId) -> Self {
        Self {
            context_id,
            agent_id,
            projection_version: CompactionProjectionVersion::CURRENT,
        }
    }

    #[must_use]
    pub fn with_projection_version(
        mut self,
        projection_version: CompactionProjectionVersion,
    ) -> Self {
        self.projection_version = projection_version;
        self
    }
}

/// Backwards-compatible alias for [`AgentCompactionRequest`].
pub type CompactionRequest = AgentCompactionRequest;

/// Explicit identity for an operator/audit compaction attempt over the complete context.
///
/// Distinct from [`AgentCompactionRequest`]: carries no `agent_id`, always reads the
/// complete context with [`crate::ProvenanceReadPrincipal::Operator`], and always persists
/// under the operator head slot (`agent_id = None`). Must never be called from agent
/// prompt execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCompactionRequest {
    pub context_id: ContextId,
    pub projection_version: CompactionProjectionVersion,
}

impl AuditCompactionRequest {
    #[must_use]
    pub fn new(context_id: ContextId) -> Self {
        Self {
            context_id,
            projection_version: CompactionProjectionVersion::CURRENT,
        }
    }

    #[must_use]
    pub fn with_projection_version(
        mut self,
        projection_version: CompactionProjectionVersion,
    ) -> Self {
        self.projection_version = projection_version;
        self
    }
}

/// Prepared prefix handed to any summarizer backend.
#[derive(Debug, Clone)]
pub struct CompactionPrefixInput {
    /// Rendered transcript of the sealed prefix (LLM input).
    pub source_rendered: String,
    pub active_planning_digest: Option<String>,
    pub recent_tail_preview: Option<String>,
    /// Hydrated ref table for validating wire refs cited in the summary.
    pub ref_table: Arc<RefTable>,
    /// Target audience framing (`AgentSelf` vs `OperatorAudit`).
    pub audience: CompactionSummaryAudience,
}

/// Latest compaction head for a context (optionally task-scoped).
///
/// `agent_id` partitions the two compaction contracts: `Some(agent)` is an
/// agent-prompt summary derived from exactly that agent's view; `None` is an
/// operator/audit summary derived from the complete context and must never
/// satisfy an agent-visible read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCompactionHead {
    pub activity_anchor: ActivityAnchorId,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub summary_text: String,
    pub trigger: ContextCompactionTrigger,
    pub event_order: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_entity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    pub projection_version: CompactionProjectionVersion,
}

/// Inputs collected before writing a compaction provenance event.
#[derive(Debug, Clone)]
pub struct ContextCompactionRecord {
    pub context_id: ContextId,
    /// `Some(agent)` for agent-prompt compaction; `None` for operator/audit compaction.
    pub agent_id: Option<AgentId>,
    pub task_id: Option<TaskId>,
    pub projection_version: CompactionProjectionVersion,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub covered_node_ids: Vec<String>,
    pub summary_text: String,
    pub trigger: ContextCompactionTrigger,
    pub recent_tail_retention: usize,
    pub pre_row_count: u64,
    pub post_row_count: u64,
    pub pre_prompt_bytes: u64,
    pub post_prompt_bytes: u64,
    pub source_render_hash: String,
    pub excluded_unresolved: bool,
}
