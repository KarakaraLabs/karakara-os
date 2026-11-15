// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_karakara::{
    GateAuthorizationRequest, GateAuthorizationResolution, GateAuthorizationResolver,
};
use baml_rt_provenance::{
    AuditDecision, GateDecisionOutcome, GateDecisionWrite, GateFlowOrigin, GateLedgerDecision,
    GatePrincipal, ProvEvent, ProvenanceWriter,
};

const DEFAULT_GATE_TTL: Duration = Duration::from_secs(15 * 60);

pub(crate) struct ProvenanceGateAuthorizationResolver {
    writer: Arc<dyn ProvenanceWriter>,
    ttl: Duration,
}

fn principal_kind(principal: &GatePrincipal) -> &'static str {
    match principal {
        GatePrincipal::Ingress { .. } => "ingress",
        GatePrincipal::Channel { .. } => "channel",
        GatePrincipal::Runtime { .. } => "runtime",
    }
}

fn outcome_label(outcome: GateDecisionOutcome) -> &'static str {
    match outcome {
        GateDecisionOutcome::Allow => "allow",
        GateDecisionOutcome::Deny => "deny",
    }
}

/// Turn a recorded decision into a resolution, counting it in the gate metrics.
fn resolution_from_decision(decision: GateLedgerDecision) -> GateAuthorizationResolution {
    baml_rt_observability::metrics::record_gate_outcome(
        outcome_label(decision.outcome),
        principal_kind(&decision.principal),
    );
    match decision.outcome {
        GateDecisionOutcome::Allow => GateAuthorizationResolution::Allow {
            principal: serde_json::to_string(&decision.principal)
                .unwrap_or_else(|_| "recorded_principal".to_string()),
        },
        GateDecisionOutcome::Deny => GateAuthorizationResolution::Deny {
            reason: decision.reason,
        },
    }
}

impl ProvenanceGateAuthorizationResolver {
    pub(crate) fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            writer,
            ttl: DEFAULT_GATE_TTL,
        }
    }

    async fn record_request(
        &self,
        request: &GateAuthorizationRequest,
        gate_id: &str,
        channel: Option<String>,
        expires_at_ms: u64,
    ) -> Result<()> {
        let task_id = request.scope.task_id_opt().cloned().ok_or_else(|| {
            BamlRtError::InvalidArgument("gate request must be task-scoped".to_string())
        })?;
        self.writer
            .add_event(ProvEvent::audit_decision(
                request.scope.context_id().clone(),
                AuditDecision::GateRequested {
                    gate_id: gate_id.to_string(),
                    task_id,
                    agent_id: request.scope.agent_id().clone(),
                    tool_name: request.tool_name.clone(),
                    channel,
                    expires_at_ms,
                },
            ))
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))
    }

    /// Decide the gate once and return the decision that stands. If another writer
    /// (a click, another runner, the expiry sweep) decided first, its decision wins.
    async fn decide(
        &self,
        request: &GateAuthorizationRequest,
        gate_id: &str,
        outcome: GateDecisionOutcome,
        principal: GatePrincipal,
        reason: impl Into<String>,
    ) -> Result<GateAuthorizationResolution> {
        let task_id = request.scope.task_id_opt().cloned().ok_or_else(|| {
            BamlRtError::InvalidArgument("gate decision must be task-scoped".to_string())
        })?;
        let stands = self
            .writer
            .decide_gate(
                request.scope.context_id(),
                GateDecisionWrite {
                    gate_id: gate_id.to_string(),
                    task_id,
                    agent_id: request.scope.agent_id().clone(),
                    outcome,
                    principal,
                    reason: reason.into(),
                },
            )
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?;
        Ok(resolution_from_decision(stands))
    }

    async fn post_slack_confirmation(
        &self,
        request: &GateAuthorizationRequest,
        gate_id: &str,
        channel: &str,
    ) -> Result<()> {
        let task_id = request
            .scope
            .task_id_opt()
            .expect("gate request checked as task-scoped");
        baml_tools_slack::post_gate_confirmation(baml_tools_slack::SlackGateConfirmation {
            channel,
            tool_name: &request.tool_name,
            prompt: &request.prompt,
            gate_id,
            context_id: request.scope.context_id().as_str(),
            task_id: task_id.as_str(),
            agent_id: request.scope.agent_id().as_str(),
        })
        .await
    }
}

