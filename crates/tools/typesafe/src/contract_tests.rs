// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{
    BamlRtError, ErrorDisposition,
    context::{InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_tools::{
    BamlTool, BundleName, ToolName, ToolRegistry, ToolSecretResolver, ToolSecretValue,
    ToolSessionContext, ToolStep,
    archive_refs::{ContextRefTables, RefTable},
    bundles::BundleType,
    config_resolver::ConfigResolver,
    prompt_projection::{
        PromptProjectionContent, PromptProjectionItem, format_conversation_history_transcript,
        project_prompt_context,
    },
    tool_schema::{ActionIdentity, DescribeAction, json_schema_value},
    tools::{ToolAccess, test_support::tool_session_context},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::*;

static TYPESAFE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct StaticConfigResolver(Value);

#[async_trait]
impl ConfigResolver for StaticConfigResolver {
    async fn get_config(&self, _bundle_name: &BundleName) -> baml_rt_core::Result<Option<Value>> {
        Ok(Some(self.0.clone()))
    }

    async fn get_config_with_version(
        &self,
        _bundle_name: &BundleName,
    ) -> baml_rt_core::Result<Option<(Value, u64)>> {
        Ok(Some((self.0.clone(), 7)))
    }
}

struct MemoryToolSecretResolver;

impl ToolSecretResolver for MemoryToolSecretResolver {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == TYPESAFE_API_KEY).then(|| ToolSecretValue::new("test-typesafe-key"))
    }
}

struct EmptyMemoryToolSecretResolver;

impl ToolSecretResolver for EmptyMemoryToolSecretResolver {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == TYPESAFE_API_KEY).then(|| ToolSecretValue::new(""))
    }
}

struct SentinelSecretResolver;

impl ToolSecretResolver for SentinelSecretResolver {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == TYPESAFE_API_KEY).then(|| ToolSecretValue::new(SECRET_SENTINEL))
    }
}

const SECRET_SENTINEL: &str = "SECRET_SENTINEL_12345";

struct HistoryContractSpikeTool;

#[async_trait]
impl BamlTool for HistoryContractSpikeTool {
    type Bundle = TypeSafe;
    const LOCAL_NAME: &'static str = "system_one";
    type OpenInput = ();
    type Input = SystemOneInput;
    type Output = SystemOneOutput;

    fn description(&self) -> &'static str {
        "Successful contract spike for the step-executor archive path"
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
        _ctx: &ToolSessionContext,
        _args: Self::Input,
    ) -> baml_rt_core::Result<Self::Output> {
        Ok(sample_output())
    }
}

fn sample_input() -> SystemOneInput {
    SystemOneInput {
        state: OpaqueJson::from(json!({
            "alert": "STATE_SENTINEL",
            "power_w": 50
        })),
        questions: BTreeMap::from([
            (
                "answers_request".to_string(),
                SystemOneQuestion::Score(ScoreQuestion {
                    instructions: OpaqueJson::from(json!("INSTRUCTIONS_SENTINEL")),
                    criteria: vec![
                        OpaqueJson::from(json!("CRITERIA_SENTINEL")),
                        OpaqueJson::from(json!("complete")),
                    ],
                }),
            ),
            (
                "has_baseline".to_string(),
                SystemOneQuestion::Noul(NoulQuestion {
                    instructions: OpaqueJson::from(json!(
                        "Does the alert include a comparison baseline?"
                    )),
                    criteria: None,
                }),
            ),
            (
                "next_agent".to_string(),
                SystemOneQuestion::Choice(ChoiceQuestion {
                    instructions: OpaqueJson::from(json!("Choose the first investigator")),
                    criteria: BTreeMap::from([
                        (
                            "grafana-investigator".to_string(),
                            OpaqueJson::from(json!("Telemetry diagnosis")),
                        ),
                        ("none".to_string(), OpaqueJson::from(Value::Null)),
                    ]),
                }),
            ),
        ]),
        model: Some(DEFAULT_MODEL.to_string()),
        timeout_ms: Some(1_000),
    }
}

fn sample_output() -> SystemOneOutput {
    SystemOneOutput {
        model: DEFAULT_MODEL.to_string(),
        answers: BTreeMap::from([
            (
                "answers_request".to_string(),
                SystemOneAnswer::Score(ScoreAnswer {
                    score: 0.8,
                    probabilities: BTreeMap::from([("0".to_string(), 0.2), ("1".to_string(), 0.8)]),
                    confidence: 0.6,
                }),
            ),
            (
                "has_baseline".to_string(),
                SystemOneAnswer::Noul(NoulAnswer { noul: 0.1 }),
            ),
            (
                "next_agent".to_string(),
                SystemOneAnswer::Choice(ChoiceAnswer {
                    choice: "grafana-investigator".to_string(),
                    probabilities: BTreeMap::from([
                        ("grafana-investigator".to_string(), 0.9),
                        ("none".to_string(), 0.1),
                    ]),
                    confidence: 0.8,
                }),
            ),
        ]),
        usage: Some(SystemOneUsage {
            input_tokens: Some(123),
            output_tokens: Some(12),
        }),
    }
}

#[test]
fn production_bundle_config_and_metadata_match_the_contract() {
    assert_eq!(TypeSafe::NAME, "typesafe");

    let default = SystemOneConfig::default();
    assert!(!default.enabled);
    assert_eq!(default.default_model, DEFAULT_MODEL);
    assert_eq!(default.allowed_models, vec![DEFAULT_MODEL.to_string()]);
    assert_eq!(default.timeout_ms, 10_000);
    assert_eq!(default.max_attempts, 3);
    assert_eq!(default.limits, SystemOneLimits::default());
    assert_eq!(default.controls, SystemOneControls::default());

    let metadata = typesafe_system_one_metadata();
    assert_eq!(metadata.name.to_string(), "typesafe/system_one");
    assert_eq!(metadata.access, Some(ToolAccess::Read));
    assert_eq!(metadata.description, SystemOneTool.description());
    assert_eq!(
        metadata
            .config_bundle
            .as_ref()
            .expect("config bundle")
            .as_str(),
        "typesafe"
    );
    let config = metadata.config.as_ref().expect("config metadata");
    assert_eq!(
        config.default,
        serde_json::to_value(SystemOneConfig::default()).expect("serialize default config")
    );
    assert_eq!(config.type_name.as_deref(), Some("SystemOneConfig"));
    assert!(
        config.validator.is_some(),
        "typed config validator must be registered"
    );
    assert_eq!(metadata.secret_requests.len(), 1);
    assert_eq!(metadata.secret_requests[0].name, "TYPESAFE_API_KEY");
}

