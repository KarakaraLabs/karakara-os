// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed route identity: package name and route key parsing from A2A requests.
//!
//! The instance dimension was removed from routing. An agent route is the package name alone.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BamlRtError, Result};

fn is_valid_identifier(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Strongly-typed agent package identifier (e.g. manifest name).
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentPackageName(String);

impl AgentPackageName {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let raw = value.as_ref();
        if raw.trim().is_empty() || raw != raw.trim() || !is_valid_identifier(raw) {
            return None;
        }
        Some(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentPackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<str> for AgentPackageName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentPackageName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Route key for an agent: the package name alone.
/// Used in HTTP paths: `/agents/{agent_package}/...`
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentRouteKey {
    pub agent_package: AgentPackageName,
}

impl AgentRouteKey {
    pub fn new(agent_package: AgentPackageName) -> Self {
        Self { agent_package }
    }

    pub fn as_str(&self) -> &str {
        self.agent_package.as_str()
    }
}

/// Extract route key from a JSON-RPC A2A request (params.metadata.target from system/internal_a2a).
/// Centralizes the protocol so all consumers use the same parsing.
pub fn route_key_from_request(request: impl AsRef<Value>) -> Result<AgentRouteKey> {
    let request = request.as_ref();
    let params = request
        .get("params")
        .and_then(|p| p.as_object())
        .ok_or_else(|| BamlRtError::InvalidArgument("params must be an object".to_string()))?;
    let meta = params
        .get("metadata")
        .and_then(|m| m.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata required for routing".to_string())
        })?;
    let target = meta
        .get("target")
        .and_then(|t| t.as_object())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument("params.metadata.target required".to_string())
        })?;
    let agent_package_str = target
        .get("agent_package")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "params.metadata.target.agent_package required".to_string(),
            )
        })?;
    // The instance dimension is removed. Reject any request that still carries it, for every
    // value including the historical `"default"`, so no caller lands silently on a binding.
    if target.contains_key("agent_instance_id") {
        return Err(BamlRtError::UnsupportedAgentInstance(
            "params.metadata.target.agent_instance_id was removed; address agents by package only"
                .to_string(),
        ));
    }
    let agent_package = AgentPackageName::parse(agent_package_str).ok_or_else(|| {
        BamlRtError::InvalidArgument(format!("invalid agent_package '{agent_package_str}'"))
    })?;
    Ok(AgentRouteKey::new(agent_package))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentPackageName, route_key_from_request};
    use crate::{A2aWireRequest, AgentCard, AgentDiscoveryEntry, BamlRtError};

    #[test]
    fn package_name_parse_rejects_invalid_characters() {
        assert!(AgentPackageName::parse("coordinator-agent").is_some());
        assert!(AgentPackageName::parse("notion_agent").is_some());
        assert!(AgentPackageName::parse("bad/name").is_none());
        assert!(AgentPackageName::parse("bad name").is_none());
        assert!(AgentPackageName::parse(" coordinator-agent ").is_none());
        assert!(AgentPackageName::parse("   ").is_none());
    }

    /// The removed instance dimension must be rejected wherever a caller could still supply it,
    /// for every value including the historical `"default"`. Discovery output must not expose it.
    #[test]
    fn agent_instance_id_is_rejected_everywhere() {
        let base = json!({
            "params": {
                "metadata": {
                    "target": { "agent_package": "coordinator-agent" }
                }
            }
        });
        let key = route_key_from_request(A2aWireRequest::from(base.clone()))
            .expect("package-only target parses");
        assert_eq!(key.agent_package.as_str(), "coordinator-agent");

        for value in ["default", "staging", "anything"] {
            let mut request = base.clone();
            request["params"]["metadata"]["target"]["agent_instance_id"] = json!(value);
            match route_key_from_request(A2aWireRequest::from(request)) {
                Err(BamlRtError::UnsupportedAgentInstance(_)) => {}
                other => panic!("expected UnsupportedAgentInstance for {value:?}, got {other:?}"),
            }
        }

        // Discovery entries must not serialize the removed field.
        let entry = AgentDiscoveryEntry {
            agent_package: "coordinator-agent".to_string(),
            name: "coordinator-agent".to_string(),
            version: "1".to_string(),
            agent_card: AgentCard {
                name: "coordinator-agent".to_string(),
                version: "1".to_string(),
                content_hash: None,
                repository_version: None,
                agent_package: "coordinator-agent".to_string(),
                tools: vec![],
                baml_functions: vec![],
                description: None,
                capabilities: vec![],
                tags: vec![],
                subscriptions: vec![],
            },
        };
        let serialized = serde_json::to_string(&entry).expect("discovery entry serializes");
        assert!(
            !serialized.contains("agent_instance_id"),
            "discovery entry must not expose agent_instance_id: {serialized}"
        );
    }
}
