// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Several agents on one host share one runtime store.
//!
//! Every agent here is built on the **same** `HostRuntimeStore`, as the runner
//! does in production. The host is a small in-process stand-in: it fans
//! produced events out to subscribers and routes `system/internal_a2a`, recording
//! `DelegationLinked` the way `baml-agent-runner::routing` does.
//!
//! ## Invariants
//!
//! - **I-INCIDENT-CONTEXT:** One incident is one `context_id`. It holds one root
//!   task per subscriber per event, and each root has its own delegation subtree.
//!   Subscribers that receive the same event have no parent link between them.
//! - **I-ONE-OWNER:** Every task has exactly one executing agent.
//! - **I-SUBSCRIBE-ANY-AGENT:** `tasks.subscribe` through any agent on the host
//!   streams the task's live frames and closes when the task finishes.
//! - **I-FOREIGN-TASK-WRITE:** A write to a task by an agent other than its owner
//!   is rejected. Writing into the shared context creates the writer's own task.
//!
//! Tests marked `#[ignore]` fail on purpose today and record the defect that a
//! later PR fixes. Run them with:
//!
//! ```bash
//! cargo nextest run -p baml-agent-runner --test host_runtime_store_test --run-ignored all
//! ```

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::{
    A2aAgent, A2aRequestHandler, HostRuntimeStore,
    baml::BamlRuntimeManager,
    tools::{BamlTool, ToolSessionContext},
};
use baml_rt_a2a::{AgentRegistry, EventDispatcher, RegistrationMode};
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentCard, AgentDiscoveryEntry, AgentDispatchAck,
    AgentDispatchRequest, AgentDispatchRoutingKey, AgentLister, AgentPackageName, AgentRouteKey,
    BamlRtError, DelegationCallContext, EventSchemaVersion, EventSourceKind, HostPublishService,
    ProducedEvent,
    bus::{BusStream, BusWithEffects},
    ensure_dispatch_task_scope,
    event_subscription::{EventSourceKey, EventSubscription},
    ids::{AgentId, ContextId, ExternalId, TaskId, UuidId},
};
use baml_rt_provenance::{
    AgentType, DelegationLinkedSpec, ProvEvent, ProvenanceWriter, SurrealProvenanceStore,
    id_semantics::{agent_node_id_string, task_execution_activity_id_string},
    task_entity_id_string,
};
use baml_rt_tools::bundles::BundleType;
use common::e2e_serial_gate;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use test_support::common::{send_stream_request_with_task, test_surreal_store};
use tokio::{sync::Notify, time::timeout};

const ALERT_SCHEMA: &str = "grafana.alert.v1";
const ALERT_SOURCE_KIND: &str = "grafana";
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

/// Leaf specialist: answers a delegated chat message and finishes.
const SPECIALIST_JS: &str = r#"
globalThis.onChatMessage = async function(message) {
  const text = message?.parts?.[0]?.text || "";
  __chat_yield({ message: { parts: [{ text: `__NAME__ handled: ${text}` }] } });
  __chat_yield({ final: true });
};
"#;

/// Coordinator: on a dispatched alert, delegates to both specialists in turn.
const COORDINATOR_JS: &str = r#"
async function delegate(target, text) {
  const session = await openToolSession("system/internal_a2a", {
    target: { agent_package: target }
  });
  await session.send({ parts: [{ text }] });
  let next = await session.continue();
  while (next?.status === "streaming" || next?.status === "suspended") {
    next = await session.continue();
  }
  if (next?.status === "error") {
    throw new Error(next?.error?.message || "delegation failed");
  }
  await session.finish();
}

globalThis.onDispatch = async function(_request) {
  await delegate("telemetry-investigator", "investigate the alert");
  await delegate("slack-notify", "post the incident summary");
  return { accepted: true, detail: "coordinator delegated" };
};
"#;

