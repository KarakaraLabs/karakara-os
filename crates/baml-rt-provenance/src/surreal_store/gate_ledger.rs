// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use baml_rt_core::ids::{AgentId, ContextId, TaskId, UuidId};
use baml_rt_id::ExternalId;
use serde_json::Value;

use super::{SurrealProvenanceStore, check_and_take_zero, map_surreal_error};
use crate::{
    error::Result,
    events::{GateDecisionOutcome, GatePrincipal},
    id_semantics::context_entity_id_string,
    store::{GateDecisionWrite, GateFlowOrigin, GateLedgerDecision, GateLedgerRequest},
    surreal_tables::{TBL_EDGE, TBL_GATE_CONFIRMATION_CLAIM, TBL_GATE_DECISION, TBL_NODE},
    vocabulary::context_scope,
};

fn parse_gate_request(props: &Value, gate_id: &str) -> Result<GateLedgerRequest> {
    let task_id = props
        .get("a2a_task_id")
        .and_then(Value::as_str)
        .map(|value| TaskId::from_external(ExternalId::new(value)))
        .ok_or_else(|| crate::error::ProvenanceError::InvalidEvent {
            activity_anchor: gate_id.to_string(),
            reason: "gate request is missing task_id".to_string(),
        })?;
    let agent_id = props
        .get("a2a_agent_id")
        .and_then(Value::as_str)
        .and_then(|value| UuidId::parse_str(value).ok())
        .map(AgentId::from_uuid)
        .ok_or_else(|| crate::error::ProvenanceError::InvalidEvent {
            activity_anchor: gate_id.to_string(),
            reason: "gate request is missing agent_id".to_string(),
        })?;
    let expires_at_ms = props
        .get("gate_expires_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| crate::error::ProvenanceError::InvalidEvent {
            activity_anchor: gate_id.to_string(),
            reason: "gate request is missing expires_at_ms".to_string(),
        })?;
    Ok(GateLedgerRequest {
        task_id,
        agent_id,
        expires_at_ms,
        channel: props
            .get("gate_channel")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn outcome_label(outcome: GateDecisionOutcome) -> &'static str {
    match outcome {
        GateDecisionOutcome::Allow => "allow",
        GateDecisionOutcome::Deny => "deny",
    }
}

fn parse_gate_decision(row: &Value, gate_id: &str) -> Result<GateLedgerDecision> {
    let invalid = |reason: String| crate::error::ProvenanceError::InvalidEvent {
        activity_anchor: gate_id.to_string(),
        reason,
    };
    let outcome = match row.get("outcome").and_then(Value::as_str) {
        Some("allow") => GateDecisionOutcome::Allow,
        Some("deny") => GateDecisionOutcome::Deny,
        Some(value) => return Err(invalid(format!("unknown gate outcome: {value}"))),
        None => return Err(invalid("gate decision is missing outcome".to_string())),
    };
    let principal_json = row
        .get("principal_json")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("gate decision is missing principal".to_string()))?;
    let principal = serde_json::from_str::<GatePrincipal>(principal_json)
        .map_err(|error| invalid(format!("invalid gate principal: {error}")))?;
    let reason = row
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(GateLedgerDecision {
        outcome,
        principal,
        reason,
    })
}

impl SurrealProvenanceStore {
    /// Create-only decision record keyed by `gate_id`. `Ok(None)` means this caller's
    /// decision now stands; `Ok(Some(existing))` is the decision that won earlier.
    async fn create_gate_decision_record(
        &self,
        context_id: &ContextId,
        gate_id: &str,
        outcome: GateDecisionOutcome,
        principal: &GatePrincipal,
        reason: &str,
    ) -> Result<Option<GateLedgerDecision>> {
        let principal_json = serde_json::to_string(principal).map_err(|error| {
            crate::error::ProvenanceError::InvalidEvent {
                activity_anchor: gate_id.to_string(),
                reason: format!("could not encode gate principal: {error}"),
            }
        })?;
        let sql = format!(
            "CREATE type::record('{TBL_GATE_DECISION}', $gate_id) CONTENT {{ \
               gate_id: $gate_id, context_id: $context_id, outcome: $outcome, \
               principal_json: $principal_json, reason: $reason \
             }} RETURN NONE;"
        );
        for attempt in 0..super::WRITE_CONFLICT_MAX_ATTEMPTS {
            let outcome_result = match self
                .db()
                .query(&sql)
                .bind(("gate_id", gate_id.to_string()))
                .bind(("context_id", context_id.as_str().to_string()))
                .bind(("outcome", outcome_label(outcome)))
                .bind(("principal_json", principal_json.clone()))
                .bind(("reason", reason.to_string()))
                .await
            {
                Ok(response) => check_and_take_zero(response, std::convert::identity).map(|_| ()),
                Err(error) => Err(error),
            };
            match outcome_result {
                Ok(()) => return Ok(None),
                Err(error) if super::is_duplicate_record_write(&error) => {
                    return self
                        .read_gate_ledger_decision(context_id, gate_id)
                        .await?
                        .map(Some)
                        .ok_or_else(|| crate::error::ProvenanceError::InvalidEvent {
                            activity_anchor: gate_id.to_string(),
                            reason: "gate decision exists in another context".to_string(),
                        });
                }
                Err(error) if super::is_transaction_conflict(&error) => {
                    tokio::time::sleep(super::jittered_backoff(attempt)).await;
                }
                Err(error) => return Err(map_surreal_error(error)),
            }
        }
        Err(crate::error::ProvenanceError::Contention {
            details: format!("gate decision for {gate_id} kept conflicting"),
        })
    }

