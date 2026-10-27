// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Operational context route bindings.
//!
//! A binding pins one context route to the exact `(agent_id, artifact_hash)`
//! pair that first participated under it. The row is mutable operational state,
//! not a provenance node, and never appears in graph export, context history, or
//! an agent prompt.
//!
//! `AgentId` answers which agent source; `artifact_hash` answers which exact
//! executable package. Two artifacts can share one `AgentId`, so both are
//! pinned together.
//!
//! Semantic decisions (binding created, binding conflicted, revision rebound,
//! pinned revision missing, undeploy override) are appended to provenance as
//! [`AuditDecision`] rows. A read that matches an existing binding is not a
//! decision and is never audited.

use baml_rt_core::{
    ArtifactHash,
    ids::{AgentId, ContextId, UuidId},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    events::AuditDecision,
    surreal_store::{SurrealProvenanceStore, map_surreal_error},
    surreal_tables::TBL_CONTEXT_ROUTE_BINDINGS,
};

/// The revision one context route is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRouteBinding {
    pub context_id: ContextId,
    pub package_name: String,
    pub agent_id: AgentId,
    pub artifact_hash: ArtifactHash,
}

impl ContextRouteBinding {
    /// True when both pinned identifiers match the resolved pair.
    #[must_use]
    pub fn pins(&self, agent_id: &AgentId, artifact_hash: &ArtifactHash) -> bool {
        &self.agent_id == agent_id && &self.artifact_hash == artifact_hash
    }
}

/// A turn's resolved route identity, before admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRouteBindingRequest {
    pub context_id: ContextId,
    pub package_name: String,
    pub agent_id: AgentId,
    pub artifact_hash: ArtifactHash,
}

impl ContextRouteBindingRequest {
    #[must_use]
    pub fn new(
        context_id: ContextId,
        package_name: impl Into<String>,
        agent_id: AgentId,
        artifact_hash: ArtifactHash,
    ) -> Self {
        Self {
            context_id,
            package_name: package_name.into(),
            agent_id,
            artifact_hash,
        }
    }
}

/// What resolving the binding did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingResolution {
    /// The context already pinned this exact pair.
    Existing(ContextRouteBinding),
    /// This turn created the binding.
    Created(ContextRouteBinding),
}

impl BindingResolution {
    #[must_use]
    pub fn binding(&self) -> &ContextRouteBinding {
        match self {
            Self::Existing(binding) | Self::Created(binding) => binding,
        }
    }
}

/// How long after its last recorded activity a pinned context still blocks an
/// undeploy without an explicit operator override.
///
/// This is a deploy guard, not a history-retention policy: rows and events stay
/// durable regardless of the window.
pub const DEFAULT_CONTEXT_ACTIVITY_RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;

/// One pinned context whose last recorded activity falls inside the guard window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedContextActivity {
    pub context_id: ContextId,
    pub latest_timestamp_ms: u64,
}

/// An explicit operator rebind of one context route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRouteRebindRequest {
    pub context_id: ContextId,
    pub package_name: String,
    pub agent_id: AgentId,
    pub artifact_hash: ArtifactHash,
    /// Required operator explanation; recorded on the revision-boundary event.
    pub rationale: String,
}

/// An explicit operator fork: stop the old context and pin a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRouteForkRequest {
    pub old_context_id: ContextId,
    pub package_name: String,
    pub new_context_id: ContextId,
    pub agent_id: AgentId,
    pub artifact_hash: ArtifactHash,
    /// Required operator explanation; recorded on the old context.
    pub rationale: String,
}

const CREATE_BINDING_SQL: &str = r#"
BEGIN;
LET $current = SELECT * FROM context_route_bindings
    WHERE context_id = $ctx AND package_name = $pkg LIMIT 1;
IF array::len($current) = 0 {
    CREATE context_route_bindings CONTENT {
        context_id: $ctx,
        package_name: $pkg,
        agent_id: $agent,
        artifact_hash: $artifact
    };
} ELSE {
    SELECT * FROM context_route_bindings WHERE false;
};
COMMIT;
"#;

const REBIND_BINDING_SQL: &str = r#"
UPDATE context_route_bindings SET
    agent_id = $agent,
    artifact_hash = $artifact
WHERE context_id = $ctx
  AND package_name = $pkg;
"#;

fn parse_agent_id(raw: &str) -> Result<AgentId> {
    UuidId::parse_str(raw)
        .map(AgentId::from_uuid)
        .map_err(|error| ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: format!("invalid agent_id {raw:?}: {error}"),
        })
}