#[test]
fn config_defaults_missing_fields_and_rejects_unknown_fields() {
    let config: SystemOneConfig = serde_json::from_value(json!({
        "enabled": true,
        "limits": {"max_questions": 12},
        "controls": {"max_concurrent_requests": 4}
    }))
    .expect("missing fields must use safe defaults");
    assert!(config.enabled);
    assert_eq!(config.default_model, DEFAULT_MODEL);
    assert_eq!(config.limits.max_questions, 12);
    assert_eq!(config.limits.max_body_bytes, 262_144);
    assert_eq!(config.controls.max_concurrent_requests, 4);
    assert!(config.validate().is_ok());

    let mut invalid = config.clone();
    invalid.max_attempts = 0;
    assert_eq!(invalid.validate(), Err("max_attempts"));
    invalid = config.clone();
    invalid.limits.max_choice_options = 256;
    assert_eq!(invalid.validate(), Err("limits.max_choice_options"));

    assert!(
        serde_json::from_value::<SystemOneConfig>(json!({"future_field": true})).is_err(),
        "unknown top-level fields must be rejected"
    );
    assert!(
        serde_json::from_value::<SystemOneConfig>(json!({
            "limits": {"future_limit": 1}
        }))
        .is_err(),
        "unknown nested fields must be rejected"
    );
}

#[test]
fn typed_config_validator_normalizes_and_caches_valid_versions() {
    let raw = json!({
        "enabled": true,
        "default_model": DEFAULT_MODEL,
        "allowed_models": [DEFAULT_MODEL],
        "timeout_ms": 5000,
        "max_attempts": 2,
        "limits": {
            "max_body_bytes": 4096,
            "max_questions": 8,
            "max_choice_options": 32,
            "max_score_levels": 5
        },
        "controls": {
            "max_concurrent_requests": 2,
            "max_tokens_per_hour": null
        }
    });

    let normalized = validate_system_one_config(raw.clone()).expect("valid config");
    assert_eq!(normalized, raw);
    let first = cached_config(raw.clone()).expect("first cache lookup");
    let second = cached_config(raw).expect("second cache lookup");
    assert!(Arc::ptr_eq(&first, &second));

    let mut invalid = normalized;
    invalid["default_model"] = json!("jev-not-allowed");
    assert_eq!(
        validate_system_one_config(invalid),
        Err("invalid configuration field: default_model".to_string())
    );

    let malformed = json!({ "future_field": true });
    let error = validate_system_one_config(malformed).expect_err("typed parse must fail");
    assert!(
        error.contains("future_field"),
        "the PUT error must name the failing field: {error}"
    );
}

#[tokio::test]
async fn production_tool_fails_closed_with_default_config() {
    let tool_name = ToolName::parse("typesafe/system_one").expect("static tool name");
    let ctx = tool_session_context(&tool_name);
    let err = SystemOneTool
        .execute(&ctx, sample_input())
        .await
        .expect_err("the default config must disable provider calls");

    let BamlRtError::ToolClassified(classified) = err else {
        panic!("expected a classified disabled error")
    };
    assert_eq!(classified.code, "typesafe_disabled");
    assert_eq!(classified.disposition, ErrorDisposition::InformAndContinue);
}

