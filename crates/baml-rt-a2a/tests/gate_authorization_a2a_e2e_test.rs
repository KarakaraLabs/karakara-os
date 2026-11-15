// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! End-to-end A2A path: tier-3 karakara gate → INPUT_REQUIRED + gateAuthorization → approve resume → tool executes.

#![allow(clippy::redundant_closure)] // test helpers take &Value; iterators often yield &&Value

use std::sync::{Arc, OnceLock};

use baml_rt::{A2aAgent, A2aRequestHandler, QuickJSConfig, baml::BamlRuntimeManager};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, collect_a2a_stream_one_shot,
    ids::{ContextId, ExternalId, MessageId, TaskId},
};
use baml_rt_karakara::{
    GateAuthorizationRequest,
    config::{KarakaraConfig, KarakaraMode, KarakaraPolicy},
    global::{global_grounding_store, set_global_karakara_config, submit_grounding},
    schema::{Anchor, AnchorSign, EnvSignals, Node, ParseArtifact, Postcondition},
};
use baml_rt_provenance::ProvenanceWriter;
use baml_rt_tools::{
    ToolMetadataBuilder,
    tools::{
        ToolAccess, ToolFunctionMetadata, ToolName, TypeBasedMetadataBuilder,
        create_one_shot_tool_from_async,
    },
};
use baml_tools_calculator::{CalculatorInput, CalculatorOutput, MathOperation};
use serde_json::Value;
use test_support::{
    common::send_stream_request_with_task, testing::provenance_fixtures::build_isolated_store,
};

static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn gate_invoker_js() -> String {
    r#"
    globalThis.onChatMessage = async function(message) {
        const text = message?.parts?.[0]?.text || "";
        if (text.includes("gate-e2e") || text.trim() === "approve") {
            try {
                const args = JSON.stringify({
                    expression: { left: 1, operation: "Add", right: 2 }
                });
                const result = await __tool_invoke("support/gate_delete", args);
                __chat_yield({
                    __session: message.__session,
                    message: { parts: [{ text: "executed:" + JSON.stringify(result) }] }
                });
                __chat_yield({ __session: message.__session, final: true });
            } catch (e) {
                const msg = String(e);
                if (msg.includes("Gate authorization required")) {
                    return;
                }
                __chat_yield({
                    __session: message.__session,
                    message: { parts: [{ text: "tool-error:" + msg }] },
                    final: true
                });
                return;
            }
            return;
        }
        __chat_yield({ __session: message.__session, message: { parts: [{ text: "unknown" }] } });
        __chat_yield({ __session: message.__session, final: true });
    };
    "#
    .to_string()
}

fn grounded_artifact() -> ParseArtifact {
    ParseArtifact {
        instruction: "Archive users inactive per agreed definition in prod db".into(),
        template: "agentic_execution".into(),
        env: EnvSignals {
            environment: "prod".into(),
            verb_class: "mutating".into(),
            reversible: true,
            external_visibility: false,
        },
        nodes: vec![
            Node {
                name: "OBJECT".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "users".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "schema users(id,last_login_at,status,deleted_at)".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "TARGET".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Index,
                    content: "prod app.db".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "ACTION".into(),
                anchors: vec![
                    Anchor {
                        sign: AnchorSign::Symbol,
                        content: "archive".into(),
                        source: "user".into(),
                    },
                    Anchor {
                        sign: AnchorSign::Icon,
                        content: "set deleted_at=now".into(),
                        source: "user".into(),
                    },
                ],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "SCOPE".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Icon,
                    content: "last_login_at older than 90 days, per user answer".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "METHOD".into(),
                anchors: vec![],
                interpretations: vec![],
                trojan: None,
            },
            Node {
                name: "CRITERION".into(),
                anchors: vec![Anchor {
                    sign: AnchorSign::Verify,
                    content: "rowcount(users where deleted_at set) >= 1".into(),
                    source: "user".into(),
                }],
                interpretations: vec![],
                trojan: None,
            },
        ],
        covers: vec!["support/gate_delete".into()],
        postconditions: vec![Postcondition {
            cmd: "true".into(),
            desc: "placeholder assertion".into(),
        }],
    }
}