fn parse_binding_row(row: &Value) -> Result<ContextRouteBinding> {
    let context_id = row
        .get("context_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: "binding row missing context_id".to_string(),
        })?;
    let package_name = row
        .get("package_name")
        .and_then(Value::as_str)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: "binding row missing package_name".to_string(),
        })?
        .to_string();
    let agent_id = row
        .get("agent_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: "binding row missing agent_id".to_string(),
        })
        .and_then(parse_agent_id)?;
    let artifact_hash = row
        .get("artifact_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: "binding row missing artifact_hash".to_string(),
        })
        .and_then(|raw| {
            raw.parse::<ArtifactHash>()
                .map_err(|error| ProvenanceError::InvalidEvent {
                    activity_anchor: "context_route_binding".to_string(),
                    reason: format!("binding row has invalid artifact_hash {raw:?}: {error}"),
                })
        })?;
    Ok(ContextRouteBinding {
        context_id: ContextId::from(context_id),
        package_name,
        agent_id,
        artifact_hash,
    })
}

impl SurrealProvenanceStore {
    /// Read the pinned revision for one context route.
    pub async fn read_context_route_binding(
        &self,
        context_id: &ContextId,
        package_name: &str,
    ) -> Result<Option<ContextRouteBinding>> {
        let sql = format!(
            "SELECT * FROM {TBL_CONTEXT_ROUTE_BINDINGS} \
             WHERE context_id = $ctx AND package_name = $pkg LIMIT 1"
        );
        let response = self
            .db()
            .query(&sql)
            .bind(("ctx", context_id.as_str().to_string()))
            .bind(("pkg", package_name.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> =
            crate::surreal_store::check_and_take_zero(response, map_surreal_error)?;
        rows.first().map(parse_binding_row).transpose()
    }

    /// Admit one turn against the context route binding.
    ///
    /// First participation creates the binding by compare-and-set: the write
    /// inserts only when the row is absent, and the re-read decides the winner.
    /// A concurrent turn that resolved a different pair loses and is refused
    /// with [`ProvenanceError::ContextRouteBindingConflict`].
    pub async fn resolve_context_route_binding(
        &self,
        request: ContextRouteBindingRequest,
    ) -> Result<BindingResolution> {
        if request.package_name.trim().is_empty() {
            return Err(ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: "binding package_name must be non-empty".to_string(),
            });
        }

        if let Some(pinned) = self
            .read_context_route_binding(&request.context_id, &request.package_name)
            .await?
        {
            if pinned.pins(&request.agent_id, &request.artifact_hash) {
                return Ok(BindingResolution::Existing(pinned));
            }
            self.record_binding_conflict(&request, &pinned).await?;
            return Err(binding_conflict(&request, &pinned));
        }

        self.run_operational_write(
            CREATE_BINDING_SQL,
            vec![
                (
                    "ctx",
                    Value::String(request.context_id.as_str().to_string()),
                ),
                ("pkg", Value::String(request.package_name.clone())),
                (
                    "agent",
                    Value::String(request.agent_id.as_str().to_string()),
                ),
                (
                    "artifact",
                    Value::String(request.artifact_hash.as_str().to_string()),
                ),
            ],
        )
        .await?;

        let pinned = self
            .read_context_route_binding(&request.context_id, &request.package_name)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: "binding create committed no row".to_string(),
            })?;

        if !pinned.pins(&request.agent_id, &request.artifact_hash) {
            // A concurrent first turn won the compare-and-set.
            self.record_binding_conflict(&request, &pinned).await?;
            return Err(binding_conflict(&request, &pinned));
        }