#[tokio::test]
async fn production_tool_uses_scoped_runner_secret_and_maps_unauthorized() {
    use axum::{Router, http::StatusCode, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let seen_authorization = Arc::new(Mutex::new(None));
    let seen_authorization_for_handler = Arc::clone(&seen_authorization);
    let app = Router::new().route(
        "/v1/systemone",
        post(move |headers: axum::http::HeaderMap| {
            let seen_authorization = Arc::clone(&seen_authorization_for_handler);
            async move {
                *seen_authorization.lock().expect("authorization lock") = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                StatusCode::UNAUTHORIZED
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 1_000,
        ..SystemOneConfig::default()
    };
    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));
    registry.set_secret_resolver(Some(Arc::new(MemoryToolSecretResolver)));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(sample_input()).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read error envelope");
    let ToolStep::Error { error } = step else {
        panic!("expected unauthorized tool error")
    };
    assert_eq!(
        error.classified.code, "typesafe_unauthorized",
        "{}",
        error.classified.message
    );
    assert_eq!(
        error.classified.disposition,
        ErrorDisposition::InformAndContinue
    );
    assert_eq!(
        seen_authorization
            .lock()
            .expect("authorization lock")
            .as_deref(),
        Some("Bearer test-typesafe-key")
    );
    server.abort();
}

#[tokio::test]
async fn production_tool_reports_missing_runner_credential_before_network_io() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_handler = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/systemone",
        post(move || {
            let requests = Arc::clone(&requests_for_handler);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::OK
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(sample_input()).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read error envelope");
    let ToolStep::Error { error } = step else {
        panic!("expected missing credential tool error")
    };
    assert_eq!(error.classified.code, "typesafe_missing_credential");
    assert_eq!(
        error.classified.disposition,
        ErrorDisposition::InformAndContinue
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "a missing credential must fail before any network request"
    );
    server.abort();
}

#[tokio::test]
async fn oversized_request_fails_validation_before_call_controls() {
    let config = SystemOneConfig {
        enabled: true,
        limits: SystemOneLimits {
            max_body_bytes: 10,
            ..SystemOneLimits::default()
        },
        ..SystemOneConfig::default()
    };
    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(sample_input()).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read error envelope");
    let ToolStep::Error { error } = step else {
        panic!("expected invalid argument tool error")
    };
    assert_eq!(
        error.classified.code, "invalid_argument",
        "validation must run before call controls, credential resolution, and network I/O"
    );
    assert_eq!(
        error.classified.disposition,
        ErrorDisposition::LlmCorrectable
    );
}

#[test]
fn schema_covers_all_question_and_answer_variants() {
    let schema = json!({
        "input": json_schema_value::<SystemOneInput>(),
        "question": json_schema_value::<SystemOneQuestion>(),
        "output": json_schema_value::<SystemOneOutput>(),
        "answer": json_schema_value::<SystemOneAnswer>(),
    });
    let schema_text = serde_json::to_string(&schema).expect("serialize schemas");
    for tag in ["choice", "score", "noul"] {
        assert!(
            schema_text.contains(&format!("\"const\":\"{tag}\"")),
            "schema must contain the {tag} variant"
        );
    }
    insta::assert_json_snapshot!("system_one_json_schema", schema);
}

#[test]
fn runtime_deserializes_raw_and_wrapped_opaque_json() {
    let raw: SystemOneInput = serde_json::from_value(json!({
        "state": {"asset": "pump-12", "samples": [1, 2, 3]},
        "questions": {
            "route": {
                "type": "choice",
                "instructions": {"task": "pick", "strict": true},
                "criteria": {"grafana": null, "none": "abstain"}
            },
            "quality": {
                "type": "score",
                "instructions": ["score", "the", "draft"],
                "criteria": [null, {"label": "complete"}]
            },
            "safe": {
                "type": "noul",
                "instructions": "Is it safe?",
                "criteria": {"true": ["yes"], "false": {"reason": "no"}}
            }
        }
    }))
    .expect("raw JSON values must deserialize");
    assert_eq!(raw.state.as_value()["asset"], "pump-12");
    let SystemOneQuestion::Choice(choice) = &raw.questions["route"] else {
        panic!("route must be Choice")
    };
    assert_eq!(choice.instructions.as_value()["task"], "pick");
    assert_eq!(choice.criteria["grafana"].as_value(), &Value::Null);

    let wrapped: SystemOneInput = serde_json::from_value(json!({
        "state": {"__baml_opaque_json": "{\"asset\":\"pump-12\"}"},
        "questions": {
            "safe": {
                "type": "noul",
                "instructions": {"__baml_opaque_json": "\"Is it safe?\""},
                "criteria": {
                    "true": {"__baml_opaque_json": "{\"label\":\"yes\"}"},
                    "false": {"__baml_opaque_json": "null"}
                }
            }
        }
    }))
    .expect("OpaqueJson wrappers must deserialize");
    assert_eq!(wrapped.state.as_value()["asset"], "pump-12");
    let SystemOneQuestion::Noul(noul) = &wrapped.questions["safe"] else {
        panic!("safe must be Noul")
    };
    assert_eq!(noul.instructions.as_value(), &json!("Is it safe?"));
    let criteria = noul.criteria.as_ref().expect("criteria");
    assert_eq!(criteria.true_value.as_value()["label"], "yes");
    assert_eq!(criteria.false_value.as_value(), &Value::Null);
}

#[tokio::test]
async fn imperative_quickjs_path_returns_production_disabled_envelope() {
    let manager = Arc::new(RwLock::new(
        test_support::common::setup_baml_runtime_manager_no_llm(),
    ));
    manager
        .write()
        .await
        .register_tool(SystemOneTool)
        .await
        .expect("register production tool");
    let mut bridge = test_support::common::setup_bridge(manager).await;
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000071").unwrap(),
    ));
    let input = serde_json::to_string(&sample_input()).expect("serialize input");
    let script = format!(
        r#"
        (async () => {{
            const session = await openToolSession("typesafe/system_one");
            await session.send({input});
            const step = await session.continue();
            await session.abort("expected disabled-tool failure");
            return step;
        }})()
        "#
    );

    let envelope = bridge
        .eval_scoped(&scope, &script)
        .await
        .expect("QuickJS call must resolve, not reject");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "typesafe_disabled");
    assert_eq!(envelope["error"]["disposition"], "inform_and_continue");
    assert_eq!(envelope["error"]["retryable"], false);
    assert!(envelope["error"]["retry_after_ms"].is_null());
    insta::assert_json_snapshot!("imperative_error_envelope", envelope);
}

#[tokio::test]
async fn invoke_tool_bridge_returns_one_shot_error_envelope() {
    let manager = Arc::new(RwLock::new(
        test_support::common::setup_baml_runtime_manager_no_llm(),
    ));
    manager
        .write()
        .await
        .register_tool(SystemOneTool)
        .await
        .expect("register production tool");
    let mut bridge = test_support::common::setup_bridge(manager).await;
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000071").unwrap(),
    ));
    let input = serde_json::to_string(&sample_input()).expect("serialize input");
    let script = format!(
        r#"
        (async () => {{
            return await invokeTool("typesafe/system_one", {input});
        }})()
        "#
    );

    let envelope = bridge
        .eval_scoped(&scope, &script)
        .await
        .expect("QuickJS call must resolve, not reject");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "typesafe_disabled");
    assert_eq!(envelope["error"]["disposition"], "inform_and_continue");
    assert_eq!(envelope["error"]["retryable"], false);
    assert!(envelope["error"]["retry_after_ms"].is_null());
    insta::assert_json_snapshot!("invoke_tool_disabled_envelope", envelope);
}

#[tokio::test]
async fn invoke_tool_bridge_malformed_input_does_not_echo_sentinel() {
    let manager = Arc::new(RwLock::new(
        test_support::common::setup_baml_runtime_manager_no_llm(),
    ));
    manager
        .write()
        .await
        .register_tool(SystemOneTool)
        .await
        .expect("register production tool");
    let mut bridge = test_support::common::setup_bridge(manager).await;
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000071").unwrap(),
    ));
    let script = r#"
        (async () => {
            return await invokeTool("typesafe/system_one", {
                state: "state",
                questions: {
                    severity: {
                        type: "score",
                        instructions: "score it",
                        criteria: "MALFORMED_CRITERIA_SENTINEL"
                    }
                }
            });
        })()
    "#;

    let envelope = bridge
        .eval_scoped(&scope, script)
        .await
        .expect("QuickJS call must resolve, not reject");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "invalid_argument");
    assert_eq!(envelope["error"]["disposition"], "llm_correctable");
    let rendered = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(rendered.contains("questions.*.criteria"));
    assert!(!rendered.contains("MALFORMED_CRITERIA_SENTINEL"));
}

