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
use baml_derive_core::TsType;
use baml_rt_core::{
    BamlRtError, ErrorDisposition,
    context::{InvocationScope, RuntimeScope},
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
};
use baml_rt_tools::{
    BamlTool, ToolName, ToolRegistry, ToolSessionContext, ToolStep,
    archive_refs::{ContextRefTables, RefTable},
    bundles::BundleType,
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
    assert_eq!(config.controls.requests_per_minute, 600);
    assert!(config.is_valid());

    let mut invalid = config.clone();
    invalid.max_attempts = 0;
    assert!(!invalid.is_valid(), "zero attempts must be rejected");
    invalid = config.clone();
    invalid.limits.max_choice_options = 256;
    assert!(
        !invalid.is_valid(),
        "limits above provider maxima must be rejected"
    );

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

#[test]
fn schema_and_typescript_cover_all_question_and_answer_variants() {
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

    let declarations = [
        <OpaqueJson as TsType>::ts_decl(),
        <ChoiceQuestion as TsType>::ts_decl(),
        <ScoreQuestion as TsType>::ts_decl(),
        <NoulCriteria as TsType>::ts_decl(),
        <NoulQuestion as TsType>::ts_decl(),
        <SystemOneQuestion as TsType>::ts_decl(),
        <SystemOneInput as TsType>::ts_decl(),
        <ChoiceAnswer as TsType>::ts_decl(),
        <ScoreAnswer as TsType>::ts_decl(),
        <NoulAnswer as TsType>::ts_decl(),
        <SystemOneAnswer as TsType>::ts_decl(),
        <SystemOneUsage as TsType>::ts_decl(),
        <SystemOneOutput as TsType>::ts_decl(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    for tag in ["choice", "score", "noul"] {
        assert!(
            declarations.contains(&format!("type: \"{tag}\"")),
            "TypeScript must contain the {tag} variant"
        );
    }
    insta::assert_snapshot!("system_one_typescript", declarations);
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