/// Hook subscriber: receives the same alert directly from the host.
const WATCHER_JS: &str = r#"
globalThis.onDispatch = async function(_request) {
  return { accepted: true, detail: "watcher saw alert" };
};
"#;

/// Owner of a long-running task: works until the test releases `b1/hold`.
const HOLDING_JS: &str = r#"
globalThis.onChatMessage = async function(_message) {
  __chat_yield({ message: { parts: [{ text: "working" }] } });
  await __tool_invoke("b1/hold", JSON.stringify({ label: "hold" }));
  __chat_yield({ message: { parts: [{ text: "done" }] } });
  __chat_yield({ final: true });
};
"#;

/// Plain chat agent that answers anything.
const ECHO_JS: &str = r#"
globalThis.onChatMessage = async function(message) {
  const text = message?.parts?.[0]?.text || "";
  __chat_yield({ message: { parts: [{ text: `echo: ${text}` }] } });
  __chat_yield({ final: true });
};
"#;

struct B1;

impl BundleType for B1 {
    const NAME: &'static str = "b1";
    fn description() -> &'static str {
        "Host runtime store test tools"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct HoldInput {
    label: String,
}

impl baml_rt_tools::DescribeAction for HoldInput {
    fn describe(&self) -> String {
        format!("holding {}", self.label)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
struct HoldOutput {
    released: bool,
}

/// Blocks the calling task until the test releases it, so the task stays live.
struct HoldTool {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl BamlTool for HoldTool {
    type Bundle = B1;
    const LOCAL_NAME: &'static str = "hold";
    type OpenInput = ();
    type Input = HoldInput;
    type Output = HoldOutput;

    fn description(&self) -> &'static str {
        "Waits until the test releases it"
    }

    async fn execute(
        &self,
        _ctx: &ToolSessionContext,
        _args: Self::Input,
    ) -> baml_rt::Result<Self::Output> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(HoldOutput { released: true })
    }
}

/// Write the boot event the runner writes, so the agent node exists in the graph.
async fn boot(store: &SurrealProvenanceStore, package: &str) -> AgentId {
    let agent_id = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new(package).expect("agent type"),
            "1.0.0".to_string(),
            format!("{package}@1.0.0"),
        ))
        .await
        .expect("write AgentBooted");
    agent_id
}

struct AgentSpec<'a> {
    package: &'a str,
    js: String,
    manager: BamlRuntimeManager,
    router: Option<Arc<dyn A2aRequestHandler>>,
}

impl<'a> AgentSpec<'a> {
    fn new(package: &'a str, js: impl Into<String>) -> Self {
        Self {
            package,
            js: js.into(),
            manager: BamlRuntimeManager::builder()
                .build()
                .expect("runtime manager"),
            router: None,
        }
    }

    fn with_router(mut self, router: Arc<dyn A2aRequestHandler>) -> Self {
        self.router = Some(router);
        self
    }

    /// Build on the host's shared runtime store, as the runner does.
    async fn build(self, host: &Arc<HostRuntimeStore>) -> A2aAgent {
        let agent_id = boot(host.provenance(), self.package).await;
        let mut builder = A2aAgent::builder()
            .with_agent_id(agent_id)
            .with_runtime_manager(self.manager);
        if let Some(router) = self.router {
            builder = builder
                .with_a2a_session_tool(RegistrationMode::Register)
                .with_a2a_session_router(router);
        }
        builder
            .with_init_js(self.js)
            .with_effect_emitter(Arc::new(BusWithEffects::new()))
            .with_host_store(Arc::clone(host))
            .build()
            .await
            .unwrap_or_else(|err| panic!("build {}: {err}", self.package))
    }
}

// ---------------------------------------------------------------------------
// In-process host
// ---------------------------------------------------------------------------

/// Routes `system/internal_a2a` by target package and records delegation lineage,
/// like the runner's `InternalA2aRouter`.
struct DelegationRouter {
    caller_name: String,
    routes: HashMap<String, A2aAgent>,
}