#[tokio::test]
async fn invoke_tool_egresses_only_contract_and_attributes_scope() {
    use axum::{Router, routing::post};
    use baml_rt_core::bus::{BusWithEffects, EffectEmitter, EffectLiveness};
    use baml_rt_provenance::{
        ProvenanceEffectSubscriber, ProvenanceOpsFilters, ProvenanceOpsQuery,
        ProvenanceOpsQueryRequest, ProvenanceOpsResource, SurrealStoreBuilder,
    };
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);

    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_for_handler = Arc::clone(&bodies);
    let app = Router::new().route(
        "/v1/systemone",
        post(move |body: axum::extract::Json<Value>| {
            let bodies = Arc::clone(&bodies_for_handler);
            async move {
                bodies.lock().expect("bodies lock").push(body.0);
                (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "model": DEFAULT_MODEL,
                        "answers": {"safe": {"type": "noul", "noul": 0.5}}
                    })),
                )
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });

    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");
    let effect_bus = Arc::new(BusWithEffects::new());
    effect_bus
        .subscribe_effect_subscriber(Arc::new(ProvenanceEffectSubscriber::new(store.clone())))
        .await;

    let mut manager = test_support::common::setup_baml_runtime_manager_no_llm();
    manager
        .tool_registry()
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    manager
        .tool_registry()
        .set_config_resolver(Some(Arc::new(StaticConfigResolver(
            serde_json::to_value(SystemOneConfig {
                enabled: true,
                ..SystemOneConfig::default()
            })
            .expect("serialize config"),
        ))));
    manager
        .tool_registry()
        .set_secret_resolver(Some(Arc::new(MemoryToolSecretResolver)));
    manager.set_effect_emitter(effect_bus.clone() as Arc<dyn EffectEmitter>);

    let manager = Arc::new(RwLock::new(manager));
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let mut bridge = baml_rt_quickjs::QuickJSBridge::new(manager, agent_id.clone())
        .await
        .expect("quickjs bridge");
    bridge.set_effect_liveness(effect_bus.clone() as Arc<dyn EffectLiveness>);
    bridge
        .register_baml_functions()
        .await
        .expect("register helpers");

    let input = serde_json::to_string(&retry_input()).expect("serialize input");
    let script = format!(
        r#"(async () => {{ return await invokeTool("typesafe/system_one", {input}); }})()"#
    );

    let context_id = ContextId::new(7, 1);
    for (message, task) in [("turn-1", "task-1"), ("turn-2", "task-2")] {
        let scope = InvocationScope::new(RuntimeScope::task_scope(
            context_id.clone(),
            agent_id.clone(),
            MessageId::from_external(ExternalId::new(message.to_string())),
            TaskId::from_external(ExternalId::new(task.to_string())),
        ));
        let envelope = bridge
            .eval_scoped(&scope, &script)
            .await
            .expect("resolve one-shot call");
        assert_eq!(envelope["status"], "done", "{envelope}");
        assert_eq!(envelope["result"]["answers"]["safe"]["noul"], 0.5);
    }

    // Egress: the mock provider saw exactly the System One contract.
    let captured: Vec<Value> = bodies.lock().expect("bodies lock").clone();
    assert_eq!(captured.len(), 2);
    for body in captured.iter() {
        let keys = body
            .as_object()
            .expect("provider body must be an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(keys, ["state", "questions", "model"]);
        let rendered = body.to_string();
        for sentinel in [
            "test-typesafe-key",
            "00000000-0000-0000-0000-000000000072",
            "context_id",
            "agent_id",
            "task_id",
            "message_id",
        ] {
            assert!(
                !rendered.contains(sentinel),
                "provider body leaked {sentinel}: {rendered}"
            );
        }
    }

    // Attribution: both turns materialize tool calls under the same context,
    // agent, and tool (one Send hop plus one Read hop per turn).
    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::ToolCalls,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id),
                tool_name: Some("typesafe/system_one".to_string()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query_ops")
        .rows;
    assert_eq!(rows.len(), 4, "two turns × two hops: {rows:?}");
    for row in &rows {
        assert_eq!(row.context_id(), Some("ctx-7-1"));
        assert_eq!(
            row.get("agent_id").and_then(Value::as_str),
            Some("00000000-0000-0000-0000-000000000072")
        );
        assert_eq!(
            row.get("tool_name").and_then(Value::as_str),
            Some("typesafe/system_one")
        );
    }
    let task_one = rows
        .iter()
        .filter(|row| row.task_id() == Some("task-1"))
        .count();
    let task_two = rows
        .iter()
        .filter(|row| row.task_id() == Some("task-2"))
        .count();
    assert_eq!(
        (task_one, task_two),
        (2, 2),
        "each turn must be attributed to its own task"
    );

    server.abort();
}

#[tokio::test]
async fn invoke_tool_bridge_maps_transient_failure_to_error_envelope() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, http::StatusCode, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_handler = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/systemone",
        post(move || {
            let requests = Arc::clone(&requests_for_handler);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });

    let manager = test_support::common::setup_baml_runtime_manager_no_llm();
    manager
        .tool_registry()
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    manager
        .tool_registry()
        .set_config_resolver(Some(Arc::new(StaticConfigResolver(
            serde_json::to_value(SystemOneConfig {
                enabled: true,
                max_attempts: 1,
                ..SystemOneConfig::default()
            })
            .expect("serialize config"),
        ))));
    manager
        .tool_registry()
        .set_secret_resolver(Some(Arc::new(MemoryToolSecretResolver)));

    let manager = Arc::new(RwLock::new(manager));
    let mut bridge = test_support::common::setup_bridge(manager).await;
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000071").unwrap(),
    ));
    let input = serde_json::to_string(&retry_input()).expect("serialize input");
    let script = format!(
        r#"(async () => {{ return await invokeTool("typesafe/system_one", {input}); }})()"#
    );

    let envelope = bridge
        .eval_scoped(&scope, &script)
        .await
        .expect("QuickJS call must resolve, not reject");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "typesafe_provider_unavailable");
    assert_eq!(envelope["error"]["retryable"], false);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "max_attempts=1 must not retry the 5xx"
    );
    server.abort();
}

