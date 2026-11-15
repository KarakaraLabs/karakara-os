// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Surreal-backed [`HostIngressRecorder`] for publish and dispatch `withTask` preludes.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{
    AgentDispatchRequest, BamlRtError, DispatchTarget, HostIngressRecorder,
    IngressPollUserMessageRef, ProducedEvent, Result,
    context::RuntimeScope,
    dispatch_ingress::{
        DispatchWorkUnit, WithTaskPrelude, dispatch_unit_runtime_scope, format_unit_ingress_body,
    },
    host_source_records_body::IngressPollBody,
    ids::{AgentId, MessageId, UuidId},
};
use baml_rt_vocabulary::vocabulary::user_speaker_kinds;
use uuid::Uuid;

use crate::{
    ProvEvent, ProvenanceWriter, SurrealProvenanceStore,
    events::{GateDecisionOutcome, GatePrincipal, ProvEventData},
    host_ingress_identity::{
        activity_anchor_for_ingress_poll_user, activity_anchor_for_ingress_unit_user,
    },
    host_ingress_types::{HostDispatchFailureKind, HostDispatchRejectedSpec, HostIngressSourceRef},
    task_agent_binding::{TaskAgentBinding, TaskAgentBindingSource},
};

/// Writes host ingress poll/unit user messages and lineage events to Surreal provenance.
pub struct HostIngressRecorderImpl {
    store: Arc<SurrealProvenanceStore>,
}

impl HostIngressRecorderImpl {
    pub fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }

    async fn record_slack_gate_decision(&self, event: &ProducedEvent) -> Result<()> {
        match self.record_slack_gate_decision_inner(event).await {
            Err(BamlRtError::InvalidArgument(reason)) => {
                if let Err(error) = self.record_rejected_slack_action(event, &reason).await {
                    tracing::warn!(error = %error, "could not persist rejected Slack gate action");
                }
                tracing::warn!(reason = %reason, "rejected invalid Slack gate action");
                Ok(())
            }
            result => result,
        }
    }

    async fn record_rejected_slack_action(
        &self,
        event: &ProducedEvent,
        reason: &str,
    ) -> Result<()> {
        let Some(context_id) = event.context_id.clone() else {
            return Ok(());
        };
        let Some(task_id) = event.task_id.clone() else {
            return Ok(());
        };
        let Some(record) = event
            .messages
            .first()
            .and_then(|message| message.get("records"))
            .and_then(serde_json::Value::as_array)
            .and_then(|records| records.first())
            .filter(|record| {
                record
                    .get("record_kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("karakara.gate_decision")
            })
        else {
            return Ok(());
        };
        let gate_id = record
            .get("gate_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let agent_id = record
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| UuidId::parse_str(value).ok())
            .map(AgentId::from_uuid)
            .unwrap_or_else(|| AgentId::from_uuid(UuidId::new(Uuid::nil())));
        self.store
            .add_event(ProvEvent::audit_decision(
                context_id,
                crate::events::AuditDecision::GateActionRejected {
                    gate_id,
                    task_id,
                    agent_id,
                    reason: reason.to_string(),
                },
            ))
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))
    }

    async fn record_slack_gate_decision_inner(&self, event: &ProducedEvent) -> Result<()> {
        let Some(record) = event
            .messages
            .first()
            .and_then(|message| message.get("records"))
            .and_then(serde_json::Value::as_array)
            .and_then(|records| records.first())
            .filter(|record| {
                record
                    .get("record_kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("karakara.gate_decision")
            })
        else {
            return Ok(());
        };
        let required = |field: &str| {
            record
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    BamlRtError::InvalidArgument(format!("Slack gate decision missing {field}"))
                })
        };
        let context_id = event.context_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument("Slack gate decision missing context_id".to_string())
        })?;
        let task_id = event.task_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument("Slack gate decision missing task_id".to_string())
        })?;
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str(required("agent_id")?).map_err(|error| {
                BamlRtError::InvalidArgument(format!("invalid gate agent_id: {error}"))
            })?);
        let outcome = match required("outcome")? {
            "allow" => GateDecisionOutcome::Allow,
            "deny" => GateDecisionOutcome::Deny,
            other => {
                return Err(BamlRtError::InvalidArgument(format!(
                    "invalid Slack gate outcome {other}"
                )));
            }
        };
        let actor = required("actor_id")?.to_string();
        let channel = required("channel_id")?.to_string();
        let gate_id = required("gate_id")?.to_string();
        // Cheap early exit for repeat clicks; `decide_gate` below is what makes the
        // decision atomic against other clicks, other runners and the expiry sweep.
        if self
            .store
            .gate_ledger_decision(&context_id, &gate_id)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?
            .is_some()
        {
            return Ok(());
        }
        let request = self
            .store
            .gate_ledger_request(&context_id, &gate_id)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("Slack action references an unknown gate".to_string())
            })?;
        if request.task_id != task_id || request.agent_id != agent_id {
            return Err(BamlRtError::InvalidArgument(
                "Slack action scope does not match its gate request".to_string(),
            ));
        }
        if request.channel.as_deref() != Some(channel.as_str()) {
            return Err(BamlRtError::InvalidArgument(
                "Slack action channel does not match its gate request".to_string(),
            ));
        }
        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::KARAKARA_GATE);
        let (outcome, principal, reason) = if now_ms >= request.expires_at_ms {
            (
                GateDecisionOutcome::Deny,
                GatePrincipal::Runtime {
                    reason: "expiry".to_string(),
                },
                "gate confirmation expired".to_string(),
            )
        } else {
            (
                outcome,
                GatePrincipal::Channel { channel, actor },
                "Slack channel action resolved the gate".to_string(),
            )
        };
        self.store
            .decide_gate(
                &context_id,
                crate::store::GateDecisionWrite {
                    gate_id,
                    task_id,
                    agent_id,
                    outcome,
                    principal,
                    reason,
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))
    }
}