impl DelegationRouter {
    fn agent_for(&self, request: &A2aWireRequest) -> baml_rt::Result<&A2aAgent> {
        let package = request
            .as_ref()
            .pointer("/params/metadata/target/agent_package")
            .and_then(Value::as_str)
            .ok_or_else(|| BamlRtError::InvalidArgument("missing target.agent_package".into()))?;
        self.routes
            .get(package)
            .ok_or_else(|| BamlRtError::InvalidArgument(format!("no route for {package}")))
    }
}

#[async_trait]
impl A2aRequestHandler for DelegationRouter {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> baml_rt::Result<BusStream<A2aStreamChunk>> {
        self.agent_for(&request)?.handle_a2a_stream(request).await
    }

    async fn handle_delegated_a2a_stream(
        &self,
        request: A2aWireRequest,
        delegation: DelegationCallContext,
    ) -> baml_rt::Result<BusStream<A2aStreamChunk>> {
        let agent = self.agent_for(&request)?;
        let parent_task_id = delegation.caller_task_id.clone().ok_or_else(|| {
            BamlRtError::InvalidArgument("delegation requires a caller task".into())
        })?;
        agent
            .provenance_writer()
            .add_event(ProvEvent::delegation_linked(DelegationLinkedSpec {
                context_id: delegation.caller_context_id.clone(),
                delegation_id: delegation.delegation_id.clone(),
                parent_task_id,
                child_task_id: delegation.child_task_id.clone(),
                caller_agent_id: delegation.caller_agent_id.clone(),
                target_agent_id: agent.agent_id().clone(),
                caller_name: self.caller_name.clone(),
                target_name: delegation.target_name.clone(),
                tool_session_id: delegation.tool_session_id.clone(),
            }))
            .await
            .map_err(|err| BamlRtError::InvalidArgument(format!("delegation lineage: {err}")))?;
        agent.handle_a2a_stream(request).await
    }
}

/// Subscriber registry the host fans produced events out through.
struct SubscriberHost {
    agents: HashMap<String, A2aAgent>,
}

fn alert_subscription_entry(package: &str) -> AgentDiscoveryEntry {
    AgentDiscoveryEntry {
        agent_package: package.into(),
        name: package.into(),
        version: "1.0.0".into(),
        agent_card: AgentCard {
            name: package.into(),
            version: "1.0.0".into(),
            content_hash: None,
            repository_version: None,
            agent_package: package.into(),
            tools: vec![],
            baml_functions: vec![],
            description: None,
            capabilities: vec![],
            tags: vec![],
            subscriptions: vec![EventSubscription {
                schema_versions: vec![EventSchemaVersion::parse(ALERT_SCHEMA).unwrap()],
                source_kinds: vec![EventSourceKind::parse(ALERT_SOURCE_KIND).unwrap()],
                ..EventSubscription::default()
            }],
        },
    }
}

impl AgentLister for SubscriberHost {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry> {
        let mut packages: Vec<&String> = self.agents.keys().collect();
        packages.sort();
        packages
            .into_iter()
            .map(|package| alert_subscription_entry(package))
            .collect()
    }
}

#[async_trait]
impl AgentRegistry for SubscriberHost {
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> baml_rt_core::Result<BusStream<A2aStreamChunk>> {
        self.agent(key)?.handle_a2a_stream(request).await
    }

    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> baml_rt_core::Result<AgentDispatchAck> {
        self.agent(key)?.handle_dispatch(request).await
    }
}

impl SubscriberHost {
    fn agent(&self, key: &AgentRouteKey) -> baml_rt_core::Result<&A2aAgent> {
        self.agents
            .get(key.agent_package.as_str())
            .ok_or_else(|| BamlRtError::AgentNotFound(key.agent_package.to_string()))
    }
}

