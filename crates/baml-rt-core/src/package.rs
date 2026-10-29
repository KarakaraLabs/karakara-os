// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent package manifest (manifest.json) — shared contract between builder and runner.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::EventSubscription;

fn default_entry_point() -> String {
    "dist/index.js".to_string()
}

/// Optional non-derivable card metadata for discovery (description, capabilities, subscriptions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestDiscovery {
    /// Description for discovery listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Capability labels for filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Event subscriptions this agent wants the host to deliver.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EventSubscription>,
}

/// Which prompt-context parts the host must build for one BAML function.
///
/// Declared by the agent author in `manifest.json`, never by agent code or the model, so a call
/// cannot widen its own context. Absent entries keep both parts, which is the historical behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationContextRequirements {
    /// Project conversation history into `ctx.tags['conversation_transcript']`.
    #[serde(default = "default_true")]
    pub conversation_history: bool,
    /// Inject `ctx.tags['tool_schema_prelude']` (the rendered tool/operation catalog).
    #[serde(default = "default_true")]
    pub tool_schema_prelude: bool,
}

fn default_true() -> bool {
    true
}

impl Default for InvocationContextRequirements {
    fn default() -> Self {
        Self {
            conversation_history: true,
            tool_schema_prelude: true,
        }
    }
}

impl InvocationContextRequirements {
    /// Requirements for a function that neither reads prior turns nor can emit tool operations,
    /// such as a formatter that only restates already-validated structured input.
    pub const SUBMISSION_ONLY: Self = Self {
        conversation_history: false,
        tool_schema_prelude: false,
    };
}

/// Canonical agent package manifest (manifest.json).
/// Single source of truth for the package contract between baml-rt-builder and baml-agent-runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub version: String,
    pub name: String,
    #[serde(default = "default_entry_point")]
    pub entry_point: String,
    /// Legacy archive label. It is optional and does not identify package contents.
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional discovery card metadata (non-derivable fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<ManifestDiscovery>,
    /// Per-function prompt-context opt-outs. Keyed by exact BAML function name (for step-executor
    /// phase prompts, the generated phase name such as `Foo__entry`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub function_context: BTreeMap<String, InvocationContextRequirements>,
}

#[cfg(test)]
mod tests {
    use super::{AgentManifest, InvocationContextRequirements};

    fn manifest_with(extra: &str) -> AgentManifest {
        let json = format!(r#"{{"version":"1.0.0","name":"agent"{extra}}}"#);
        serde_json::from_str(&json).expect("manifest parses")
    }

    #[test]
    fn function_context_is_optional_and_defaults_to_building_everything() {
        let manifest = manifest_with("");
        assert!(manifest.function_context.is_empty());
        let requirements = manifest
            .function_context
            .get("AnyFunction")
            .copied()
            .unwrap_or_default();
        assert!(requirements.conversation_history);
        assert!(requirements.tool_schema_prelude);
    }

    #[test]
    fn partial_function_context_entry_keeps_unspecified_parts() {
        let manifest = manifest_with(
            r#","function_context":{"PureFormatter":{"conversation_history":false}}"#,
        );
        let requirements = manifest.function_context["PureFormatter"];
        assert!(!requirements.conversation_history);
        assert!(
            requirements.tool_schema_prelude,
            "unspecified parts must keep their default"
        );
    }

    #[test]
    fn submission_only_disables_both_parts() {
        let requirements = InvocationContextRequirements::SUBMISSION_ONLY;
        assert!(!requirements.conversation_history);
        assert!(!requirements.tool_schema_prelude);
    }
}