#[tokio::test]
async fn malformed_input_envelope_does_not_echo_the_bad_value() {
    let manager = Arc::new(RwLock::new(
        test_support::common::setup_baml_runtime_manager_no_llm(),
    ));
    manager
        .write()
        .await
        .register_tool(SystemOneTool)
        .await
        .expect("register production tool");
    let mut bridge = test_support::common::setup_bridge(manager).await;
    let scope = InvocationScope::synthetic_message(AgentId::from_uuid(
        UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap(),
    ));
    let script = r#"
        (async () => {
            const session = await openToolSession("typesafe/system_one");
            await session.send({
                state: "state",
                questions: {
                    severity: {
                        type: "score",
                        instructions: "score it",
                        criteria: "MALFORMED_CRITERIA_SENTINEL"
                    }
                }
            });
            const step = await session.continue();
            await session.abort("expected malformed-input failure");
            return step;
        })()
    "#;

    let envelope = bridge
        .eval_scoped(&scope, script)
        .await
        .expect("QuickJS call must resolve, not reject");
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "invalid_argument");
    assert_eq!(envelope["error"]["disposition"], "llm_correctable");
    let rendered = serde_json::to_string(&envelope).expect("serialize envelope");
    assert!(rendered.contains("questions.*.criteria"));
    assert!(!rendered.contains("MALFORMED_CRITERIA_SENTINEL"));
}

