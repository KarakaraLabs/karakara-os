// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "llm-tests")]

mod common;

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use baml_rt::{InterceptorDecision, LLMCallContext, LLMInterceptor, baml::BamlRuntimeManager};
use baml_rt_core::{
    AgentDiscoveryEntry, AgentLister,
    bus::BusWithEffects,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, SurrealProvenanceStore};
use baml_tools_system::SystemBundle;
use common::{
    CapturingA2aHandler, StaticAgentList, TempDirCleanup, build_agent_dir_to_temp_async,
    discovery_entry, e2e_serial_gate, post_a2a_sse_collect, start_runner_api_server,
};
use serde_json::{Value, json};
use test_support::common::{
    send_stream_request, test_surreal_store, workspace_fnox_path, workspace_root,
};
use tokio::time::{Duration, timeout};

#[derive(Clone, Copy)]
enum PlanResponse {
    DelegateClickUp,
    Empty,
    Undiscovered,
}

#[derive(Clone)]
struct CoordinatorRoutingInterceptor {
    response: PlanResponse,
    planner_prompts: Arc<Mutex<Vec<String>>>,
    format_capabilities_called: Arc<AtomicBool>,
}

#[async_trait]
impl LLMInterceptor for CoordinatorRoutingInterceptor {
    async fn intercept_llm_call(
        &self,
        ctx: &LLMCallContext,
    ) -> baml_rt_core::Result<InterceptorDecision> {
        let function = ctx.function_id.full_name();
        match function.as_str() {
            "ClassifyCoordinatorTurn" => Ok(InterceptorDecision::Substitute(json!({
                "kind": "ready",
                "inferred_intent": "List all in-progress ClickUp tasks."
            }))),
            "MakeStructuredPlan" => {
                self.planner_prompts
                    .lock()
                    .expect("planner prompt mutex poisoned")
                    .push(serde_json::to_string(&ctx.prompt).unwrap_or_default());
                let plan_steps = match self.response {
                    PlanResponse::DelegateClickUp => json!([{
                        "agent_package": "clickup-agent",
                        "sub_message": "List every task whose status is in progress."
                    }]),
                    PlanResponse::Empty => json!([]),
                    PlanResponse::Undiscovered => json!([{
                        "agent_package": "notion-agent",
                        "sub_message": "List every in-progress task."
                    }]),
                };
                Ok(InterceptorDecision::Substitute(json!({
                    "intent_description": "Retrieve in-progress ClickUp tasks.",
                    "objective": "Return all in-progress ClickUp tasks.",
                    "plan_steps": plan_steps,
                    "citations": []
                })))
            }
            "FormatCapabilities" => {
                self.format_capabilities_called
                    .store(true, Ordering::SeqCst);
                Ok(InterceptorDecision::Substitute(json!({
                    "parts": [{"type": "text", "text": "unexpected capabilities fallback"}],
                    "citations": []
                })))
            }
            other => Ok(InterceptorDecision::Block(format!(
                "unexpected coordinator LLM call: {other}"
            ))),
        }
    }

    async fn on_llm_call_complete(
        &self,
        _ctx: &LLMCallContext,
        _result: &baml_rt_core::Result<Value>,
        _ms: u64,
    ) {
    }
}

struct CoordinatorHarness {
    agent: baml_rt::A2aAgent,
    store: Arc<SurrealProvenanceStore>,
    built_dir: PathBuf,
    handler: Arc<CapturingA2aHandler>,
    planner_prompts: Arc<Mutex<Vec<String>>>,
    format_capabilities_called: Arc<AtomicBool>,
}

async fn setup_coordinator(
    entries: Vec<AgentDiscoveryEntry>,
    response: PlanResponse,
) -> CoordinatorHarness {
    let agent_dir = workspace_root().join("agents").join("coordinator-agent");
    let built_dir = build_agent_dir_to_temp_async(agent_dir, "coordinator-agent-routing").await;
    let mut manager = BamlRuntimeManager::builder()
        .with_fnox_llm_resolver(workspace_fnox_path())
        .build()
        .expect("create runtime manager");
    manager
        .load_schema(built_dir.to_str().expect("coordinator path utf8"))
        .expect("load coordinator schema");

    let planner_prompts = Arc::new(Mutex::new(Vec::new()));
    let format_capabilities_called = Arc::new(AtomicBool::new(false));
    manager
        .register_llm_interceptor(CoordinatorRoutingInterceptor {
            response,
            planner_prompts: planner_prompts.clone(),
            format_capabilities_called: format_capabilities_called.clone(),
        })
        .await;

    let allowlist: HashSet<String> = [
        "system/callback",
        "system/discover_agents",
        "system/discover_tools",
        "system/internal_a2a",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    manager
        .set_tool_allowlist(allowlist)
        .await
        .expect("set coordinator tool allowlist");

    let agent_list: Arc<dyn AgentLister> = Arc::new(StaticAgentList { entries });
    let handler = Arc::new(CapturingA2aHandler::default());
    let registry = manager.tool_registry();
    registry
        .register_bundle(SystemBundle::new(
            agent_list,
            registry.clone(),
            handler.clone(),
        ))
        .expect("register system bundle");

    let store = test_surreal_store().await;
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("coordinator-agent").expect("agent type"),
            "1.0.0".to_string(),
            "coordinator-agent@1.0.0".to_string(),
        ))
        .await
        .expect("write coordinator boot event");

    let agent_code = fs::read_to_string(built_dir.join("dist").join("index.js"))
        .expect("coordinator dist/index.js");
    let agent = baml_rt::A2aAgent::builder()
        .with_agent_id(agent_id)
        .with_surreal_store(store.clone())
        .with_runtime_manager(manager)
        .with_init_js(agent_code)
        .with_effect_emitter(Arc::new(BusWithEffects::new()))
        .build()
        .await
        .expect("build coordinator agent");

    CoordinatorHarness {
        agent,
        store,
        built_dir,
        handler,
        planner_prompts,
        format_capabilities_called,
    }
}