async fn setup_gate_agent() -> A2aAgent {
    setup_gate_agent_with_store(None).await
}

async fn setup_gate_agent_with_store(
    store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
) -> A2aAgent {
    set_global_karakara_config(KarakaraConfig {
        default: KarakaraPolicy {
            enabled: true,
            mode: KarakaraMode::Enforce,
            enforce_min_tier: 2,
            ..Default::default()
        },
        overrides: Default::default(),
    });

    let manager = BamlRuntimeManager::builder()
        .build()
        .expect("runtime manager");

    let name = ToolName::parse("support/gate_delete").expect("parse tool name");
    let class_name = ToolFunctionMetadata::derive_class_name(name.bundle(), name.local());
    let metadata = TypeBasedMetadataBuilder::<(), CalculatorInput, CalculatorOutput>::new(
        name,
        class_name,
        "Tier-3 gate E2E delete-class tool".to_string(),
    )
    .with_access(ToolAccess::Delete)
    .with_tags(vec!["karakara:tier=3".to_string()])
    .build_metadata();
    let handler = create_one_shot_tool_from_async::<(), CalculatorInput, CalculatorOutput, _>(
        metadata.clone(),
        |input: CalculatorInput| {
            Box::pin(async move {
                let left = input.expression.left;
                let right = input.expression.right;
                let result = match input.expression.operation {
                    MathOperation::Add => left + right,
                    MathOperation::Subtract => left - right,
                    MathOperation::Multiply => left * right,
                    MathOperation::Divide => {
                        if right == 0 {
                            return Err(baml_rt::BamlRtError::InvalidArgument(
                                "division by zero".into(),
                            ));
                        }
                        left / right
                    }
                };
                Ok(CalculatorOutput {
                    expression: format!("{left} + {right}"),
                    result: result as f64,
                    formatted: format!("{result}"),
                })
            })
        },
    );
    manager
        .tool_registry()
        .register_dynamic(metadata, handler)
        .expect("register gate_delete tool");

    let mut builder = A2aAgent::builder().with_runtime_manager(manager);
    if let Some(store) = store {
        builder = builder.with_surreal_store(store);
    }
    builder
        .with_init_js(gate_invoker_js())
        .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
        .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
        .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
        .build()
        .await
        .expect("build gate e2e agent")
}

async fn collect_stream(agent: &A2aAgent, request: Value) -> baml_rt::Result<Vec<Value>> {
    let stream = agent
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await?;
    let chunks: Vec<A2aStreamChunk> = collect_a2a_stream_one_shot(stream).await;
    Ok(chunks.into_iter().map(A2aStreamChunk::into_inner).collect())
}

