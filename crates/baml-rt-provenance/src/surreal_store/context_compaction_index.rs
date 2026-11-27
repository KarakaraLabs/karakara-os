// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Write-maintained compaction head per context (optionally task-scoped).

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, TaskId};
use serde_json::Value;

use super::{
    SurrealProvenanceStore, WRITE_CONFLICT_MAX_ATTEMPTS,
    helpers::{check_and_take_zero, map_surreal_error},
    is_transaction_conflict, jittered_backoff,
};
use crate::{
    context_compaction::types::{
        CURRENT_COMPACTION_PROJECTION_VERSION, CompactionProjectionVersion, ContextCompactionHead,
        ContextCompactionTrigger,
    },
    error::{ProvenanceError, Result},
    events::{ProvEvent, ProvEventData},
    surreal_tables::TBL_CONTEXT_COMPACTION_INDEX,
};

fn compaction_head_key(
    context_id: &str,
    agent_id: Option<&str>,
    task_entity_id: Option<&str>,
    projection_version: CompactionProjectionVersion,
) -> String {
    let principal = match agent_id {
        Some(agent) => format!("agent:{agent}"),
        None => "audit".to_string(),
    };
    let task = task_entity_id.unwrap_or("context");
    format!(
        "{context_id}\u{1f}v{}\u{1f}{principal}\u{1f}{task}",
        projection_version.as_u32()
    )
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ContextCompactionIndexRow {
    pub head_key: String,
    pub context_id: String,
    /// `Some(agent_id)` for an agent-prompt summary; `None` for an operator/audit summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_entity_id: Option<String>,
    pub projection_version: u32,
    pub activity_anchor: String,
    pub covered_event_order_start: u64,
    pub covered_event_order_end: u64,
    pub summary_text: String,
    pub trigger: String,
    pub event_order: u64,
}

impl SurrealProvenanceStore {
    pub(crate) async fn upsert_context_compaction_index(&self, event: &ProvEvent) -> Result<()> {
        let ProvEventData::ContextCompactionRecorded {
            agent_id,
            task_id,
            projection_version,
            covered_event_order_start,
            covered_event_order_end,
            summary_text,
            trigger,
            ..
        } = event.data()
        else {
            return Ok(());
        };
        let Some(context_id) = event.context_id_opt() else {
            return Ok(());
        };
        let ctx_str = context_id.as_str().to_string();
        let agent_str = agent_id.as_ref().map(|a| a.as_str().to_string());
        let task_str = task_id.as_ref().map(|t| format!("task:{}", t.as_str()));
        let head_key = compaction_head_key(
            &ctx_str,
            agent_str.as_deref(),
            task_str.as_deref(),
            *projection_version,
        );
        let row = ContextCompactionIndexRow {
            head_key,
            context_id: ctx_str.clone(),
            agent_id: agent_str,
            task_entity_id: task_str,
            projection_version: projection_version.as_u32(),
            activity_anchor: event.id().as_str().to_string(),
            covered_event_order_start: *covered_event_order_start,
            covered_event_order_end: *covered_event_order_end,
            summary_text: summary_text.clone(),
            trigger: trigger.as_wire_str().to_string(),
            event_order: event.timestamp_ms(),
        };
        let agent_set = if row.agent_id.is_some() {
            "agent_id = $agent_id"
        } else {
            "agent_id = NONE"
        };
        let task_set = if row.task_entity_id.is_some() {
            "task_entity_id = $task_entity_id"
        } else {
            "task_entity_id = NONE"
        };
        let sql = format!(
            "UPSERT {TBL_CONTEXT_COMPACTION_INDEX} SET \
               head_key = $head_key, \
               context_id = $context_id, \
               {agent_set}, \
               {task_set}, \
               projection_version = $projection_version, \
               activity_anchor = $activity_anchor, \
               covered_event_order_start = $covered_event_order_start, \
               covered_event_order_end = $covered_event_order_end, \
               summary_text = $summary_text, \
               trigger = $trigger, \
               event_order = $event_order \
             WHERE head_key = $head_key"
        );

        let lane = self.shared_write_lanes[self.shared_write_lane(Some(&ctx_str))].clone();
        let _lane_guard = lane.lock().await;
        for attempt in 0..WRITE_CONFLICT_MAX_ATTEMPTS {
            let mut q = self
                .db()
                .query(&sql)
                .bind(("head_key", row.head_key.clone()))
                .bind(("context_id", row.context_id.clone()))
                .bind(("projection_version", row.projection_version))
                .bind(("activity_anchor", row.activity_anchor.clone()))
                .bind(("covered_event_order_start", row.covered_event_order_start))
                .bind(("covered_event_order_end", row.covered_event_order_end))
                .bind(("summary_text", row.summary_text.clone()))
                .bind(("trigger", row.trigger.clone()))
                .bind(("event_order", row.event_order));
            if let Some(agent_id) = &row.agent_id {
                q = q.bind(("agent_id", agent_id.clone()));
            }
            if let Some(task_entity_id) = &row.task_entity_id {
                q = q.bind(("task_entity_id", task_entity_id.clone()));
            }
            let outcome = q.await.and_then(surrealdb::IndexedResults::check);
            match outcome {
                Ok(_) => return Ok(()),
                Err(e) if is_transaction_conflict(&e) => {
                    if attempt + 1 >= WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: e.message().to_string(),
                        });
                    }
                    tokio::time::sleep(jittered_backoff(attempt)).await;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }
        }
        Ok(())
    }

    /// Latest agent-owned compaction head for one concrete agent and projection version.
    ///
    /// Never returns an operator/audit head (`agent_id = None`) or a sibling agent's head.
    pub async fn latest_agent_compaction_head(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        task_id: Option<&TaskId>,
        projection_version: CompactionProjectionVersion,
    ) -> Result<Option<ContextCompactionHead>> {
        self.latest_compaction_head_for_version(
            context_id,
            Some(agent_id),
            task_id,
            projection_version,
        )
        .await
    }

    /// Latest operator/audit compaction head (`agent_id = None`) for one projection version.
    ///
    /// Never returns an agent-owned head (`agent_id = Some(_)`).
    pub async fn latest_audit_compaction_head(
        &self,
        context_id: &ContextId,
        task_id: Option<&TaskId>,
        projection_version: CompactionProjectionVersion,
    ) -> Result<Option<ContextCompactionHead>> {
        self.latest_compaction_head_for_version(context_id, None, task_id, projection_version)
            .await
    }

    /// Latest compaction head for one principal under [`CURRENT_COMPACTION_PROJECTION_VERSION`].
    ///
    /// Pass `Some(agent)` for an agent-prompt summary or `None` for the operator/audit summary.
    /// The two contracts never satisfy each other's query.
    pub async fn latest_compaction_head(
        &self,
        context_id: &ContextId,
        agent_id: Option<&AgentId>,
        task_id: Option<&TaskId>,
    ) -> Result<Option<ContextCompactionHead>> {
        self.latest_compaction_head_for_version(
            context_id,
            agent_id,
            task_id,
            CURRENT_COMPACTION_PROJECTION_VERSION,
        )
        .await
    }

    /// Latest compaction head for one principal and explicit projection contract version.
    pub async fn latest_compaction_head_for_version(
        &self,
        context_id: &ContextId,
        agent_id: Option<&AgentId>,
        task_id: Option<&TaskId>,
        projection_version: CompactionProjectionVersion,
    ) -> Result<Option<ContextCompactionHead>> {
        let task_entity = task_id.map(|t| format!("task:{}", t.as_str()));
        let head_key = compaction_head_key(
            context_id.as_str(),
            agent_id.map(AgentId::as_str),
            task_entity.as_deref(),
            projection_version,
        );
        let query = format!(
            "SELECT * FROM {TBL_CONTEXT_COMPACTION_INDEX} \
             WHERE head_key = $head_key \
             ORDER BY event_order DESC LIMIT 1"
        );
        let response = self
            .db()
            .query(&query)
            .bind(("head_key", head_key))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.into_iter().next().and_then(|row| {
            let stored_version = row
                .get("projection_version")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .map(CompactionProjectionVersion)?;
            let parsed_agent_id = match row.get("agent_id").and_then(Value::as_str) {
                Some(raw) => Some(AgentId::from_uuid(
                    baml_rt_core::ids::UuidId::parse_str(raw).ok()?,
                )),
                None => None,
            };
            Some(ContextCompactionHead {
                activity_anchor: ActivityAnchorId::from(row.get("activity_anchor")?.as_str()?),
                covered_event_order_start: row.get("covered_event_order_start")?.as_u64()?,
                covered_event_order_end: row.get("covered_event_order_end")?.as_u64()?,
                summary_text: row.get("summary_text")?.as_str()?.to_string(),
                trigger: parse_trigger(row.get("trigger")?.as_str()?)?,
                event_order: row.get("event_order")?.as_u64()?,
                task_entity_id: row
                    .get("task_entity_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                agent_id: parsed_agent_id,
                projection_version: stored_version,
            })
        }))
    }
}

fn parse_trigger(raw: &str) -> Option<ContextCompactionTrigger> {
    match raw {
        "post_turn_threshold" => Some(ContextCompactionTrigger::PostTurnThreshold),
        "pre_model_emergency" => Some(ContextCompactionTrigger::PreModelEmergency),
        "manual_operator" => Some(ContextCompactionTrigger::ManualOperator),
        _ => None,
    }
}