#[tokio::test]
async fn wrapper_carries_descriptions_into_transcript_and_archive_header() {
    let registry = ToolRegistry::new();
    registry
        .register(SystemOneTool)
        .expect("register production tool");
    let input = sample_input();
    let output = sample_output();
    let input_json = serde_json::to_value(&input).expect("serialize input");
    let output_json = serde_json::to_value(&output).expect("serialize output");

    let production_tool = SystemOneTool;
    assert_eq!(
        production_tool.describe_invocation(&input),
        describe_request(&input)
    );
    let actual_identity = production_tool
        .action_identity(&input)
        .expect("production action identity");
    let expected_identity = request_action_identity(&input);
    assert_eq!(actual_identity.name, expected_identity.name);
    assert_eq!(actual_identity.fields, expected_identity.fields);
    assert_eq!(
        production_tool.describe_result(&output),
        describe_output(&output)
    );

    let direct = registry
        .describe_invocation_for(
            "typesafe/system_one",
            &json!({"op": "Send", "input": input_json.clone()}),
        )
        .expect("direct description");
    let wrapped = registry
        .describe_invocation_for(
            "typesafe/system_one",
            &json!({"step": {"op": "Send", "input": input_json.clone()}}),
        )
        .expect("wrapped description");
    assert_eq!(direct, wrapped);
    for expected in [
        "answers_request:score",
        "has_baseline:noul",
        "next_agent:choice",
        "model=jev-1.13.0",
    ] {
        assert!(direct.contains(expected), "missing {expected}: {direct}");
    }

    let result_summary = registry
        .describe_result_for("typesafe/system_one", &output_json)
        .expect("result summary");
    assert!(result_summary.contains("answers_request:score"));
    assert!(result_summary.contains("has_baseline:noul"));
    assert!(result_summary.contains("next_agent:choice"));

    let items = vec![
        PromptProjectionItem {
            timestamp_ms: 1,
            activity_anchor: "typesafe-contract-call".to_string(),
            role: "assistant".to_string(),
            content: PromptProjectionContent::ToolCall {
                tool_name: "typesafe/system_one".to_string(),
                args: json!({"op": "Send", "input": input_json.clone()}),
            },
        },
        PromptProjectionItem {
            timestamp_ms: 2,
            activity_anchor: "typesafe-contract-result".to_string(),
            role: "tool".to_string(),
            content: PromptProjectionContent::ToolResult {
                tool_name: "typesafe/system_one".to_string(),
                result: output_json.clone(),
            },
        },
    ];
    let history = project_prompt_context(items, &registry, &RefTable::new(), None);
    let transcript = format_conversation_history_transcript(
        history.as_array().expect("projected history array"),
    );
    for sentinel in [
        "STATE_SENTINEL",
        "INSTRUCTIONS_SENTINEL",
        "CRITERIA_SENTINEL",
    ] {
        assert!(
            !transcript.contains(sentinel),
            "transcript leaked {sentinel}"
        );
        assert!(!direct.contains(sentinel), "description leaked {sentinel}");
        assert!(
            !result_summary.contains(sentinel),
            "result summary leaked {sentinel}"
        );
    }
    insta::assert_snapshot!("imperative_history_transcript", transcript);

    // Archive identities are capped by the core renderer. Keep this compatibility case within the
    // 32-character value budget so every submitted ID and type remains visible in the header.
    let archive_input = SystemOneInput {
        state: OpaqueJson::from(json!({"sentinel": "STATE_SENTINEL"})),
        questions: BTreeMap::from([
            (
                "baseline".to_string(),
                SystemOneQuestion::Noul(NoulQuestion {
                    instructions: OpaqueJson::from(json!("INSTRUCTIONS_SENTINEL")),
                    criteria: None,
                }),
            ),
            (
                "route".to_string(),
                SystemOneQuestion::Choice(ChoiceQuestion {
                    instructions: OpaqueJson::from(json!("Choose")),
                    criteria: BTreeMap::from([(
                        "none".to_string(),
                        OpaqueJson::from(json!("CRITERIA_SENTINEL")),
                    )]),
                }),
            ),
        ]),
        model: Some(DEFAULT_MODEL.to_string()),
        timeout_ms: None,
    };
    let archive_input_json = serde_json::to_value(archive_input).expect("serialize archive input");
    let mut manager = test_support::common::setup_baml_runtime_manager_no_llm();
    manager
        .register_tool(HistoryContractSpikeTool)
        .await
        .expect("register history contract tool");
    let handle = manager.tool_session_handle();
    let scope = RuntimeScope::task_scope(
        ContextId::new(7, 2),
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000073").unwrap()),
        MessageId::from_external(ExternalId::new("typesafe-history-message")),
        TaskId::from_external(ExternalId::new("typesafe-history-task")),
    );
    let session = handle
        .open_tool_session(&scope, "typesafe/system_one", json!({}))
        .await
        .expect("open step-executor compatibility session");
    let outcome = handle
        .tool_session_send_blocking(
            &session,
            archive_input_json,
            &scope,
            &ContextRefTables::new(),
            Duration::from_secs(1),
        )
        .await
        .expect("run blocking Send");
    let baml_rt_quickjs::tool_session_handle::ToolSessionSendBlockingOutcome::Completed(result) =
        outcome
    else {
        panic!("expected completed blocking Send")
    };
    let header = result.header;
    for expected in ["baseline:noul", "route:choice", "jev-1.13.0"] {
        assert!(
            header.contains(expected),
            "header missing {expected}: {header}"
        );
    }
    for sentinel in [
        "STATE_SENTINEL",
        "INSTRUCTIONS_SENTINEL",
        "CRITERIA_SENTINEL",
    ] {
        assert!(
            !header.contains(sentinel),
            "archive header leaked {sentinel}"
        );
    }
    insta::assert_snapshot!("step_executor_archive_header", header);
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct DeadlineInput {
    value: String,
}

impl DescribeAction for DeadlineInput {
    fn describe(&self) -> String {
        "running deadline contract probe".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct DeadlineOutput {
    value: String,
}

struct DeadlineTool;

#[async_trait]
impl BamlTool for DeadlineTool {
    type Bundle = TypeSafe;
    const LOCAL_NAME: &'static str = "deadline_probe";
    type OpenInput = ();
    type Input = DeadlineInput;
    type Output = DeadlineOutput;

    fn description(&self) -> &'static str {
        "Proves that a one-shot deadline starts during Read"
    }

    async fn execute(
        &self,
        _ctx: &ToolSessionContext,
        args: Self::Input,
    ) -> baml_rt_core::Result<Self::Output> {
        let events = Mutex::new(Vec::new());
        run_spike_adapter(
            SpikeCall {
                valid: true,
                timeout: Duration::from_millis(100),
                validation_wait: Duration::ZERO,
                limiter_wait: Duration::ZERO,
                attempts: vec![SpikeAttempt::Success(Duration::ZERO)],
                backoff: Duration::ZERO,
            },
            &events,
        )
        .await
        .map_err(BamlRtError::ToolExecution)?;
        Ok(DeadlineOutput { value: args.value })
    }
}

#[derive(Debug, Clone, Copy)]
enum SpikeAttempt {
    Retry(Duration),
    Success(Duration),
}

struct SpikeCall {
    valid: bool,
    timeout: Duration,
    validation_wait: Duration,
    limiter_wait: Duration,
    attempts: Vec<SpikeAttempt>,
    backoff: Duration,
}

async fn run_spike_adapter(
    call: SpikeCall,
    events: &Mutex<Vec<&'static str>>,
) -> Result<(), String> {
    // The production adapter creates this deadline when execute starts. A one-shot session calls
    // execute from Read, so time between Send and Read is outside the deadline by construction.
    let deadline = tokio::time::Instant::now() + call.timeout;
    let run = async {
        events.lock().unwrap().push("validate");
        tokio::time::sleep(call.validation_wait).await;
        if !call.valid {
            return Err("invalid_argument".to_string());
        }

        events.lock().unwrap().push("limiter_wait");
        tokio::time::sleep(call.limiter_wait).await;
        events.lock().unwrap().push("limiter_acquired");

        for (index, attempt) in call.attempts.iter().enumerate() {
            events.lock().unwrap().push("attempt");
            let (delay, retry) = match attempt {
                SpikeAttempt::Retry(delay) => (*delay, true),
                SpikeAttempt::Success(delay) => (*delay, false),
            };
            tokio::time::sleep(delay).await;
            if !retry {
                events.lock().unwrap().push("success");
                return Ok(());
            }
            if index + 1 < call.attempts.len() {
                events.lock().unwrap().push("backoff");
                tokio::time::sleep(call.backoff).await;
            }
        }
        Err("attempts_exhausted".to_string())
    };

    tokio::time::timeout_at(deadline, run)
        .await
        .map_err(|_| "deadline_exceeded".to_string())?
}

#[tokio::test(start_paused = true)]
async fn adapter_deadline_covers_limiter_attempts_and_backoff() {
    let events = Mutex::new(Vec::new());
    run_spike_adapter(
        SpikeCall {
            valid: true,
            timeout: Duration::from_secs(10),
            validation_wait: Duration::from_secs(1),
            limiter_wait: Duration::from_secs(2),
            attempts: vec![
                SpikeAttempt::Retry(Duration::from_secs(1)),
                SpikeAttempt::Retry(Duration::from_secs(1)),
                SpikeAttempt::Success(Duration::from_secs(1)),
            ],
            backoff: Duration::from_secs(1),
        },
        &events,
    )
    .await
    .expect("call must fit the overall deadline");
    assert_eq!(
        *events.lock().unwrap(),
        [
            "validate",
            "limiter_wait",
            "limiter_acquired",
            "attempt",
            "backoff",
            "attempt",
            "backoff",
            "attempt",
            "success",
        ]
    );

    let invalid_events = Mutex::new(Vec::new());
    let invalid = run_spike_adapter(
        SpikeCall {
            valid: false,
            timeout: Duration::from_secs(10),
            validation_wait: Duration::ZERO,
            limiter_wait: Duration::from_secs(9),
            attempts: vec![SpikeAttempt::Success(Duration::ZERO)],
            backoff: Duration::ZERO,
        },
        &invalid_events,
    )
    .await;
    assert_eq!(invalid, Err("invalid_argument".to_string()));
    assert_eq!(*invalid_events.lock().unwrap(), ["validate"]);

    let validation_events = Mutex::new(Vec::new());
    let validation_timeout = run_spike_adapter(
        SpikeCall {
            valid: true,
            timeout: Duration::from_secs(5),
            validation_wait: Duration::from_secs(6),
            limiter_wait: Duration::ZERO,
            attempts: vec![SpikeAttempt::Success(Duration::ZERO)],
            backoff: Duration::ZERO,
        },
        &validation_events,
    )
    .await;
    assert_eq!(validation_timeout, Err("deadline_exceeded".to_string()));
    assert_eq!(*validation_events.lock().unwrap(), ["validate"]);

    let limiter_events = Mutex::new(Vec::new());
    let limiter_timeout = run_spike_adapter(
        SpikeCall {
            valid: true,
            timeout: Duration::from_secs(5),
            validation_wait: Duration::ZERO,
            limiter_wait: Duration::from_secs(6),
            attempts: vec![SpikeAttempt::Success(Duration::ZERO)],
            backoff: Duration::ZERO,
        },
        &limiter_events,
    )
    .await;
    assert_eq!(limiter_timeout, Err("deadline_exceeded".to_string()));
    assert_eq!(
        *limiter_events.lock().unwrap(),
        ["validate", "limiter_wait"]
    );

    let backoff_events = Mutex::new(Vec::new());
    let backoff_timeout = run_spike_adapter(
        SpikeCall {
            valid: true,
            timeout: Duration::from_secs(5),
            validation_wait: Duration::ZERO,
            limiter_wait: Duration::ZERO,
            attempts: vec![
                SpikeAttempt::Retry(Duration::from_secs(1)),
                SpikeAttempt::Success(Duration::ZERO),
            ],
            backoff: Duration::from_secs(5),
        },
        &backoff_events,
    )
    .await;
    assert_eq!(backoff_timeout, Err("deadline_exceeded".to_string()));
    assert_eq!(
        *backoff_events.lock().unwrap(),
        [
            "validate",
            "limiter_wait",
            "limiter_acquired",
            "attempt",
            "backoff"
        ]
    );

    let attempt_events = Mutex::new(Vec::new());
    let attempts_exhausted = run_spike_adapter(
        SpikeCall {
            valid: true,
            timeout: Duration::from_secs(5),
            validation_wait: Duration::ZERO,
            limiter_wait: Duration::ZERO,
            attempts: vec![
                SpikeAttempt::Retry(Duration::ZERO),
                SpikeAttempt::Retry(Duration::ZERO),
            ],
            backoff: Duration::ZERO,
        },
        &attempt_events,
    )
    .await;
    assert_eq!(attempts_exhausted, Err("attempts_exhausted".to_string()));
    assert_eq!(
        attempt_events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "attempt")
            .count(),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn send_to_read_gap_does_not_consume_adapter_deadline() {
    let registry = ToolRegistry::new();
    registry
        .register(DeadlineTool)
        .expect("register deadline tool");
    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/deadline_probe", json!({}), &context_id, &agent_id)
        .await
        .expect("open session");
    registry
        .session_send(&session, json!({"value": "ok"}))
        .await
        .expect("send input");

    tokio::time::sleep(Duration::from_secs(30)).await;

    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read after gap");
    let ToolStep::Done {
        output: Some(output),
    } = step
    else {
        panic!("expected done output")
    };
    assert_eq!(output["value"], "ok");
}

type MockResponse = (
    axum::http::StatusCode,
    Vec<(&'static str, &'static str)>,
    String,
);

fn retry_input() -> SystemOneInput {
    SystemOneInput {
        state: OpaqueJson::from(json!({"asset": "pump-12"})),
        questions: BTreeMap::from([(
            "safe".to_string(),
            SystemOneQuestion::Noul(NoulQuestion {
                instructions: OpaqueJson::from(json!("Is the reading safe?")),
                criteria: None,
            }),
        )]),
        model: None,
        timeout_ms: None,
    }
}

fn noul_success_body() -> String {
    json!({
        "model": DEFAULT_MODEL,
        "answers": {"safe": {"type": "noul", "noul": 0.5}}
    })
    .to_string()
}

/// Run one send/read against a mock provider serving the given response
/// sequence (repeating the last response once the queue is exhausted). Returns
/// the session step and the number of HTTP requests the adapter made.
async fn run_against_mock(
    config: SystemOneConfig,
    input: SystemOneInput,
    responses: Vec<MockResponse>,
) -> (ToolStep, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);

    let queue = Arc::new(Mutex::new(
        responses
            .into_iter()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_handler = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/systemone",
        post(move || {
            let queue = Arc::clone(&queue);
            let requests = Arc::clone(&requests_for_handler);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                let mut queue = queue.lock().expect("response queue");
                let response = queue
                    .pop_front()
                    .unwrap_or_else(|| queue.back().cloned().expect("at least one mock response"));
                let (status, headers, body) = response;
                let mut builder = axum::response::Response::builder().status(status);
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
                builder
                    .body(axum::body::Body::from(body))
                    .expect("build mock response")
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });

    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));
    registry.set_secret_resolver(Some(Arc::new(MemoryToolSecretResolver)));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(input).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read step");
    server.abort();
    (step, requests)
}

#[test]
fn retry_after_header_parsing_matrix() {
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        map
    }

    assert_eq!(retry_after_ms(&headers(&[])), None);
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after", "5")])),
        Some(5000)
    );
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after-ms", "250")])),
        Some(250)
    );
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after-ms", "250"), ("retry-after", "5")])),
        Some(250)
    );

    let future = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(30));
    let delay = retry_after_ms(&headers(&[("retry-after", &future)])).expect("future date");
    assert!(
        (29_000..=31_000).contains(&delay),
        "future HTTP date delay was {delay}ms"
    );

    let past = httpdate::fmt_http_date(std::time::SystemTime::now() - Duration::from_secs(30));
    assert_eq!(retry_after_ms(&headers(&[("retry-after", &past)])), None);
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after", "garbage")])),
        None
    );
    assert_eq!(retry_after_ms(&headers(&[("retry-after", "-5")])), None);
}