fn host_poll_ingress_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::nil()))
}

fn ingress_user_message_metadata() -> Option<HashMap<String, String>> {
    Some(HashMap::from([(
        "user_speaker_kind".to_string(),
        user_speaker_kinds::INGRESS.to_string(),
    )]))
}

fn build_poll_user_message_event(
    event: &ProducedEvent,
    body: &IngressPollBody,
) -> Result<ProvEvent> {
    let context_id = event
        .context_id
        .clone()
        .ok_or_else(|| BamlRtError::InvalidArgument("ProducedEvent missing context_id".into()))?;
    let batch_message_id = event
        .message_id
        .as_deref()
        .ok_or_else(|| BamlRtError::InvalidArgument("ProducedEvent missing message_id".into()))?;
    let message_id = MessageId::from(batch_message_id);
    let anchor = activity_anchor_for_ingress_poll_user(&context_id, batch_message_id);
    let agent_id = host_poll_ingress_agent_id();
    let content = vec![body.0.clone()];
    let timestamp_ms =
        baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_POLL_USER);
    if let Some(task_id) = event.task_id.clone() {
        return Ok(ProvEvent::Task(crate::events::TaskScopedEvent {
            id: anchor,
            context_id,
            task_id,
            timestamp_ms,
            data: ProvEventData::MessageReceived {
                id: message_id,
                role: "user".to_string(),
                content,
                metadata: ingress_user_message_metadata(),
                agent_id: host_poll_ingress_agent_id(),
                citations: Vec::new(),
            },
        }));
    }
    Ok(ProvEvent::Global(crate::events::GlobalEvent {
        id: anchor,
        context_id,
        timestamp_ms,
        data: ProvEventData::MessageReceived {
            id: message_id.clone(),
            role: "user".to_string(),
            content,
            metadata: ingress_user_message_metadata(),
            agent_id,
            citations: Vec::new(),
        },
    }))
}

fn build_unit_user_message_event(
    scope: &RuntimeScope,
    unit_key: &str,
    body: &IngressPollBody,
) -> ProvEvent {
    let context_id = scope.context_id().clone();
    let task_id = scope
        .task_id_opt()
        .expect("dispatch unit scope must be task-scoped")
        .clone();
    let message_id = scope.message_id().clone();
    let anchor = activity_anchor_for_ingress_unit_user(&context_id, unit_key);
    ProvEvent::Task(crate::events::TaskScopedEvent {
        id: anchor,
        context_id,
        task_id,
        timestamp_ms: baml_rt_core::now_unix_ms(baml_rt_core::clock_events::HOST_INGRESS_UNIT_USER),
        data: ProvEventData::MessageReceived {
            id: message_id,
            role: "user".to_string(),
            content: vec![body.0.clone()],
            metadata: ingress_user_message_metadata(),
            agent_id: scope.agent_id().clone(),
            citations: Vec::new(),
        },
    })
}

