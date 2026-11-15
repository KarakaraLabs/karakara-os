// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Host-resolved authorization for irreversible tool calls.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{Result, context::RuntimeScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// One irreversible action presented to the host gate ledger.
#[derive(Debug, Clone)]
pub struct GateAuthorizationRequest {
    pub scope: RuntimeScope,
    pub tool_name: String,
    pub args: Value,
    pub prompt: String,
}

impl GateAuthorizationRequest {
    /// Stable identity for retries of the same task, tool, and arguments.
    #[must_use]
    pub fn gate_id(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.scope.context_id().as_str().as_bytes());
        hash.update([0]);
        if let Some(task_id) = self.scope.task_id_opt() {
            hash.update(task_id.as_str().as_bytes());
        }
        hash.update([0]);
        hash.update(self.tool_name.as_bytes());
        hash.update([0]);
        hash.update(serde_json::to_vec(&self.args).unwrap_or_default());
        format!("gate-{:x}", hash.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GateAuthorizationResolution {
    Allow { principal: String },
    Pending { prompt: String },
    Deny { reason: String },
}

#[async_trait]
pub trait GateAuthorizationResolver: Send + Sync {
    async fn resolve(
        &self,
        request: &GateAuthorizationRequest,
    ) -> Result<GateAuthorizationResolution>;
}

/// Safe default for tests and hosts that have not installed a graph-backed resolver.
#[derive(Debug, Default)]
pub struct DenyGateAuthorizationResolver;

#[async_trait]
impl GateAuthorizationResolver for DenyGateAuthorizationResolver {
    async fn resolve(
        &self,
        request: &GateAuthorizationRequest,
    ) -> Result<GateAuthorizationResolution> {
        let reason = if request.scope.task_id_opt().is_none() {
            "tier-3 gate requires a task-scoped context"
        } else {
            "tier-3 gate has no registered ingress or channel"
        };
        Ok(GateAuthorizationResolution::Deny {
            reason: reason.to_string(),
        })
    }
}

pub fn deny_gate_authorization_resolver() -> Arc<dyn GateAuthorizationResolver> {
    Arc::new(DenyGateAuthorizationResolver)
}