#[tokio::test]
async fn production_tool_retries_transient_5xx_and_recovers() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 5_000,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![
            (StatusCode::INTERNAL_SERVER_ERROR, vec![], String::new()),
            (StatusCode::OK, vec![], noul_success_body()),
        ],
    )
    .await;
    let ToolStep::Done {
        output: Some(output),
    } = step
    else {
        panic!("expected success after a 5xx retry")
    };
    assert_eq!(output["answers"]["safe"]["noul"], 0.5);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn production_tool_retries_429_using_retry_after_ms_and_recovers() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 5_000,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![
            (
                StatusCode::TOO_MANY_REQUESTS,
                vec![("retry-after-ms", "10")],
                String::new(),
            ),
            (StatusCode::OK, vec![], noul_success_body()),
        ],
    )
    .await;
    let ToolStep::Done {
        output: Some(output),
    } = step
    else {
        panic!("expected success after a 429 retry")
    };
    assert_eq!(output["answers"]["safe"]["noul"], 0.5);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn production_tool_retries_429_using_retry_after_seconds_and_recovers() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 5_000,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![
            (
                StatusCode::TOO_MANY_REQUESTS,
                vec![("retry-after", "0")],
                String::new(),
            ),
            (StatusCode::OK, vec![], noul_success_body()),
        ],
    )
    .await;
    let ToolStep::Done {
        output: Some(output),
    } = step
    else {
        panic!("expected success after a 429 retry")
    };
    assert_eq!(output["answers"]["safe"]["noul"], 0.5);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn production_tool_429_with_retry_after_beyond_deadline_fails_immediately() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 200,
        ..SystemOneConfig::default()
    };
    let started = std::time::Instant::now();
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![(
            StatusCode::TOO_MANY_REQUESTS,
            vec![("retry-after", "60")],
            String::new(),
        )],
    )
    .await;
    let elapsed = started.elapsed();
    let ToolStep::Error { error } = step else {
        panic!("expected a rate limit error")
    };
    assert_eq!(error.classified.code, "typesafe_rate_limited");
    assert_eq!(error.classified.retry_after_ms, Some(60_000));
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "a Retry-After beyond the deadline must not schedule a retry"
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "a Retry-After beyond the deadline must fail immediately, not sleep until the deadline (elapsed {elapsed:?})"
    );
}