#[async_trait::async_trait]
impl HostIngressRecorder for HostIngressRecorderImpl {
    async fn expire_pending_gates(&self) -> Result<usize> {
        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::KARAKARA_GATE);
        self.store
            .expire_pending_gate_requests(now_ms)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))
    }

    async fn record_source_poll(&self, event: &ProducedEvent) -> Result<()> {
        self.record_slack_gate_decision(event).await?;
        let context_id = event.context_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument("ProducedEvent missing context_id".into())
        })?;
        let source_kind = event.source_kind.as_str().to_string();
        let source_key = event.source_key.as_str().to_string();
        let source_cursor = event
            .messages
            .first()
            .and_then(|m| m.get("source_cursor"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let record_count = event.messages.len();
        let source_message_ts: Vec<String> = event
            .messages
            .iter()
            .filter_map(|m| m.get("message_ts").and_then(|v| v.as_str()))
            .map(str::to_string)
            .collect();
        let channel = event
            .messages
            .iter()
            .flat_map(|message| {
                message
                    .get("records")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .find_map(|record| {
                record
                    .get("channel_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
            });
        let prov_event = ProvEvent::host_source_poll_recorded_with_channel(
            context_id,
            source_kind,
            source_key,
            source_cursor,
            event.schema_version.as_str().to_string(),
            record_count,
            event.producer_key.clone(),
            channel,
            source_message_ts,
        );
        // Gates resolve the flow's origin from this row, so a lost write would
        // later deny the flow with no trace. Fail the poll so the event is retried;
        // the row's id comes from (context, source, cursor), so a retry overwrites it.
        self.store
            .add_event(prov_event)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))
    }

    async fn record_ingress_poll_user_message(
        &self,
        event: &ProducedEvent,
        body: &IngressPollBody,
    ) -> Result<IngressPollUserMessageRef> {
        let batch_message_id = event.message_id.as_deref().ok_or_else(|| {
            BamlRtError::InvalidArgument("ProducedEvent missing message_id".into())
        })?;
        let prov_event = build_poll_user_message_event(event, body)?;
        self.store
            .add_event_with_logging(prov_event, "host ingress poll user message")
            .await;
        Ok(IngressPollUserMessageRef {
            message_id: MessageId::from(batch_message_id),
        })
    }

    async fn with_task_prelude(
        &self,
        parent: &RuntimeScope,
        agent_id: AgentId,
        unit: DispatchWorkUnit,
    ) -> Result<WithTaskPrelude> {
        let body = format_unit_ingress_body(&unit);
        if body.0.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "withTask records produced empty ingress body".into(),
            ));
        }
        let scope = dispatch_unit_runtime_scope(
            parent.context_id().clone(),
            agent_id,
            unit.unit_key.0.as_str(),
        );
        let task_id = scope
            .task_id_opt()
            .expect("dispatch unit scope must be task-scoped")
            .clone();
        let context_id = scope.context_id().clone();
        let binding = TaskAgentBinding::new(
            context_id,
            task_id,
            scope.agent_id().clone(),
            TaskAgentBindingSource::HostWithTaskPrelude,
        )
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?;
        self.store
            .bind_task_executing_agent(binding)
            .await
            .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?;
        let prov_event = build_unit_user_message_event(&scope, unit.unit_key.0.as_str(), &body);
        self.store
            .add_event_with_logging(prov_event, "host ingress unit user message")
            .await;
        Ok(WithTaskPrelude {
            unit_key: unit.unit_key.0.clone(),
            scope,
            unit_history_ref: 1,
        })
    }

    async fn record_dispatch_accepted(
        &self,
        request: &AgentDispatchRequest,
        target: DispatchTarget,
    ) -> Result<()> {
        let context_id = request
            .context_id
            .clone()
            .ok_or_else(|| BamlRtError::InvalidArgument("dispatch missing context_id".into()))?;
        let source = HostIngressSourceRef::from_dispatch_request(request);
        let (source_kind, source_key) = match &source {
            HostIngressSourceRef::SourceRecords { kind, key } => (kind.clone(), key.clone()),
            HostIngressSourceRef::Unspecified => (
                HostIngressSourceRef::UNSPECIFIED_KIND.to_string(),
                HostIngressSourceRef::UNSPECIFIED_KEY.to_string(),
            ),
        };
        let prov_event = ProvEvent::host_dispatch_accepted(
            context_id,
            request.routing_key.as_str().to_string(),
            request.message_type.as_str().to_string(),
            target,
            source_kind,
            source_key,
            request.producer_key.clone(),
        );
        self.store
            .add_event_with_logging(prov_event, "host dispatch accepted")
            .await;
        Ok(())
    }

    async fn record_dispatch_rejected(
        &self,
        request: &AgentDispatchRequest,
        target: DispatchTarget,
        detail: &str,
        transport_failure: bool,
    ) -> Result<()> {
        let context_id = request
            .context_id
            .clone()
            .ok_or_else(|| BamlRtError::InvalidArgument("dispatch missing context_id".into()))?;
        let prov_event = ProvEvent::host_dispatch_rejected(HostDispatchRejectedSpec {
            context_id,
            routing_key: request.routing_key.as_str().to_string(),
            schema_version: request.message_type.clone(),
            target,
            source: HostIngressSourceRef::from_dispatch_request(request),
            producer_key: request.producer_key.clone(),
            detail: detail.to_string(),
            failure_kind: HostDispatchFailureKind::from_transport_flag(transport_failure),
        });
        self.store
            .add_event_with_logging(prov_event, "host dispatch rejected")
            .await;
        Ok(())
    }
}
