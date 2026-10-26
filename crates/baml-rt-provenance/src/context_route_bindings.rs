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
//! Semantic decisions (binding created, binding conflicted) are appended to
//! provenance as [`AuditDecision`] rows. A read that matches an existing
//! binding is not a decision and is never audited.

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
