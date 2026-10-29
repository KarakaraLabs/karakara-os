// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! [`ProvenanceWriter`] implementation — normalized event write path.

use async_trait::async_trait;
use serde_json::Value;

use super::{SurrealProvenanceStore, payload::payload_records_from_event};
use crate::{
    context_agent_leases::ContextAgentFence,
    error::{ProvenanceError, Result},
    events::{ProvEvent, ProvEventData},
    id_semantics::agent_node_id_string,
    normalizer::{NormalizeContext, validate_event},
    payload_record::StorageKind,
    payload_storage,
    store::{ProvenanceWriter, TaskAgentResolution},
    surreal_write_batch::call_activity_id_from_normalized,
    task_agent_binding::event_local_executing_agent_id,
};

fn transcript_agent_node_id(event: &ProvEvent, context: &NormalizeContext) -> Option<String> {
    let event_agent = match event.data() {
        ProvEventData::HostDispatchAccepted {
            target_agent_id, ..
        }
        | ProvEventData::HostDispatchRejected {
            target_agent_id, ..
        } => target_agent_id.clone(),
        _ => event_local_executing_agent_id(event),
    };
    event_agent
        .or_else(|| context.task_agent_id.clone())
        .map(|agent_id| agent_node_id_string(&agent_id))
}

