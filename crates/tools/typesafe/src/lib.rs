// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! TypeSafe System One tool contracts and registration.
//!
//! This crate provides the production bundle, configuration, metadata, wire DTOs, and local
//! request and provider-response validation. HTTP execution, credentials, and call controls remain
//! to be implemented.

use std::collections::BTreeMap;

mod error;
pub(crate) mod validation;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::{JsonSchemaType, TsType};
use baml_rt_core::{BamlRtError, ClassifiedToolError, Result as BamlResult};
use baml_rt_tools::{
    ActionIdentity, BamlTool, BundleType, DescribeAction, OpaqueJson, ToolSessionContext, baml_tool,
};
pub use error::SystemOneError;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Value, json};
use validation::{
    INPUT_EXPECTED_REQUEST, classify_input_deserialization_error, deserialize_input_model,
    deserialize_input_questions, deserialize_input_state, deserialize_input_timeout,
    validate_system_one_request,
};

pub const DEFAULT_MODEL: &str = "jev-1.13.0";
const SYSTEM_ONE_DESCRIPTION: &str = "Ask a TypeSafe System One model independently evaluated Choice, Score, and Noul questions about supplied JSON state.";

fn tagged_variant_schema<T: JsonSchemaType>(tag_value: &str) -> Value {
    let mut schema = match T::json_schema_inline() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    schema.insert("type".to_string(), Value::String("object".to_string()));

    let mut properties = match schema.remove("properties") {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    properties.insert(
        "type".to_string(),
        json!({ "type": "string", "const": tag_value }),
    );
    schema.insert("properties".to_string(), Value::Object(properties));

    let mut required = match schema.remove("required") {
        Some(Value::Array(values)) => values,
        _ => Vec::new(),
    };
    let type_field = Value::String("type".to_string());
    if !required.contains(&type_field) {
        required.push(type_field);
    }
    schema.insert("required".to_string(), Value::Array(required));
    schema.insert("additionalProperties".to_string(), Value::Bool(false));
    Value::Object(schema)
}

fn tagged_union_ts_decl(type_name: &str, variants: &[(&str, &str)]) -> String {
    let union = variants
        .iter()
        .map(|(tag, payload)| format!("({{ type: \"{tag}\" }} & {payload})"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("export type {type_name} = {union};")
}

fn optional_schema(inner: Value) -> Value {
    json!({ "anyOf": [inner, { "type": "null" }] })
}

/// Operator limits applied to one System One request.
///
/// Request counts and byte limits use `u32`; every documented provider and local maximum fits in
/// that range. Configuration validation rejects zero and provider-incompatible maxima before use.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SystemOneLimits {
    pub max_body_bytes: u32,
    pub max_questions: u32,
    pub max_choice_options: u32,
    pub max_score_levels: u32,
}

impl Default for SystemOneLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 262_144,
            max_questions: 64,
            max_choice_options: 255,
            max_score_levels: 10,
        }
    }
}

/// Process-local load and usage controls for System One.
///
/// Concurrency and request counts use `u32`. Token accounting stays `u64` because it accumulates
/// usage over an hour rather than sizing one request.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SystemOneControls {
    pub max_concurrent_requests: u32,
    pub requests_per_minute: u32,
    pub max_tokens_per_hour: Option<u64>,
}

impl Default for SystemOneControls {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 16,
            requests_per_minute: 600,
            max_tokens_per_hour: None,
        }
    }
}

/// Non-secret configuration shared by the `typesafe` bundle.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SystemOneConfig {
    pub enabled: bool,
    pub default_model: String,
    pub allowed_models: Vec<String>,
    pub timeout_ms: u64,
    pub max_attempts: u32,
    pub limits: SystemOneLimits,
    pub controls: SystemOneControls,
}

impl Default for SystemOneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_model: DEFAULT_MODEL.to_string(),
            allowed_models: vec![DEFAULT_MODEL.to_string()],
            timeout_ms: 10_000,
            max_attempts: 3,
            limits: SystemOneLimits::default(),
            controls: SystemOneControls::default(),
        }
    }
}

impl SystemOneConfig {
    fn is_valid(&self) -> bool {
        !self.default_model.trim().is_empty()
            && !self.allowed_models.is_empty()
            && self
                .allowed_models
                .iter()
                .all(|model| !model.trim().is_empty())
            && self.allowed_models.contains(&self.default_model)
            && self.timeout_ms > 0
            && self.max_attempts > 0
            && self.limits.max_body_bytes > 0
            && self.limits.max_questions > 0
            && (1..=255).contains(&self.limits.max_choice_options)
            && (2..=10).contains(&self.limits.max_score_levels)
            && self.controls.max_concurrent_requests > 0
            && self.controls.requests_per_minute > 0
    }
}