async fn test_gate() -> tokio::sync::OwnedSemaphorePermit {
    let gate = GATE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone();
    gate.acquire_owned().await.expect("test gate")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_chat_text_never_grants_tier3_authorization() {
    let _permit = test_gate().await;
    let agent = setup_gate_agent().await;
    let context_id = ContextId::new(42, 1);
    let task_id = TaskId::from_external(ExternalId::new("live-task:ctx-42-1:msg-gate-1"));

    let scope = baml_rt_core::context::RuntimeScope::task_scope(
        context_id.clone(),
        agent.agent_id().clone(),
        MessageId::from("msg-gate-1"),
        task_id.clone(),
    );
    submit_grounding(&scope, grounded_artifact(), None);
    assert!(
        global_grounding_store()
            .get_live(&scope, std::time::Duration::from_secs(3600))
            .is_some()
    );

    let turn1 = send_stream_request_with_task(
        "msg-gate-1",
        "gate-e2e",
        "corr-42-1",
        Some(context_id.clone()),
        None,
    );
    let r1 = collect_stream(&agent, turn1).await.expect("first stream");
    let response1 = serde_json::to_string(&r1).unwrap_or_default();
    assert!(response1.contains("authorization denied"));
    assert!(!response1.contains("executed:"));
    // A later message containing the word "approve" is just another message. It
    // cannot change the durable denial or authorize the tool call.
    let turn2 =
        send_stream_request_with_task("msg-gate-2", "approve", "corr-42-2", Some(context_id), None);
    let r2 = collect_stream(&agent, turn2).await.expect("second stream");
    let response2 = serde_json::to_string(&r2).unwrap_or_default();
    assert!(!response2.contains("executed:"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_ingress_auto_approves_tier3_in_the_a2a_path() {
    let _permit = test_gate().await;
    let context_id = ContextId::new(43, 1);
    let store = build_isolated_store().await;
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            context_id.clone(),
            "grafana".to_string(),
            "grafana:alert-1".to_string(),
            "alert-1".to_string(),
            "grafana.alert.v1".to_string(),
            1,
            None,
            Vec::new(),
        ))
        .await
        .expect("write Grafana ingress");
    let store_for_assert = store.clone();
    let agent = setup_gate_agent_with_store(Some(store)).await;
    let task_id = TaskId::from_external(ExternalId::new("live-task:ctx-43-1:msg-gate-1"));
    let scope = baml_rt_core::context::RuntimeScope::task_scope(
        context_id.clone(),
        agent.agent_id().clone(),
        MessageId::from("msg-gate-1"),
        task_id,
    );
    submit_grounding(&scope, grounded_artifact(), None);

    let request = send_stream_request_with_task(
        "msg-gate-1",
        "gate-e2e",
        "corr-43-1",
        Some(context_id.clone()),
        None,
    );
    let responses = collect_stream(&agent, request).await.expect("A2A stream");
    let response = serde_json::to_string(&responses).unwrap_or_default();
    assert!(
        response.contains("executed:"),
        "trusted ingress must run the tool: {response}"
    );
    assert!(!response.contains("authorization denied"));

    let decision = store_for_assert
        .gate_ledger_decision(
            &context_id,
            &GateAuthorizationRequest {
                scope,
                tool_name: "support/gate_delete".to_string(),
                args: serde_json::json!({
                    "expression": { "left": 1, "operation": "Add", "right": 2 }
                }),
                prompt: String::new(),
            }
            .gate_id(),
        )
        .await
        .expect("read machine ingress gate decision")
        .expect("machine ingress decision");
    assert_eq!(
        decision.outcome,
        baml_rt_provenance::GateDecisionOutcome::Allow
    );
    assert!(matches!(
        decision.principal,
        baml_rt_provenance::GatePrincipal::Ingress { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_ingress_recovers_a_request_left_without_a_decision() {
    let _permit = test_gate().await;
    let context_id = ContextId::new(44, 1);
    let store = build_isolated_store().await;
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            context_id.clone(),
            "grafana".to_string(),
            "grafana:alert-2".to_string(),
            "alert-2".to_string(),
            "grafana.alert.v1".to_string(),
            1,
            None,
            Vec::new(),
        ))
        .await
        .expect("write Grafana ingress");
    let store_for_assert = store.clone();
    let agent = setup_gate_agent_with_store(Some(store.clone())).await;
    let task_id = TaskId::from_external(ExternalId::new("live-task:ctx-44-1:msg-gate-1"));
    let scope = baml_rt_core::context::RuntimeScope::task_scope(
        context_id.clone(),
        agent.agent_id().clone(),
        MessageId::from("msg-gate-1"),
        task_id.clone(),
    );
    let gate_id = GateAuthorizationRequest {
        scope: scope.clone(),
        tool_name: "support/gate_delete".to_string(),
        args: serde_json::json!({
            "expression": { "left": 1, "operation": "Add", "right": 2 }
        }),
        prompt: String::new(),
    }
    .gate_id();
    // Simulate an earlier attempt whose request write succeeded but whose decision
    // write failed: a channel-less request with no decision.
    store
        .add_event(baml_rt_provenance::ProvEvent::audit_decision(
            context_id.clone(),
            baml_rt_provenance::AuditDecision::GateRequested {
                gate_id: gate_id.clone(),
                task_id,
                agent_id: agent.agent_id().clone(),
                tool_name: "support/gate_delete".to_string(),
                channel: None,
                expires_at_ms: baml_rt_core::now_unix_ms(baml_rt_core::clock_events::KARAKARA_GATE)
                    + 900_000,
            },
        ))
        .await
        .expect("write orphaned request");
    submit_grounding(&scope, grounded_artifact(), None);

    let request = send_stream_request_with_task(
        "msg-gate-1",
        "gate-e2e",
        "corr-44-1",
        Some(context_id.clone()),
        None,
    );
    let responses = collect_stream(&agent, request).await.expect("A2A stream");
    let response = serde_json::to_string(&responses).unwrap_or_default();
    assert!(
        response.contains("executed:"),
        "a channel-less request must be decided again, not left pending: {response}"
    );
    let decision = store_for_assert
        .gate_ledger_decision(&context_id, &gate_id)
        .await
        .expect("read decision")
        .expect("decision");
    assert_eq!(
        decision.outcome,
        baml_rt_provenance::GateDecisionOutcome::Allow
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_gate_ledger_denies_instead_of_running_the_tool() {
    let _permit = test_gate().await;
    let context_id = ContextId::new(45, 1);
    let store = build_isolated_store().await;
    store
        .add_event(baml_rt_provenance::ProvEvent::host_source_poll_recorded(
            context_id.clone(),
            "grafana".to_string(),
            "grafana:alert-3".to_string(),
            "alert-3".to_string(),
            "grafana.alert.v1".to_string(),
            1,
            None,
            Vec::new(),
        ))
        .await
        .expect("write Grafana ingress");
    let agent = setup_gate_agent_with_store(Some(store.clone())).await;
    let task_id = TaskId::from_external(ExternalId::new("live-task:ctx-45-1:msg-gate-1"));
    let scope = baml_rt_core::context::RuntimeScope::task_scope(
        context_id.clone(),
        agent.agent_id().clone(),
        MessageId::from("msg-gate-1"),
        task_id.clone(),
    );
    let gate_id = GateAuthorizationRequest {
        scope: scope.clone(),
        tool_name: "support/gate_delete".to_string(),
        args: serde_json::json!({
            "expression": { "left": 1, "operation": "Add", "right": 2 }
        }),
        prompt: String::new(),
    }
    .gate_id();
    // An expiry that cannot be read back makes the ledger read fail. The gate must
    // deny, even though this flow's origin would otherwise auto-approve.
    store
        .add_event(baml_rt_provenance::ProvEvent::audit_decision(
            context_id.clone(),
            baml_rt_provenance::AuditDecision::GateRequested {
                gate_id,
                task_id,
                agent_id: agent.agent_id().clone(),
                tool_name: "support/gate_delete".to_string(),
                channel: Some("C123".to_string()),
                expires_at_ms: u64::MAX,
            },
        ))
        .await
        .expect("write unreadable request");
    submit_grounding(&scope, grounded_artifact(), None);

    let request = send_stream_request_with_task(
        "msg-gate-1",
        "gate-e2e",
        "corr-45-1",
        Some(context_id),
        None,
    );
    let responses = collect_stream(&agent, request).await.expect("A2A stream");
    let response = serde_json::to_string(&responses).unwrap_or_default();
    assert!(
        !response.contains("executed:"),
        "a gate that cannot be resolved must not run the tool: {response}"
    );
}