impl SurrealProvenanceStore {
    async fn write_event(
        &self,
        event: crate::events::ProvEvent,
        fence: Option<&ContextAgentFence>,
    ) -> Result<()> {
        validate_event(&event)?;
        self.enforce_message_activity_anchor_invariant(&event)
            .await?;
        self.enforce_step_completion_gate(&event).await?;
        // Registration is deliberately before graph normalization. A metadata
        // mismatch must fail without an UPSERT mutating either immutable node.
        if let ProvEventData::AgentBooted {
            agent_id,
            agent_type,
            agent_version,
            identity_digest,
            artifact_hash,
            deployment_content_hash,
            ..
        } = event.data()
        {
            self.upsert_agent_package_registry_on_boot(
                agent_id,
                agent_type.as_str(),
                identity_digest,
            )
            .await?;
            self.register_agent_artifact(artifact_hash, deployment_content_hash, agent_version)
                .await?;
        }
        let mut payload_records = payload_records_from_event(&event)?;
        let mut context = match event.task_id() {
            Some(tid) => {
                let resolution = self.get_task_agent_id(tid).await?;
                if matches!(resolution, TaskAgentResolution::TimedOut) {
                    tracing::warn!(
                        task_id = tid.as_str(),
                        event_id = event.id().as_str(),
                        "agent-scoped normalization skipped: get_task_agent_id timed out"
                    );
                }
                let mut task_agent_id = resolution.for_normalization();
                if task_agent_id.is_none() {
                    task_agent_id = event_local_executing_agent_id(&event);
                }
                let task_boot_event_id = match event.data() {
                    ProvEventData::TaskExecutionStarted { boot_event_id, .. } => {
                        boot_event_id.clone()
                    }
                    _ => None,
                };
                NormalizeContext {
                    task_agent_id,
                    linked_llm_call_scope_ordinal: None,
                    task_boot_event_id,
                }
            }
            None => NormalizeContext::default(),
        };
        if let crate::events::ProvEventData::PromptRejected {
            llm_call_activity_anchor,
            ..
        } = event.data()
            && let Some(linked) = self
                .resolve_llm_call_scope_ordinal_by_event_anchor(llm_call_activity_anchor.as_str())
                .await?
        {
            context.linked_llm_call_scope_ordinal = Some(linked);
        }
        let transcript_agent_node_id = transcript_agent_node_id(&event, &context);
        let normalized = self
            .normalizer
            .normalize_with_context(&event, Some(&context))?;
        let context_id_opt = event.context_id_opt().map(|c| c.as_str().to_string());
        let anchor = event.id().as_str().to_string();
        let activity_id = call_activity_id_from_normalized(&normalized, &anchor);

        let mut blob_bodies: Vec<(String, String)> = Vec::new();
        let mut inline_payload_bytes: usize = 0;
        for p in &mut payload_records {
            if let Some(ref a) = activity_id {
                p.activity_id = Some(a.clone());
            }
            if payload_storage::should_offload_payload(&p.payload_kind, p.payload_json.len()) {
                let v: Value = serde_json::from_str(&p.payload_json).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("payload json for offload: {e}"),
                    }
                })?;
                let canon = payload_storage::canonical_json_string(&v).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("canonical json for offload: {e}"),
                    }
                })?;
                let hash = payload_storage::sha256_hex_utf8(&canon);
                p.search_text = payload_storage::search_text_snippet(&canon);
                p.content_hash = Some(hash.clone());
                p.storage_kind = StorageKind::Blob;
                p.file_key = Some(payload_storage::logical_file_key_for_tool_archive(&hash));
                p.payload_json.clear();
                blob_bodies.push((hash, canon));
            } else {
                inline_payload_bytes = inline_payload_bytes.saturating_add(p.payload_json.len());
                p.search_text = payload_storage::search_text_snippet(&p.payload_json);
            }
        }

        let plans = crate::surreal_write_batch::build_event_write_plans(
            &normalized,
            context_id_opt.as_deref(),
            transcript_agent_node_id.as_deref(),
            &payload_records,
            &blob_bodies,
            &self.write_coalesce,
        );
        let total_stmts: usize = plans.iter().map(|p| p.statement_count).sum();
        let total_binds: usize = plans.iter().map(|p| p.binds.len()).sum();
        tracing::debug!(
            target: "baml_rt_provenance::surreal",
            anchor = %anchor,
            txn_parts = plans.len(),
            statements = total_stmts,
            bind_count = total_binds,
            payload_rows = payload_records.len(),
            blob_rows = blob_bodies.len(),
            inline_payload_bytes,
            "provenance add_event write txn"
        );
        for plan in plans {
            let plan = match fence {
                Some(fence) => plan.with_fence_guard(fence),
                None => plan,
            };
            if plan.shared {
                self.run_shared_event_write(context_id_opt.as_deref(), plan)
                    .await?;
            } else {
                self.run_event_write_plan(plan).await?;
            }
        }
        if let Some(fence) = fence {
            self.ensure_context_agent_fence(fence).await?;
        }

        self.update_context_picker_index(&event).await?;
        self.update_context_planning_index(&event).await?;
        self.upsert_context_compaction_index(&event).await?;

        if let (Some(cache), Some(ctx)) = (&self.mermaid_cache, context_id_opt.as_deref()) {
            cache.invalidate(ctx);
        }
        if let Some(ctx) = context_id_opt.as_deref()
            && let Ok(guard) = self.ref_table_cache.read()
            && let Some(tables) = guard.as_ref()
        {
            // Narrow invalidation to the agents this write touches. A sibling agent's ref table
            // (including its in-memory pending receipts) survives an unrelated write.
            let mut affected_agents: Vec<String> = Vec::new();
            if let Some(agent) = context.task_agent_id.as_ref() {
                affected_agents.push(agent.as_str().to_string());
            }
            if let Some(agent) = event.message_agent_id() {
                affected_agents.push(agent.as_str().to_string());
            }
            affected_agents.sort();
            affected_agents.dedup();
            if affected_agents.is_empty() {
                baml_rt_tools::archive_refs::invalidate_ref_tables_for_context(tables, ctx);
            } else {
                for agent in &affected_agents {
                    baml_rt_tools::archive_refs::invalidate_ref_table(tables, ctx, agent);
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ProvenanceWriter for SurrealProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        self.write_event(event, None).await
    }

    async fn add_event_fenced(
        &self,
        event: crate::events::ProvEvent,
        fence: ContextAgentFence,
    ) -> Result<()> {
        self.write_event(event, Some(&fence)).await
    }
}