/// Bundle marker for TypeSafe-hosted tools.
pub struct TypeSafe;

impl BundleType for TypeSafe {
    const NAME: &'static str = "typesafe";

    fn description() -> &'static str {
        "TypeSafe hosted model tools."
    }
}

/// One System One request. Question IDs are the keys in `questions`.
#[derive(Debug, Clone, Serialize)]
pub struct SystemOneInput {
    pub state: OpaqueJson,
    pub questions: BTreeMap<String, SystemOneQuestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemOneInputWire {
    #[serde(deserialize_with = "deserialize_input_state")]
    state: OpaqueJson,
    #[serde(deserialize_with = "deserialize_input_questions")]
    questions: BTreeMap<String, SystemOneQuestion>,
    #[serde(default, deserialize_with = "deserialize_input_model")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_input_timeout")]
    timeout_ms: Option<u64>,
}

impl<'de> Deserialize<'de> for SystemOneInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        SystemOneInputWire::deserialize(deserializer)
            .map(|wire| Self {
                state: wire.state,
                questions: wire.questions,
                model: wire.model,
                timeout_ms: wire.timeout_ms,
            })
            .map_err(|error| {
                let message = match classify_input_deserialization_error(&error.to_string()) {
                    Some(message) => message,
                    None => INPUT_EXPECTED_REQUEST,
                };
                de::Error::custom(message)
            })
    }
}

impl DescribeAction for SystemOneInput {
    fn describe(&self) -> String {
        describe_request(self)
    }
}

impl TsType for SystemOneInput {
    fn ts_type_name() -> &'static str {
        "SystemOneInput"
    }

    fn ts_decl() -> Option<String> {
        Some(
            "export interface SystemOneInput { state: OpaqueJson; questions: Record<string, SystemOneQuestion>; model?: string | null; timeout_ms?: number | null; }"
                .to_string(),
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["OpaqueJson", "SystemOneQuestion"]
    }
}

impl JsonSchemaType for SystemOneInput {
    fn json_schema_inline() -> Value {
        json!({
            "type": "object",
            "properties": {
                "state": OpaqueJson::json_schema_inline(),
                "questions": {
                    "type": "object",
                    "additionalProperties": SystemOneQuestion::json_schema_inline()
                },
                "model": optional_schema(json!({ "type": "string" })),
                "timeout_ms": optional_schema(json!({ "type": "integer", "minimum": 0 }))
            },
            "required": ["state", "questions"],
            "additionalProperties": false
        })
    }
}

/// A question evaluated independently against the request state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SystemOneQuestion {
    Choice(ChoiceQuestion),
    Score(ScoreQuestion),
    Noul(NoulQuestion),
}

impl SystemOneQuestion {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Choice(_) => "choice",
            Self::Score(_) => "score",
            Self::Noul(_) => "noul",
        }
    }
}

impl TsType for SystemOneQuestion {
    fn ts_type_name() -> &'static str {
        "SystemOneQuestion"
    }

    fn ts_decl() -> Option<String> {
        Some(tagged_union_ts_decl(
            Self::ts_type_name(),
            &[
                ("choice", "ChoiceQuestion"),
                ("score", "ScoreQuestion"),
                ("noul", "NoulQuestion"),
            ],
        ))
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["ChoiceQuestion", "ScoreQuestion", "NoulQuestion"]
    }
}