#[tokio::test(start_paused = true)]
async fn sleep_for_retry_rejects_unfittable_delay_without_waiting() {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    let started = tokio::time::Instant::now();

    let slept = sleep_for_retry(deadline, Duration::from_secs(60)).await;

    assert!(!slept);
    assert_eq!(
        started.elapsed(),
        Duration::ZERO,
        "an unfittable delay must be rejected immediately, not at the deadline"
    );
}

#[tokio::test]
async fn production_tool_does_not_retry_unauthorized() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![(StatusCode::UNAUTHORIZED, vec![], String::new())],
    )
    .await;
    let ToolStep::Error { error } = step else {
        panic!("expected an unauthorized error")
    };
    assert_eq!(error.classified.code, "typesafe_unauthorized");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn production_tool_maps_403_to_unauthorized_without_retry() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![(StatusCode::FORBIDDEN, vec![], String::new())],
    )
    .await;
    let ToolStep::Error { error } = step else {
        panic!("expected an unauthorized error")
    };
    assert_eq!(error.classified.code, "typesafe_unauthorized");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "403 must not be retried"
    );
}

#[tokio::test]
async fn production_tool_empty_credential_fails_with_missing_credential() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Router, http::StatusCode, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_handler = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/systemone",
        post(move || {
            let requests = Arc::clone(&requests_for_handler);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));
    registry.set_secret_resolver(Some(Arc::new(EmptyMemoryToolSecretResolver)));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(retry_input()).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read error envelope");
    let ToolStep::Error { error } = step else {
        panic!("expected missing credential tool error")
    };
    assert_eq!(error.classified.code, "typesafe_missing_credential");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "an empty credential must fail before any network request"
    );
    server.abort();
}

#[tokio::test]
async fn production_tool_never_leaks_secret_into_rendered_surfaces() {
    use axum::{Router, http::StatusCode, routing::post};
    use test_support::common::TempEnvVar;
    use tokio::net::TcpListener;

    let _env_guard = TYPESAFE_ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock TypeSafe server");
    let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
    let _base_url = TempEnvVar::set(TYPESAFE_API_BASE_URL, &base_url);
    let seen_authorization = Arc::new(Mutex::new(None));
    let seen_authorization_for_handler = Arc::clone(&seen_authorization);
    let app = Router::new().route(
        "/v1/systemone",
        post(move |headers: axum::http::HeaderMap| {
            let seen_authorization = Arc::clone(&seen_authorization_for_handler);
            async move {
                *seen_authorization.lock().expect("authorization lock") = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                StatusCode::UNAUTHORIZED
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock TypeSafe");
    });

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let registry = ToolRegistry::new();
    registry
        .register_dynamic(
            typesafe_system_one_metadata(),
            typesafe_system_one_build().expect("build production tool"),
        )
        .expect("register production tool");
    registry.set_config_resolver(Some(Arc::new(StaticConfigResolver(
        serde_json::to_value(config).expect("serialize config"),
    ))));
    registry.set_secret_resolver(Some(Arc::new(SentinelSecretResolver)));

    let context_id = ContextId::new(7, 1);
    let agent_id =
        AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000072").unwrap());
    let session = registry
        .open_session("typesafe/system_one", Value::Null, &context_id, &agent_id)
        .await
        .expect("open System One session");
    registry
        .session_send(
            &session,
            serde_json::to_value(retry_input()).expect("serialize input"),
        )
        .await
        .expect("send input");
    let step = registry
        .session_read(&session, json!({}))
        .await
        .expect("read error envelope");
    let ToolStep::Error { error } = step else {
        panic!("expected unauthorized tool error")
    };
    assert_eq!(error.classified.code, "typesafe_unauthorized");

    // The key is sent as a bearer token, but never echoed back into any
    // rendered surface.
    assert_eq!(
        seen_authorization
            .lock()
            .expect("authorization lock")
            .as_deref(),
        Some("Bearer SECRET_SENTINEL_12345")
    );
    let rendered_error = serde_json::to_string(&json!({
        "message": error.classified.message,
        "hint": error.classified.hint,
    }))
    .expect("serialize classified error");
    assert!(
        !rendered_error.contains(SECRET_SENTINEL),
        "error envelope leaked the credential"
    );

    let description = registry
        .describe_invocation_for(
            "typesafe/system_one",
            &json!({"op": "Send", "input": serde_json::to_value(retry_input()).expect("input")}),
        )
        .expect("tool-call description");
    assert!(
        !description.contains(SECRET_SENTINEL),
        "tool-call description leaked the credential"
    );
    server.abort();
}

#[tokio::test]
async fn production_tool_does_not_retry_client_errors() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![(StatusCode::BAD_REQUEST, vec![], String::new())],
    )
    .await;
    let ToolStep::Error { error } = step else {
        panic!("expected a contract error")
    };
    assert_eq!(error.classified.code, "typesafe_provider_contract");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn production_tool_exhausts_attempts_then_reports_provider_unavailable() {
    use std::sync::atomic::Ordering;

    use axum::http::StatusCode;

    let config = SystemOneConfig {
        enabled: true,
        timeout_ms: 5_000,
        max_attempts: 2,
        ..SystemOneConfig::default()
    };
    let (step, requests) = run_against_mock(
        config,
        retry_input(),
        vec![(StatusCode::INTERNAL_SERVER_ERROR, vec![], String::new())],
    )
    .await;
    let ToolStep::Error { error } = step else {
        panic!("expected a provider unavailable error")
    };
    assert_eq!(error.classified.code, "typesafe_provider_unavailable");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}