    pub(super) async fn claim_gate_decision_record(
        &self,
        context_id: &ContextId,
        decision: &GateDecisionWrite,
    ) -> Result<Option<GateLedgerDecision>> {
        self.create_gate_decision_record(
            context_id,
            &decision.gate_id,
            decision.outcome,
            &decision.principal,
            &decision.reason,
        )
        .await
    }

    pub(super) async fn read_gate_flow_origin(
        &self,
        context_id: &ContextId,
    ) -> Result<GateFlowOrigin> {
        let sql = format!(
            "SELECT props FROM {TBL_NODE} \
             WHERE node_id IN (SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $context_node AND rel_type = $scoped_to AND from_label = 'Message') \
               AND label = 'Message' \
               AND (props.a2a_host_ingress_kind = 'source_poll_recorded' \
                 OR props.a2a_user_speaker_kind = 'human')"
        );
        let response = self
            .db()
            .query(crate::surreal_hoist::hoist_in_subqueries(&sql))
            .bind((
                "context_node",
                context_entity_id_string(context_id.as_str()),
            ))
            .bind(("scoped_to", context_scope::SCOPED_TO))
            .await
            .map_err(map_surreal_error)?;
        let rows = check_and_take_zero(response, map_surreal_error)?;
        let mut machine_ingress = None;
        let mut has_live_chat = false;
        for props in rows.iter().filter_map(|row| row.get("props")) {
            if props.get("a2a_user_speaker_kind").and_then(Value::as_str) == Some("human") {
                has_live_chat = true;
            }
            if let Some(channel) = props
                .get("a2a_host_ingress_channel")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                return Ok(GateFlowOrigin::Channel {
                    channel: channel.to_string(),
                });
            }
            if machine_ingress.is_none()
                && props.get("a2a_host_ingress_kind").and_then(Value::as_str)
                    == Some("source_poll_recorded")
            {
                let kind = props
                    .get("a2a_host_ingress_source_kind")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let key = props
                    .get("a2a_host_ingress_source_key")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                machine_ingress = match (kind, key) {
                    (Some(kind), Some(key)) => Some(format!("{kind}:{key}")),
                    (Some(kind), None) => Some(kind.to_string()),
                    _ => None,
                };
            }
        }
        // Each ingress event opens its own context, so a context carries one
        // ingress kind. A later A2A chat message in a machine-ingress context
        // joins that flow; connecting the chat is itself the trust decision.
        if let Some(ingress_key) = machine_ingress {
            return Ok(GateFlowOrigin::MachineIngress { ingress_key });
        }
        Ok(if has_live_chat {
            GateFlowOrigin::LiveChat
        } else {
            GateFlowOrigin::None
        })
    }

    /// Create-only claim on `gate_id`. `Ok(true)` means this caller created the
    /// claim and must post the confirmation; `Ok(false)` means another caller did.
    pub(super) async fn create_gate_confirmation_claim(&self, gate_id: &str) -> Result<bool> {
        let sql = format!(
            "CREATE type::record('{TBL_GATE_CONFIRMATION_CLAIM}', $gate_id) \
             CONTENT {{ gate_id: $gate_id }} RETURN NONE;"
        );
        for attempt in 0..super::WRITE_CONFLICT_MAX_ATTEMPTS {
            let outcome = match self
                .db()
                .query(&sql)
                .bind(("gate_id", gate_id.to_string()))
                .await
            {
                Ok(response) => check_and_take_zero(response, std::convert::identity).map(|_| ()),
                Err(error) => Err(error),
            };
            match outcome {
                Ok(()) => return Ok(true),
                Err(error) if super::is_duplicate_record_write(&error) => return Ok(false),
                // A concurrent create of the same record surfaces as a conflict;
                // retry so the loser observes the duplicate instead.
                Err(error) if super::is_transaction_conflict(&error) => {
                    tokio::time::sleep(super::jittered_backoff(attempt)).await;
                }
                Err(error) => return Err(map_surreal_error(error)),
            }
        }
        Err(crate::error::ProvenanceError::Contention {
            details: format!("gate confirmation claim for {gate_id} kept conflicting"),
        })
    }

    pub async fn expire_pending_gate_requests(&self, now_ms: u64) -> Result<usize> {
        // A row whose expiry is missing compares below any number, so it is picked up
        // here and quarantined below instead of being skipped forever.
        let sql = format!(
            "SELECT props, props.gate_expires_at_ms AS gate_expiry FROM {TBL_NODE} \
             WHERE label = 'AuditDecision' \
               AND props.audit_decision_kind = 'gate_requested' \
               AND (props.gate_expires_at_ms = NONE OR props.gate_expires_at_ms <= $now_ms) \
               AND props.gate_id NOT IN (SELECT VALUE gate_id FROM {TBL_GATE_DECISION}) \
             ORDER BY gate_expiry ASC LIMIT 100"
        );
        let response = self
            .db()
            .query(crate::surreal_hoist::hoist_in_subqueries(&sql))
            .bind(("now_ms", now_ms))
            .await
            .map_err(map_surreal_error)?;
        let rows = check_and_take_zero(response, map_surreal_error)?;
        let mut expired = 0;
        for props in rows.iter().filter_map(|row| row.get("props")) {
            let (Some(context_id), Some(gate_id)) = (
                props.get("a2a_context_id").and_then(Value::as_str),
                props.get("gate_id").and_then(Value::as_str),
            ) else {
                tracing::warn!("gate request without context or gate id cannot be expired");
                continue;
            };
            let context_id = ContextId::from(context_id);
            let request = match parse_gate_request(props, gate_id) {
                Ok(request) => request,
                Err(error) => {
                    // Close the malformed gate with a Deny so it stops taking sweep slots
                    // and any reader of this gate sees a decision instead of retrying.
                    tracing::warn!(gate_id, error = %error, "quarantining malformed gate request");
                    self.create_gate_decision_record(
                        &context_id,
                        gate_id,
                        GateDecisionOutcome::Deny,
                        &GatePrincipal::Runtime {
                            reason: "malformed_request".to_string(),
                        },
                        &format!("malformed gate request: {error}"),
                    )
                    .await?;
                    continue;
                }
            };
            let decision = GateDecisionWrite {
                gate_id: gate_id.to_string(),
                task_id: request.task_id,
                agent_id: request.agent_id,
                outcome: GateDecisionOutcome::Deny,
                principal: GatePrincipal::Runtime {
                    reason: "expiry".to_string(),
                },
                reason: "gate confirmation expired".to_string(),
            };
            let stands =
                crate::store::ProvenanceWriter::decide_gate(self, &context_id, decision.clone())
                    .await?;
            if stands.principal == decision.principal && stands.outcome == decision.outcome {
                expired += 1;
            }
        }
        Ok(expired)
    }

    pub(super) async fn read_gate_ledger_request(
        &self,
        context_id: &ContextId,
        gate_id: &str,
    ) -> Result<Option<GateLedgerRequest>> {
        let Some(props) = self
            .read_gate_audit_props(context_id, gate_id, "gate_requested")
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(parse_gate_request(&props, gate_id)?))
    }

    pub(super) async fn read_gate_ledger_decision(
        &self,
        context_id: &ContextId,
        gate_id: &str,
    ) -> Result<Option<GateLedgerDecision>> {
        let sql = format!(
            "SELECT * FROM type::record('{TBL_GATE_DECISION}', $gate_id) \
             WHERE context_id = $context_id"
        );
        let response = self
            .db()
            .query(&sql)
            .bind(("gate_id", gate_id.to_string()))
            .bind(("context_id", context_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows = check_and_take_zero(response, map_surreal_error)?;
        rows.first()
            .map(|row| parse_gate_decision(row, gate_id))
            .transpose()
    }

    async fn read_gate_audit_props(
        &self,
        context_id: &ContextId,
        gate_id: &str,
        kind: &str,
    ) -> Result<Option<Value>> {
        let sql = format!(
            "SELECT props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE label = 'AuditDecision' \
               AND props.a2a_context_id = $context_id \
               AND props.audit_decision_kind = $kind \
               AND props.gate_id = $gate_id \
             ORDER BY event_order DESC LIMIT 1"
        );
        let response = self
            .db()
            .query(crate::surreal_hoist::hoist_in_subqueries(&sql))
            .bind(("context_id", context_id.as_str().to_string()))
            .bind(("gate_id", gate_id.to_string()))
            .bind(("kind", kind.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows = check_and_take_zero(response, map_surreal_error)?;
        Ok(rows.first().and_then(|row| row.get("props")).cloned())
    }
}