/// The predictive-maintenance topology on one shared store: the coordinator and a
/// hook-subscribed watcher both subscribe to Grafana alerts; the coordinator
/// delegates to the investigator and slack-notify.
struct IncidentHost {
    store: Arc<SurrealProvenanceStore>,
    dispatcher: EventDispatcher,
    agent_ids: HashMap<&'static str, AgentId>,
}

async fn incident_host() -> IncidentHost {
    let runtime = HostRuntimeStore::new(test_surreal_store().await);
    let store = Arc::clone(runtime.provenance());
    let investigator = AgentSpec::new(
        "telemetry-investigator",
        SPECIALIST_JS.replace("__NAME__", "investigator"),
    )
    .build(&runtime)
    .await;
    let notifier = AgentSpec::new(
        "slack-notify",
        SPECIALIST_JS.replace("__NAME__", "slack-notify"),
    )
    .build(&runtime)
    .await;
    let router: Arc<dyn A2aRequestHandler> = Arc::new(DelegationRouter {
        caller_name: "maintenance-coordinator".into(),
        routes: HashMap::from([
            ("telemetry-investigator".to_string(), investigator.clone()),
            ("slack-notify".to_string(), notifier.clone()),
        ]),
    });
    let coordinator = AgentSpec::new("maintenance-coordinator", COORDINATOR_JS)
        .with_router(router)
        .build(&runtime)
        .await;
    let watcher = AgentSpec::new("alert-watcher", WATCHER_JS)
        .build(&runtime)
        .await;

    let agent_ids = HashMap::from([
        ("maintenance-coordinator", coordinator.agent_id().clone()),
        ("alert-watcher", watcher.agent_id().clone()),
        ("telemetry-investigator", investigator.agent_id().clone()),
        ("slack-notify", notifier.agent_id().clone()),
    ]);
    let host = SubscriberHost {
        agents: HashMap::from([
            ("maintenance-coordinator".to_string(), coordinator),
            ("alert-watcher".to_string(), watcher),
        ]),
    };
    IncidentHost {
        store,
        dispatcher: EventDispatcher::new(Arc::new(host), HostPublishService::without_provenance()),
        agent_ids,
    }
}

/// One alert as the grafana-alerts intake produces it; the intake maps the alert
/// fingerprint to `context_id`.
fn grafana_alert(context_id: &ContextId, message_id: &str) -> ProducedEvent {
    ProducedEvent {
        routing_key: AgentDispatchRoutingKey::parse("grafana:intake").unwrap(),
        schema_version: EventSchemaVersion::parse(ALERT_SCHEMA).unwrap(),
        source_kind: EventSourceKind::parse(ALERT_SOURCE_KIND).unwrap(),
        source_key: EventSourceKey::parse("grafana:pump-vibration").unwrap(),
        messages: vec![json!({ "alertname": "PumpVibrationHigh", "status": "firing" })],
        context_id: Some(context_id.clone()),
        task_id: None,
        message_id: Some(message_id.to_string()),
        producer_key: None,
        metadata: None,
    }
}

/// The root task the host assigns to `package` for this event.
fn dispatch_root_task(event: &ProducedEvent, package: &str) -> TaskId {
    let mut request = event.clone().into_dispatch_request();
    let route = AgentRouteKey::new(AgentPackageName::parse(package).unwrap());
    ensure_dispatch_task_scope(&mut request, &route);
    request.task_id.expect("host assigns a dispatch task")
}

// ---------------------------------------------------------------------------
// Graph reads
// ---------------------------------------------------------------------------

/// Every agent recorded as executing `task_id`.
async fn executing_agents(store: &SurrealProvenanceStore, task_id: &TaskId) -> BTreeSet<String> {
    let mut response = store
        .db()
        .query("SELECT VALUE to_id FROM prov_edge WHERE from_id = $from AND rel_type = $rel")
        .bind(("from", task_execution_activity_id_string(task_id.as_str())))
        .bind(("rel", "WAS_EXECUTED_BY"))
        .await
        .expect("query executing agents");
    let ids: Vec<String> = response.take(0).expect("decode executing agents");
    ids.into_iter().collect()
}