impl JsonSchemaType for SystemOneQuestion {
    fn json_schema_inline() -> Value {
        json!({
            "oneOf": [
                tagged_variant_schema::<ChoiceQuestion>("choice"),
                tagged_variant_schema::<ScoreQuestion>("score"),
                tagged_variant_schema::<NoulQuestion>("noul")
            ]
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ChoiceQuestion {
    pub instructions: OpaqueJson,
    pub criteria: BTreeMap<String, OpaqueJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ScoreQuestion {
    pub instructions: OpaqueJson,
    pub criteria: Vec<OpaqueJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulQuestion {
    pub instructions: OpaqueJson,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<NoulCriteria>,
}

impl TsType for NoulQuestion {
    fn ts_type_name() -> &'static str {
        "NoulQuestion"
    }

    fn ts_decl() -> Option<String> {
        Some(
            "export interface NoulQuestion { instructions: OpaqueJson; criteria?: NoulCriteria | null; }"
                .to_string(),
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["OpaqueJson", "NoulCriteria"]
    }
}

impl JsonSchemaType for NoulQuestion {
    fn json_schema_inline() -> Value {
        json!({
            "type": "object",
            "properties": {
                "instructions": OpaqueJson::json_schema_inline(),
                "criteria": optional_schema(NoulCriteria::json_schema_inline())
            },
            "required": ["instructions"],
            "additionalProperties": false
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub true_value: OpaqueJson,
    #[serde(rename = "false")]
    pub false_value: OpaqueJson,
}

impl TsType for NoulCriteria {
    fn ts_type_name() -> &'static str {
        "NoulCriteria"
    }

    fn ts_decl() -> Option<String> {
        Some("export interface NoulCriteria { true: OpaqueJson; false: OpaqueJson; }".to_string())
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["OpaqueJson"]
    }
}

impl JsonSchemaType for NoulCriteria {
    fn json_schema_inline() -> Value {
        json!({
            "type": "object",
            "properties": {
                "true": OpaqueJson::json_schema_inline(),
                "false": OpaqueJson::json_schema_inline()
            },
            "required": ["true", "false"],
            "additionalProperties": false
        })
    }
}

/// Validated System One response returned to agent code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOneOutput {
    pub model: String,
    pub answers: BTreeMap<String, SystemOneAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SystemOneUsage>,
}

impl TsType for SystemOneOutput {
    fn ts_type_name() -> &'static str {
        "SystemOneOutput"
    }

    fn ts_decl() -> Option<String> {
        Some(
            "export interface SystemOneOutput { model: string; answers: Record<string, SystemOneAnswer>; usage?: SystemOneUsage | null; }"
                .to_string(),
        )
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["SystemOneAnswer", "SystemOneUsage"]
    }
}

impl JsonSchemaType for SystemOneOutput {
    fn json_schema_inline() -> Value {
        json!({
            "type": "object",
            "properties": {
                "model": { "type": "string" },
                "answers": {
                    "type": "object",
                    "additionalProperties": SystemOneAnswer::json_schema_inline()
                },
                "usage": optional_schema(SystemOneUsage::json_schema_inline())
            },
            "required": ["model", "answers"],
            "additionalProperties": false
        })
    }
}

/// A typed answer keyed by the corresponding question ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SystemOneAnswer {
    Choice(ChoiceAnswer),
    Score(ScoreAnswer),
    Noul(NoulAnswer),
}

impl SystemOneAnswer {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Choice(_) => "choice",
            Self::Score(_) => "score",
            Self::Noul(_) => "noul",
        }
    }
}

impl TsType for SystemOneAnswer {
    fn ts_type_name() -> &'static str {
        "SystemOneAnswer"
    }

    fn ts_decl() -> Option<String> {
        Some(tagged_union_ts_decl(
            Self::ts_type_name(),
            &[
                ("choice", "ChoiceAnswer"),
                ("score", "ScoreAnswer"),
                ("noul", "NoulAnswer"),
            ],
        ))
    }

    fn ts_dependencies() -> Vec<&'static str> {
        vec!["ChoiceAnswer", "ScoreAnswer", "NoulAnswer"]
    }
}

impl JsonSchemaType for SystemOneAnswer {
    fn json_schema_inline() -> Value {
        json!({
            "oneOf": [
                tagged_variant_schema::<ChoiceAnswer>("choice"),
                tagged_variant_schema::<ScoreAnswer>("score"),
                tagged_variant_schema::<NoulAnswer>("noul")
            ]
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ChoiceAnswer {
    pub choice: String,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ScoreAnswer {
    pub score: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct NoulAnswer {
    pub noul: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOneUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

impl TsType for SystemOneUsage {
    fn ts_type_name() -> &'static str {
        "SystemOneUsage"
    }

    fn ts_decl() -> Option<String> {
        Some(
            "export interface SystemOneUsage { input_tokens?: number | null; output_tokens?: number | null; }"
                .to_string(),
        )
    }
}

impl JsonSchemaType for SystemOneUsage {
    fn json_schema_inline() -> Value {
        json!({
            "type": "object",
            "properties": {
                "input_tokens": optional_schema(json!({ "type": "integer", "minimum": 0 })),
                "output_tokens": optional_schema(json!({ "type": "integer", "minimum": 0 }))
            },
            "additionalProperties": false
        })
    }
}

fn single_line_summary_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// Safe request summary used by transcript and archive projections.
pub fn describe_request(input: &SystemOneInput) -> String {
    let questions = input
        .questions
        .iter()
        .map(|(id, question)| format!("{}:{}", single_line_summary_value(id), question.kind()))
        .collect::<Vec<_>>()
        .join(", ");
    let model = single_line_summary_value(input.model.as_deref().unwrap_or("default"));
    format!(
        "typesafe/system_one: {} questions ({questions}) model={model}",
        input.questions.len()
    )
}

/// Safe archive identity. State, instructions, and criteria values are intentionally absent.
pub fn request_action_identity(input: &SystemOneInput) -> ActionIdentity {
    let questions = input
        .questions
        .iter()
        .map(|(id, question)| format!("{}:{}", single_line_summary_value(id), question.kind()))
        .collect::<Vec<_>>()
        .join(", ");
    ActionIdentity::new(
        Some("system_one"),
        vec![
            ("questions", json!(questions)),
            (
                "model",
                json!(single_line_summary_value(
                    input.model.as_deref().unwrap_or("default")
                )),
            ),
        ],
    )
}

/// Safe result summary for the archive fallback path.
pub fn describe_output(output: &SystemOneOutput) -> String {
    let answers = output
        .answers
        .iter()
        .map(|(id, answer)| format!("{}:{}", single_line_summary_value(id), answer.kind()))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "typesafe/system_one: {} answers ({answers}) model={}",
        output.answers.len(),
        single_line_summary_value(&output.model)
    )
}

fn config_for_context(ctx: &ToolSessionContext) -> BamlResult<SystemOneConfig> {
    let config = match &ctx.config {
        Some(config) => serde_json::from_value(config.clone()).map_err(|_| {
            SystemOneError::disabled("TypeSafe System One configuration is invalid")
        })?,
        None => SystemOneConfig::default(),
    };
    if !config.is_valid() {
        return Err(
            SystemOneError::disabled("TypeSafe System One configuration is invalid").into(),
        );
    }
    Ok(config)
}

/// Host adapter for `POST /v1/systemone`.
#[derive(Default)]
pub struct SystemOneTool;

#[baml_tool(
    name = "typesafe/system_one",
    description = "Ask a TypeSafe System One model independently evaluated Choice, Score, and Noul questions about supplied JSON state.",
    tags = ["typesafe", "system_one", "read"],
    access = Read,
    config = {
        bundle = "typesafe",
        schema = SystemOneConfig,
        default = SystemOneConfig::default(),
    },
    secrets = [
        { name = "TYPESAFE_API_KEY", description = "TypeSafe API key", reason = "Required to authenticate with the TypeSafe System One API" },
    ],
)]
#[async_trait]
impl BamlTool for SystemOneTool {
    type Bundle = TypeSafe;
    const LOCAL_NAME: &'static str = "system_one";
    type OpenInput = ();
    type Input = SystemOneInput;
    type Output = SystemOneOutput;

    fn description(&self) -> &'static str {
        SYSTEM_ONE_DESCRIPTION
    }

    fn describe_open(&self) -> String {
        "using TypeSafe System One for typed state evaluation".to_string()
    }

    fn describe_invocation(&self, input: &Self::Input) -> String {
        describe_request(input)
    }

    fn action_identity(&self, input: &Self::Input) -> Option<ActionIdentity> {
        Some(request_action_identity(input))
    }

    fn describe_result(&self, output: &Self::Output) -> String {
        describe_output(output)
    }

    async fn execute(
        &self,
        ctx: &ToolSessionContext,
        args: Self::Input,
    ) -> BamlResult<Self::Output> {
        let config = config_for_context(ctx)?;
        if !config.enabled {
            return Err(SystemOneError::disabled(
                "TypeSafe System One is disabled by operator configuration",
            )
            .into());
        }
        let _validated = validate_system_one_request(&config, &args)?;

        Err(SystemOneError::provider_unavailable(
            "TypeSafe System One provider execution is not available yet",
        )
        .into())
    }

    fn classify_execution_error(err: &BamlRtError) -> ClassifiedToolError {
        let BamlRtError::Json(source) = err else {
            return ClassifiedToolError::from_baml_error(err);
        };
        let raw = source.to_string();
        match classify_input_deserialization_error(&raw) {
            Some(message) => SystemOneError::invalid_argument(message).into_classified(),
            None => SystemOneError::provider_contract(
                "TypeSafe System One encountered invalid JSON outside tool input",
            )
            .into_classified(),
        }
    }
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod validation_tests;