        self.record_audit_decision(
            &request.context_id,
            AuditDecision::BindingCreated {
                package_name: request.package_name.clone(),
                agent_id: request.agent_id.clone(),
                artifact_hash: request.artifact_hash.clone(),
            },
        )
        .await?;
        Ok(BindingResolution::Created(pinned))
    }

    async fn record_binding_conflict(
        &self,
        request: &ContextRouteBindingRequest,
        pinned: &ContextRouteBinding,
    ) -> Result<()> {
        self.record_audit_decision(
            &request.context_id,
            AuditDecision::BindingConflict {
                package_name: request.package_name.clone(),
                pinned_agent_id: pinned.agent_id.clone(),
                pinned_artifact_hash: pinned.artifact_hash.clone(),
                resolved_agent_id: request.agent_id.clone(),
                resolved_artifact_hash: request.artifact_hash.clone(),
            },
        )
        .await
    }

    /// Read the binding that pins `agent_id` in a context, regardless of package.
    ///
    /// Lease acquisition uses this to corroborate the artifact it is allowed to
    /// run: a boot whose bytes differ from the pinned revision may not take the
    /// pair.
    pub async fn read_context_route_binding_for_agent(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<Option<ContextRouteBinding>> {
        let sql = format!(
            "SELECT * FROM {TBL_CONTEXT_ROUTE_BINDINGS} \
             WHERE context_id = $ctx AND agent_id = $agent LIMIT 1"
        );
        let response = self
            .db()
            .query(&sql)
            .bind(("ctx", context_id.as_str().to_string()))
            .bind(("agent", agent_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> =
            crate::surreal_store::check_and_take_zero(response, map_surreal_error)?;
        rows.first().map(parse_binding_row).transpose()
    }

    /// Pinned contexts for one exact revision with activity at or after
    /// `active_since_ms`.
    ///
    /// Activity comes from the context picker index (the durable last-activity
    /// summary written for conversational and host-ingress events). A context
    /// with no picker row has no recorded activity to protect.
    pub async fn list_pinned_context_activity(
        &self,
        package_name: &str,
        artifact_hash: &ArtifactHash,
        active_since_ms: u64,
    ) -> Result<Vec<PinnedContextActivity>> {
        let sql = format!(
            "SELECT * FROM {TBL_CONTEXT_ROUTE_BINDINGS} \
             WHERE package_name = $pkg AND artifact_hash = $artifact"
        );
        let response = self
            .db()
            .query(&sql)
            .bind(("pkg", package_name.to_string()))
            .bind(("artifact", artifact_hash.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> =
            crate::surreal_store::check_and_take_zero(response, map_surreal_error)?;
        let context_ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.get("context_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        if context_ids.is_empty() {
            return Ok(Vec::new());
        }

        let sql = "SELECT context_id, latest_timestamp_ms FROM context_picker_index \
                   WHERE context_id IN $ctxs AND latest_timestamp_ms >= $since";
        let response = self
            .db()
            .query(sql)
            .bind(("ctxs", context_ids))
            .bind(("since", active_since_ms))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> =
            crate::surreal_store::check_and_take_zero(response, map_surreal_error)?;
        let mut active: Vec<PinnedContextActivity> = rows
            .iter()
            .filter_map(|row| {
                let context_id = row.get("context_id").and_then(Value::as_str)?;
                let latest_timestamp_ms = row.get("latest_timestamp_ms").and_then(Value::as_u64)?;
                Some(PinnedContextActivity {
                    context_id: ContextId::from(context_id),
                    latest_timestamp_ms,
                })
            })
            .collect();
        active.sort_by_key(|row| row.context_id.as_str().to_string());
        Ok(active)
    }

    /// Explicitly move one context route to a new revision.
    ///
    /// Rebind is the only path that changes an existing binding outside of
    /// `close_and_fork`. It records the revision boundary and releases the old
    /// pair lease so a boot still running the previous artifact cannot keep
    /// writing.
    pub async fn rebind_context_route_binding(
        &self,
        request: ContextRouteRebindRequest,
    ) -> Result<ContextRouteBinding> {
        require_rationale(&request.rationale, "rebind")?;
        let pinned = self
            .read_context_route_binding(&request.context_id, &request.package_name)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: format!(
                    "rebind requires an existing binding for context {} package {}",
                    request.context_id.as_str(),
                    request.package_name
                ),
            })?;
        if pinned.pins(&request.agent_id, &request.artifact_hash) {
            return Ok(pinned);
        }

        self.run_operational_write(
            REBIND_BINDING_SQL,
            vec![
                (
                    "ctx",
                    Value::String(request.context_id.as_str().to_string()),
                ),
                ("pkg", Value::String(request.package_name.clone())),
                (
                    "agent",
                    Value::String(request.agent_id.as_str().to_string()),
                ),
                (
                    "artifact",
                    Value::String(request.artifact_hash.as_str().to_string()),
                ),
            ],
        )
        .await?;

        let updated = self
            .read_context_route_binding(&request.context_id, &request.package_name)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: "rebind committed no row".to_string(),
            })?;

        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT);
        self.release_context_agent_lease_for_pair(&pinned.context_id, &pinned.agent_id, now_ms)
            .await?;

        self.record_audit_decision(
            &request.context_id,
            AuditDecision::RevisionRebound {
                package_name: request.package_name,
                from_agent_id: pinned.agent_id,
                from_artifact_hash: pinned.artifact_hash,
                to_agent_id: updated.agent_id.clone(),
                to_artifact_hash: updated.artifact_hash.clone(),
                rationale: request.rationale,
            },
        )
        .await?;
        Ok(updated)
    }

    /// Stop an old context and start a new one under the supplied revision.
    ///
    /// The old binding stays in place so its history remains explainable; the
    /// operator stops sending work to it. The new context receives its own
    /// binding and a revision-boundary event lands on the old context.
    pub async fn close_and_fork_context_route_binding(
        &self,
        request: ContextRouteForkRequest,
    ) -> Result<ContextRouteBinding> {
        require_rationale(&request.rationale, "close_and_fork")?;
        if request.old_context_id == request.new_context_id {
            return Err(ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: "close_and_fork requires a new context id".to_string(),
            });
        }
        let old = self
            .read_context_route_binding(&request.old_context_id, &request.package_name)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_route_binding".to_string(),
                reason: format!(
                    "close_and_fork requires an existing binding for context {} package {}",
                    request.old_context_id.as_str(),
                    request.package_name
                ),
            })?;

        let forked = self
            .resolve_context_route_binding(ContextRouteBindingRequest::new(
                request.new_context_id.clone(),
                request.package_name.clone(),
                request.agent_id.clone(),
                request.artifact_hash.clone(),
            ))
            .await?
            .binding()
            .clone();

        let now_ms = baml_rt_core::now_unix_ms(baml_rt_core::clock_events::PROVENANCE_EVENT);
        self.release_context_agent_lease_for_pair(&old.context_id, &old.agent_id, now_ms)
            .await?;

        self.record_audit_decision(
            &request.old_context_id,
            AuditDecision::RevisionRebound {
                package_name: request.package_name,
                from_agent_id: old.agent_id,
                from_artifact_hash: old.artifact_hash,
                to_agent_id: forked.agent_id.clone(),
                to_artifact_hash: forked.artifact_hash.clone(),
                rationale: format!("close_and_fork: {}", request.rationale),
            },
        )
        .await?;
        Ok(forked)
    }

    /// Record one operator undeploy override and the pinned-revision loss it
    /// accepts, on every context the revision was pinning.
    pub async fn record_undeploy_override(
        &self,
        package_name: &str,
        agent_id: &AgentId,
        artifact_hash: &ArtifactHash,
        contexts: &[ContextId],
        rationale: &str,
        resolved: Option<(&AgentId, &ArtifactHash)>,
    ) -> Result<()> {
        require_rationale(rationale, "undeploy override")?;
        let context_names: Vec<String> = contexts
            .iter()
            .map(|context| context.as_str().to_string())
            .collect();
        for context_id in contexts {
            self.record_audit_decision(
                context_id,
                AuditDecision::UndeployOverride {
                    package_name: package_name.to_string(),
                    agent_id: agent_id.clone(),
                    artifact_hash: artifact_hash.clone(),
                    contexts: context_names.clone(),
                    rationale: rationale.to_string(),
                },
            )
            .await?;
            self.record_audit_decision(
                context_id,
                AuditDecision::PinnedRevisionMissing {
                    package_name: package_name.to_string(),
                    pinned_agent_id: agent_id.clone(),
                    pinned_artifact_hash: artifact_hash.clone(),
                    resolved_agent_id: resolved.map(|(agent, _)| agent.clone()),
                    resolved_artifact_hash: resolved.map(|(_, artifact)| artifact.clone()),
                },
            )
            .await?;
        }
        Ok(())
    }
}

fn require_rationale(rationale: &str, operation: &str) -> Result<()> {
    if rationale.trim().is_empty() {
        return Err(ProvenanceError::InvalidEvent {
            activity_anchor: "context_route_binding".to_string(),
            reason: format!("{operation} requires a non-empty operator rationale"),
        });
    }
    Ok(())
}

fn binding_conflict(
    request: &ContextRouteBindingRequest,
    pinned: &ContextRouteBinding,
) -> ProvenanceError {
    ProvenanceError::ContextRouteBindingConflict(Box::new(
        crate::error::ContextRouteBindingConflict {
            context_id: request.context_id.as_str().to_string(),
            package_name: request.package_name.clone(),
            pinned_agent_id: pinned.agent_id.as_str().to_string(),
            pinned_artifact_hash: pinned.artifact_hash.as_str().to_string(),
            resolved_agent_id: request.agent_id.as_str().to_string(),
            resolved_artifact_hash: request.artifact_hash.as_str().to_string(),
        },
    ))
}