#[async_trait]
impl GateAuthorizationResolver for ProvenanceGateAuthorizationResolver {
    async fn resolve(
        &self,
        request: &GateAuthorizationRequest,
    ) -> Result<GateAuthorizationResolution> {
        let Some(_task_id) = request.scope.task_id_opt() else {
            return Ok(GateAuthorizationResolution::Deny {
                reason: "tier-3 gate requires a task-scoped context".to_string(),
            });
        };
        let gate_id = request.gate_id();
        if let Some(decision) = self
            .writer
            .gate_ledger_decision(request.scope.context_id(), &gate_id)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?
        {
            return Ok(resolution_from_decision(decision));
        }

        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::KARAKARA_GATE);
        if let Some(pending) = self
            .writer
            .gate_ledger_request(request.scope.context_id(), &gate_id)
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?
            // Only a channel request waits on someone. A request without a channel
            // means an earlier attempt recorded it but failed to write its decision;
            // fall through and decide again from the origin, which is deterministic.
            .filter(|pending| pending.channel.is_some())
        {
            if now_ms >= pending.expires_at_ms {
                return self
                    .decide(
                        request,
                        &gate_id,
                        GateDecisionOutcome::Deny,
                        GatePrincipal::Runtime {
                            reason: "expiry".to_string(),
                        },
                        "gate confirmation expired",
                    )
                    .await;
            }
            return Ok(GateAuthorizationResolution::Pending {
                prompt: request.prompt.clone(),
            });
        }

        let origin = self
            .writer
            .gate_flow_origin(request.scope.context_id())
            .await
            .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?;
        let expires_at_ms = now_ms.saturating_add(self.ttl.as_millis() as u64);
        if let GateFlowOrigin::Channel { channel } = &origin {
            // Record the request before claiming: if this runner dies before
            // posting, the request still exists and the expiry sweep denies it.
            self.record_request(request, &gate_id, Some(channel.clone()), expires_at_ms)
                .await?;
            // Concurrent identical calls can all reach this point. The store lets
            // exactly one of them claim the gate; only that one posts the buttons.
            let claimed = self
                .writer
                .claim_gate_confirmation(&gate_id)
                .await
                .map_err(|error| BamlRtError::BamlRuntime(error.to_string()))?;
            if !claimed {
                return Ok(GateAuthorizationResolution::Pending {
                    prompt: request.prompt.clone(),
                });
            }
            if let Err(error) = self
                .post_slack_confirmation(request, &gate_id, channel)
                .await
            {
                return self
                    .decide(
                        request,
                        &gate_id,
                        GateDecisionOutcome::Deny,
                        GatePrincipal::Runtime {
                            reason: "channel_delivery_failed".to_string(),
                        },
                        format!("could not post confirmation to Slack channel {channel}: {error}"),
                    )
                    .await;
            }
            baml_rt_observability::metrics::record_gate_outcome("pending", "channel");
            return Ok(GateAuthorizationResolution::Pending {
                prompt: request.prompt.clone(),
            });
        }

        if let GateFlowOrigin::MachineIngress { ingress_key } = &origin {
            self.record_request(request, &gate_id, None, expires_at_ms)
                .await?;
            return self
                .decide(
                    request,
                    &gate_id,
                    GateDecisionOutcome::Allow,
                    GatePrincipal::Ingress {
                        ingress_key: ingress_key.clone(),
                    },
                    "registered machine ingress authorized the flow",
                )
                .await;
        }

        let reason = match origin {
            GateFlowOrigin::LiveChat => "chat flow has no typed channel confirmation integration",
            GateFlowOrigin::None => "flow has no registered ingress or channel",
            GateFlowOrigin::Channel { .. } | GateFlowOrigin::MachineIngress { .. } => {
                unreachable!("gate origin was handled above")
            }
        };
        self.record_request(request, &gate_id, None, expires_at_ms)
            .await?;
        self.decide(
            request,
            &gate_id,
            GateDecisionOutcome::Deny,
            GatePrincipal::Runtime {
                reason: "no_trusted_origin".to_string(),
            },
            reason,
        )
        .await
    }
}