async fn run_coordinator(
    harness: CoordinatorHarness,
    context_id: ContextId,
) -> (String, CoordinatorHarness) {
    let server = start_runner_api_server(
        "coordinator-agent",
        harness.agent.clone(),
        harness.store.clone(),
    )
    .await
    .expect("start coordinator runner API");
    let correlation_id = baml_rt_core::correlation::generate_correlation_id();
    let request = send_stream_request(
        "coordinator-routing-message",
        "Use the ClickUp agent to list all tasks in progress.",
        correlation_id.as_str(),
        Some(context_id),
    );
    let responses = timeout(
        Duration::from_secs(30),
        post_a2a_sse_collect(
            &reqwest::Client::new(),
            &format!("{}/agents/coordinator-agent/a2a/sse", server.base_url),
            &request,
        ),
    )
    .await
    .expect("coordinator request timed out")
    .expect("coordinator request failed");
    server.stop().await;
    (
        serde_json::to_string(&responses).expect("serialize coordinator responses"),
        harness,
    )
}

#[tokio::test]
async fn coordinator_discovers_candidates_explicitly_and_delegates_once() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire E2E gate");
    let harness = setup_coordinator(
        vec![
            discovery_entry("coordinator-agent", &["coordination"]),
            discovery_entry("clickup-agent", &["clickup:list-tasks"]),
        ],
        PlanResponse::DelegateClickUp,
    )
    .await;
    let _built_dir = TempDirCleanup::new(harness.built_dir.clone());

    let (responses, harness) =
        run_coordinator(harness, ContextId::from("ctx-coordinator-route")).await;
    let prompts = harness
        .planner_prompts
        .lock()
        .expect("planner prompt mutex poisoned");
    assert_eq!(prompts.len(), 1, "planner should run exactly once");
    let prompt = &prompts[0];
    let available_start = prompt
        .find("---AVAILABLE AGENTS---")
        .expect("planner prompt must contain explicit available-agent section");
    let available_end = prompt[available_start..]
        .find("---END AVAILABLE AGENTS---")
        .map(|offset| available_start + offset)
        .expect("planner prompt must close explicit available-agent section");
    let available = &prompt[available_start..available_end];
    assert!(available.contains("clickup-agent"), "prompt: {prompt}");
    assert!(
        !available.contains("coordinator-agent"),
        "coordinator must be filtered from routing candidates: {available}"
    );
    drop(prompts);

    let calls = harness.handler.snapshot_calls().await;
    assert_eq!(calls.len(), 1, "coordinator must delegate exactly once");
    assert_eq!(calls[0].agent_package, "clickup-agent");
    assert!(calls[0].prompt.contains("in progress"));
    assert!(
        !harness.format_capabilities_called.load(Ordering::SeqCst),
        "routable work must not fall through to FormatCapabilities"
    );
    assert!(
        !responses.contains("unexpected capabilities fallback"),
        "responses: {responses}"
    );
}

#[tokio::test]
async fn coordinator_routing_failures_do_not_fall_back_to_a_greeting() {
    let _permit = e2e_serial_gate().acquire().await.expect("acquire E2E gate");
    let cases = [
        (
            PlanResponse::Empty,
            vec![discovery_entry("clickup-agent", &["clickup:list-tasks"])],
            "Routing planner returned no delegation steps",
        ),
        (
            PlanResponse::Undiscovered,
            vec![discovery_entry("clickup-agent", &["clickup:list-tasks"])],
            "Routing planner selected undiscovered agent package",
        ),
        (
            PlanResponse::DelegateClickUp,
            vec![discovery_entry("coordinator-agent", &["coordination"])],
            "No specialist agents matched this request",
        ),
    ];

    for (index, (response, entries, expected_error)) in cases.into_iter().enumerate() {
        let harness = setup_coordinator(entries, response).await;
        let _built_dir = TempDirCleanup::new(harness.built_dir.clone());
        let context_id = format!("ctx-coordinator-routing-error-{index}");
        let (responses, harness) =
            run_coordinator(harness, ContextId::from(context_id.as_str())).await;

        assert!(
            responses.contains(expected_error),
            "expected {expected_error:?} in responses: {responses}"
        );
        assert!(
            harness.handler.snapshot_calls().await.is_empty(),
            "routing validation failures must happen before delegation"
        );
        assert!(
            !harness.format_capabilities_called.load(Ordering::SeqCst),
            "routing failures must not fall through to FormatCapabilities"
        );
        assert!(
            !responses.contains("unexpected capabilities fallback"),
            "responses: {responses}"
        );
    }
}