/// The task's current owner pointer (`WAS_LAST_EXECUTED_BY`).
async fn head_agent(store: &SurrealProvenanceStore, task_id: &TaskId) -> Vec<String> {
    let mut response = store
        .db()
        .query("SELECT VALUE to_id FROM prov_edge WHERE from_id = $from AND rel_type = $rel")
        .bind(("from", task_entity_id_string(task_id)))
        .bind(("rel", "WAS_LAST_EXECUTED_BY"))
        .await
        .expect("query head agent");
    response.take(0).expect("decode head agent")
}

async fn assert_single_owner(store: &SurrealProvenanceStore, task_id: &TaskId, owner: &AgentId) {
    let expected = agent_node_id_string(owner);
    let agents = executing_agents(store, task_id).await;
    assert_eq!(
        agents,
        BTreeSet::from([expected.clone()]),
        "task {} must have exactly one executing agent",
        task_id.as_str()
    );
    assert_eq!(
        head_agent(store, task_id).await,
        vec![expected],
        "task {} owner pointer must name its executing agent",
        task_id.as_str()
    );
}

#[derive(Debug)]
struct DelegationRow {
    context_id: String,
    parent_task_id: String,
    child_task_id: String,
    target_name: String,
}

async fn delegation_rows(store: &SurrealProvenanceStore) -> Vec<DelegationRow> {
    let mut response = store
        .db()
        .query("SELECT props FROM prov_node WHERE label = 'Delegation'")
        .await
        .expect("query delegations");
    let rows: Vec<Value> = response.take(0).expect("decode delegations");
    rows.iter()
        .map(|row| {
            let prop = |key: &str| {
                row.pointer(&format!("/props/{key}"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            DelegationRow {
                context_id: prop("a2a_context_id"),
                parent_task_id: prop("a2a_delegation_parent_task_id"),
                child_task_id: prop("a2a_task_id"),
                target_name: prop("a2a_delegation_target_name"),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stream helpers
// ---------------------------------------------------------------------------

fn chunk_state(response: &Value) -> Option<&str> {
    let chunk = response.pointer("/result/chunk")?;
    chunk
        .pointer("/statusUpdate/status/state")
        .or_else(|| chunk.pointer("/task/status/state"))
        .and_then(Value::as_str)
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    )
}

/// A chat message into `context_id`; with `task_id` it targets that existing task.
fn chat_request_for_task(
    message_id: &str,
    text: &str,
    context_id: &ContextId,
    task_id: Option<&TaskId>,
) -> Value {
    send_stream_request_with_task(
        message_id,
        text,
        baml_rt_core::correlation::generate_correlation_id().as_str(),
        Some(context_id.clone()),
        task_id.cloned(),
    )
}

fn chat_request(message_id: &str, text: &str, context_id: &ContextId) -> Value {
    chat_request_for_task(message_id, text, context_id, None)
}

/// The task a stream frame belongs to.
fn task_id_of(frame: &Value) -> Option<TaskId> {
    let chunk = frame.pointer("/result/chunk")?;
    chunk
        .pointer("/task/id")
        .or_else(|| chunk.pointer("/statusUpdate/taskId"))
        .or_else(|| chunk.pointer("/artifactUpdate/taskId"))
        .and_then(Value::as_str)
        .map(|id| TaskId::from_external(ExternalId::new(id.to_string())))
}

fn subscribe_request(task_id: &TaskId) -> A2aWireRequest {
    A2aWireRequest::from(json!({
        "jsonrpc": "2.0",
        "method": "tasks.subscribe",
        "params": { "id": task_id.as_str(), "stream": true },
        "id": baml_rt_core::correlation::generate_correlation_id().as_str(),
    }))
}

/// Owner agent holding one live task, plus a second agent on the same store.
struct LiveTask {
    store: Arc<SurrealProvenanceStore>,
    owner: A2aAgent,
    other: A2aAgent,
    context_id: ContextId,
    task_id: TaskId,
    release: Arc<Notify>,
    owner_run: tokio::task::JoinHandle<Vec<Value>>,
}

async fn live_task() -> LiveTask {
    let runtime = HostRuntimeStore::new(test_surreal_store().await);
    let store = Arc::clone(runtime.provenance());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut owner_spec = AgentSpec::new("telemetry-investigator", HOLDING_JS);
    owner_spec
        .manager
        .register_tool(HoldTool {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        })
        .await
        .expect("register b1/hold");
    let owner = owner_spec.build(&runtime).await;
    let other = AgentSpec::new("maintenance-coordinator", ECHO_JS)
        .build(&runtime)
        .await;

    let context_id = ContextId::from(format!("ctx-b1-{}", uuid::Uuid::new_v4()).as_str());
    let request = chat_request("b1-owner-msg", "start work", &context_id);
    let mut stream = owner
        .handle_a2a_stream(A2aWireRequest::from(request))
        .await
        .expect("owner accepts the message");
    let first = timeout(STEP_TIMEOUT, stream.next())
        .await
        .expect("owner stream starts")
        .expect("owner stream is open")
        .into_inner();
    let task_id = task_id_of(&first)
        .unwrap_or_else(|| panic!("first owner frame must carry the task id: {first}"));
    let mut owner_run = tokio::spawn(async move {
        let mut frames = vec![first];
        frames.extend(
            stream
                .map(A2aStreamChunk::into_inner)
                .collect::<Vec<_>>()
                .await,
        );
        frames
    });
    tokio::select! {
        () = entered.notified() => {}
        frames = &mut owner_run => {
            panic!("owner run ended before reaching b1/hold: {frames:#?}");
        }
        () = tokio::time::sleep(STEP_TIMEOUT) => {
            panic!("owner task never reached b1/hold");
        }
    }

    LiveTask {
        store,
        owner,
        other,
        context_id,
        task_id,
        release,
        owner_run,
    }
}

/// Subscribe to the live task through `via`, release it, and return every frame
/// the subscription delivers before closing.
async fn subscribe_until_closed(live: &LiveTask, via: &A2aAgent) -> Vec<Value> {
    let mut stream = via
        .handle_a2a_stream(subscribe_request(&live.task_id))
        .await
        .expect("tasks.subscribe opens");
    let snapshot = timeout(STEP_TIMEOUT, stream.next())
        .await
        .expect("subscription sends the task snapshot")
        .expect("subscription is open")
        .into_inner();
    assert!(
        snapshot.pointer("/result/chunk/task").is_some(),
        "first frame must be the task snapshot read from the shared graph: {snapshot}"
    );

    live.release.notify_one();
    let mut frames = vec![snapshot];
    let collected = timeout(STEP_TIMEOUT, async {
        while let Some(chunk) = stream.next().await {
            frames.push(chunk.into_inner());
        }
    })
    .await;
    assert!(
        collected.is_ok(),
        "subscription must close after the task finishes; frames so far: {frames:#?}"
    );
    frames
}

fn assert_reports_completion(frames: &[Value]) {
    let states: Vec<&str> = frames.iter().filter_map(chunk_state).collect();
    assert!(
        states.last().is_some_and(|state| is_terminal(state)),
        "subscription must deliver the task's terminal state; states: {states:?}, frames: {frames:#?}"
    );
    assert!(
        states.contains(&"TASK_STATE_COMPLETED"),
        "subscription must report COMPLETED; states: {states:?}"
    );
}

// ---------------------------------------------------------------------------
// I-INCIDENT-CONTEXT / I-ONE-OWNER
// ---------------------------------------------------------------------------

/// One Grafana alert, two subscribers, two delegated children, one context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_alert_produces_a_task_forest_with_one_owner_per_task() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let host = incident_host().await;
    let context_id = ContextId::from(format!("ctx-b1-{}", uuid::Uuid::new_v4()).as_str());
    let alert = grafana_alert(&context_id, "alert-1");

    let outcome = timeout(STEP_TIMEOUT, host.dispatcher.deliver_event(alert.clone()))
        .await
        .expect("delivery finishes")
        .expect("delivery succeeds");
    assert_eq!(outcome.subscribers_matched, 2, "{outcome:?}");
    assert_eq!(outcome.subscribers_accepted, 2, "{outcome:?}");

    let coordinator_root = dispatch_root_task(&alert, "maintenance-coordinator");
    let watcher_root = dispatch_root_task(&alert, "alert-watcher");
    assert_ne!(coordinator_root, watcher_root);
    assert_single_owner(
        &host.store,
        &coordinator_root,
        &host.agent_ids["maintenance-coordinator"],
    )
    .await;
    assert_single_owner(&host.store, &watcher_root, &host.agent_ids["alert-watcher"]).await;

    let delegations = delegation_rows(&host.store).await;
    assert_eq!(
        delegations.len(),
        2,
        "coordinator delegates exactly twice: {delegations:#?}"
    );
    for row in &delegations {
        assert_eq!(row.context_id, context_id.as_str(), "{row:?}");
        assert_eq!(
            row.parent_task_id,
            coordinator_root.as_str(),
            "children hang under the coordinator's root: {row:?}"
        );
        assert_ne!(
            row.parent_task_id,
            watcher_root.as_str(),
            "the watcher's root has no children: {row:?}"
        );
        assert_ne!(row.child_task_id, coordinator_root.as_str());
    }
    let targets: BTreeSet<&str> = delegations
        .iter()
        .map(|row| row.target_name.as_str())
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from(["slack-notify", "telemetry-investigator"])
    );
    for row in &delegations {
        let child = TaskId::from_external(ExternalId::new(row.child_task_id.clone()));
        let owner = &host.agent_ids[row.target_name.as_str()];
        assert_single_owner(&host.store, &child, owner).await;
    }
}

/// A repeat alert maps to the same context and adds one root task per subscriber.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_alert_reuses_the_context_and_adds_root_tasks() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let host = incident_host().await;
    let context_id = ContextId::from(format!("ctx-b1-{}", uuid::Uuid::new_v4()).as_str());
    let first = grafana_alert(&context_id, "alert-1");
    let repeat = grafana_alert(&context_id, "alert-2");

    for alert in [&first, &repeat] {
        let outcome = timeout(STEP_TIMEOUT, host.dispatcher.deliver_event(alert.clone()))
            .await
            .expect("delivery finishes")
            .expect("delivery succeeds");
        assert_eq!(outcome.subscribers_accepted, 2, "{outcome:?}");
    }

    let mut roots = BTreeSet::new();
    for alert in [&first, &repeat] {
        for package in ["maintenance-coordinator", "alert-watcher"] {
            let root = dispatch_root_task(alert, package);
            assert_single_owner(&host.store, &root, &host.agent_ids[package]).await;
            roots.insert(root.as_str().to_string());
        }
    }
    assert_eq!(
        roots.len(),
        4,
        "one root per subscriber per alert: {roots:?}"
    );

    let delegations = delegation_rows(&host.store).await;
    assert_eq!(delegations.len(), 4, "{delegations:#?}");
    assert!(
        delegations
            .iter()
            .all(|row| row.context_id == context_id.as_str()),
        "every delegation stays in the incident context: {delegations:#?}"
    );
    for alert in [&first, &repeat] {
        let root = dispatch_root_task(alert, "maintenance-coordinator");
        let children = delegations
            .iter()
            .filter(|row| row.parent_task_id == root.as_str())
            .count();
        assert_eq!(
            children, 2,
            "each coordinator root has its own subtree: {delegations:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// I-SUBSCRIBE-ANY-AGENT
// ---------------------------------------------------------------------------

/// Control: subscribing through the task's owner works today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_through_the_owner_streams_live_frames_and_closes() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let live = live_task().await;
    let frames = subscribe_until_closed(&live, &live.owner).await;
    assert_reports_completion(&frames);
    live.owner_run.await.expect("owner run finishes");
}

/// Every agent listens on the host's one broadcaster, so a subscription through
/// a non-owner receives the frames the owner commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_through_another_agent_streams_live_frames_and_closes() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let live = live_task().await;
    let frames = subscribe_until_closed(&live, &live.other).await;
    assert_reports_completion(&frames);
    live.owner_run.await.expect("owner run finishes");
}

// ---------------------------------------------------------------------------
// I-FOREIGN-TASK-WRITE
// ---------------------------------------------------------------------------

/// Another agent writing into the shared context gets a task of its own and
/// leaves the existing task's owner alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_write_by_another_agent_creates_its_own_task() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let live = live_task().await;
    let request = chat_request("b1-other-msg", "joining the incident", &live.context_id);
    let frames: Vec<Value> = timeout(STEP_TIMEOUT, async {
        live.other
            .handle_a2a_stream(A2aWireRequest::from(request))
            .await
            .expect("context write accepted")
            .map(A2aStreamChunk::into_inner)
            .collect()
            .await
    })
    .await
    .expect("context write finishes");
    assert!(
        frames.iter().all(|frame| frame.get("error").is_none()),
        "{frames:#?}"
    );
    let other_task = frames
        .iter()
        .find_map(task_id_of)
        .unwrap_or_else(|| panic!("context write must report its task: {frames:#?}"));
    assert_ne!(
        other_task, live.task_id,
        "the writer gets a task of its own"
    );

    assert_single_owner(&live.store, &other_task, live.other.agent_id()).await;
    assert_single_owner(&live.store, &live.task_id, live.owner.agent_id()).await;

    live.release.notify_one();
    live.owner_run.await.expect("owner run finishes");
}

/// Bindings are appended without a conflict check. Today a foreign write is
/// accepted, records the writer as a second executing agent, and leaves the task
/// with no owner pointer at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "known defect: a write to another agent's task is accepted and re-binds the task"]
async fn task_write_by_another_agent_is_rejected() {
    let _permit = e2e_serial_gate().acquire().await.expect("e2e gate");
    let live = live_task().await;
    let request = chat_request_for_task(
        "b1-foreign-msg",
        "writing into someone else's task",
        &live.context_id,
        Some(&live.task_id),
    );
    let outcome = timeout(STEP_TIMEOUT, async {
        match live
            .other
            .handle_a2a_stream(A2aWireRequest::from(request))
            .await
        {
            Ok(stream) => Ok(stream
                .map(A2aStreamChunk::into_inner)
                .collect::<Vec<_>>()
                .await),
            Err(err) => Err(err),
        }
    })
    .await
    .expect("foreign write finishes");

    let rejected = match &outcome {
        Err(_) => true,
        Ok(frames) => frames.iter().any(|frame| frame.get("error").is_some()),
    };
    // Check ownership before asserting rejection, so a failure shows the re-bind.
    let agents = executing_agents(&live.store, &live.task_id).await;
    let head = head_agent(&live.store, &live.task_id).await;
    live.release.notify_one();
    live.owner_run.await.expect("owner run finishes");

    let owner = agent_node_id_string(live.owner.agent_id());
    assert_eq!(
        (agents, head),
        (BTreeSet::from([owner.clone()]), vec![owner]),
        "a foreign write must not re-bind the task (executing agents, owner pointer)"
    );
    assert!(
        rejected,
        "a write to another agent's task must be rejected: {outcome:#?}"
    );
}
