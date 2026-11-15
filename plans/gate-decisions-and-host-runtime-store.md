# Gate decisions and the host-wide runtime store

Status: draft plan, rev 2 (2026-11-13). Two independent workstreams; either can land first.

## Premise

Karakara hosts several cooperating agents per runner. Flows start at an **ingress**: a
registered tool-owned event source (webhook intake or poll producer) or a chat channel.
Reference flow (predictive-maintenance demo):

```
Grafana alert ──webhook──▶ grafana-alerts intake ── fingerprint → context_id (mapping.rs)
                                   │ ProducedEvent (grafana.alert.v1)
                                   ▼
                     maintenance-coordinator.onDispatch   (subscribes via manifest)
                        │ system/internal_a2a               │ system/internal_a2a
                        ▼                                   ▼
               telemetry-investigator               slack-notify
               (child task, DelegationLinked)       (child task, DelegationLinked)
```

Operators already decide which ingresses to trust (they register and filter hooks). That
decision is the trust anchor; the runtime must not re-ask a human by default, and must not
put approval semantics into the A2A transport.

---

## Part A — Gate decisions resolved from the flow's ingress

**Status: functionally complete (2026-11-14).** The remaining items under *Deferred* are
optimizations and test coverage, not new functionality.

### What the code does today (verified)

| Step | Where | Behavior |
|---|---|---|
| Gate trips | `baml-rt-karakara/src/interceptor.rs:166-174` | `set_pending` into a process-global `OnceLock` store, returns `RequireAuthorization(prompt)` |
| Surface | `baml-rt-quickjs/src/quickjs_bridge/stream.rs:612-655` | Injects `INPUT_REQUIRED` **only if a live chat stream exists**; otherwise `debug` log and return |
| Decide | `a2a_transport.rs:2034-2052` → `pending_auth.rs:76-88` | Any live-stream resume parses free text; anything not `deny`/`reject` (or missing text → `"approve"`) **grants** |
| Consume | `interceptor.rs:265` | `is_granted` matches `(agent_id:task_id, tool, args fingerprint)` once |

Consequences: in the Grafana flow (`onDispatch`, no chat stream) a tier-3 tool just fails and
leaves a stale pending entry; in chat flows approval is a side effect of *any* resume; nothing
records who or what decided; state is in-process memory, not durable or cluster-safe;
task-less scopes share one key (`agent_id:`).

### Target design

1. **Every flow carries its origin.** The context is linked in the graph to the ingress that
   opened it (`IngressReceived` → context edge; intake key / producer key / source kind). Gate
   resolution reads it by traversing edges (graph-first), never from request metadata.
2. **No new tier, config or setting.** Registering an ingress is the operator's trust decision;
   the gate outcome is derived from the flow's origin in the graph:
   - Flow opened by a registered machine ingress (webhook intake / producer, e.g. Grafana) and
     no chat channel in the flow → tier-3 passes automatically, recorded with the ingress as
     principal.
   - Flow opened by a chat ingress (Slack conversation, live A2A chat stream) → tier-3 is
     confirmed in that channel (point 3). Egress posts (e.g. slack-notify) do not bind a channel
     and do not open flows; ingress/egress dedup is the channel integration's job.
   - No registered ingress and no channel → deny with a recorded reason.
   - Each ingress event opens its own context, so a flow has exactly one origin. Connecting a
     chat channel is the same kind of trust decision as registering an ingress: a person chatting
     into a machine-started flow joins it and is trusted; no per-person or per-message approval.
   Existing karakara settings (`enabled`, `mode`, `enforce_min_tier`, per-package overrides) and
   tool-declared tiers are unchanged.
3. **Channel confirmation.** Anyone who can act in the flow's channel can confirm; the runtime
   keeps **no approver list**. Channel access is the authorization, owned by Slack (or the other
   channel), not by A2A.
4. **Decisions are typed events from the channel integration**, e.g. a Slack interactive
   button → `GateDecided`. Free text on an A2A resume never decides a gate. A2A carries at most a
   typed "gate resolved, continue" signal.
5. **Durable and auditable.** `GateRequested` / `GateDecided` are provenance events.
   `principal` is `Ingress { ingress_key }` or `Channel { channel, actor }`. Pending gates
   survive restart and lease moves; they expire to `Deny`.
6. **One decision covers identical retries.** A decision is keyed by context, task, tool and
   exact args (`gate_id`). Once allowed, an identical call in the same task passes without
   asking again; a deny or expiry likewise stands for that call in that task.

### PR breakdown

**PR A1 — fail closed, stop text-parsing (small, ship first)**
- [x] Remove gate resolution from the live-stream resume path.
- [x] Delete `resolve_on_resume`, `grant_on_resume`, `is_gate_denial`, `grant_pending_gate_authorization`
      (`pending_auth.rs` removed).
- [x] No channel to surface to → gate resolves `Deny` with a structured reason.
- [x] `gate_id` includes context and task; task-less scopes are refused.
- [x] A resolver error denies the call. The interceptor pipeline treats interceptor errors as
      "continue", which would otherwise run the tool (`baml-rt-interceptor/src/interceptor.rs`).
- [x] Tests: resume with arbitrary text never grants (`test_chat_text_never_grants_tier3_authorization`);
      no resolver denies with reason (`grounded_tier3_without_host_resolver_denies_with_reason`).

**PR A2 — ingress origin (makes the Grafana flow work automatically)**
- [x] Origin from the graph: the existing `HostSourcePollRecorded` row (now carrying the typed
      channel) is reached via `SCOPED_TO` edges from the context (`read_gate_flow_origin`); no
      separate edge was needed. The poll write now fails the poll instead of only logging.
- [x] Gate resolution applies the rule in design point 2 and emits `GateRequested` + `GateDecided`.
- [x] Global `PendingGateAuthStore` replaced by the durable ledger (`surreal_store/gate_ledger.rs`).
- [x] Decisions are a create-only `gate_decision` record per `gate_id`: the first writer (click,
      resolver, expiry sweep, any runner) wins; `GateDecided` is the winner's audit event.
- [x] A request without a channel is decided again rather than left pending; malformed expired
      requests are quarantined as Deny so they cannot block the sweep; expiry is required.
- [x] Tests: machine ingress auto-approves tier-3 over A2A and records an `Ingress` principal
      (`machine_ingress_auto_approves_tier3_in_the_a2a_path`); origin from graph-linked ingress.

**PR A3 — channel confirmation**
- [x] Post confirmation to the flow's channel (Slack interactive message via `tools/slack`), 10 s
      request timeout. A create-only `gate_confirmation_claim` lets exactly one concurrent caller
      post, across runners.
- [x] `block_actions` → `GateDecided { principal: Channel { channel, actor } }`; invalid clicks are
      recorded as `GateActionRejected` and never block the event queue.
- [x] The decision is delivered into the gate's context/task; an identical retry passes (point 6).
- [x] Expiry → `Deny` (resolver and poll-cycle sweep). Docs in `how-to-write-agents.md` §3.4b;
      `baml_rt.gate.outcome_total` metrics.

### Deferred (optimizations, not blocking Part A)

- [ ] Typed continue signal to the suspended run (stream- or dispatch-originated), routed to the
      lease owner, so a click resumes the run without relying on the agent's retry.
- [ ] Test: Grafana webhook → coordinator `onDispatch` → delegated child hits tier-3 → auto-approved.
- [ ] Tests with a live Slack agent: click on a suspended run; restart / second runner sees the decision.
- [ ] Move Slack confirmation posting behind a host trait; read the bot token through the secret
      resolver instead of `SLACK_BOT_TOKEN`; drop the `baml-rt-a2a` → `baml-tools-slack` dependency.
- [ ] Gate TTL is hardcoded at 15 min.

### Prerequisite risk: ingress trust needs authenticated ingress

`GrafanaWebhookIntake::auth_tier()` is `WebhookAuthTier::Public` (`grafana-alerts/src/intake.rs:84`).
If ingress trust auto-approves irreversible tools, anyone who can reach that path can forge an
alert and trigger them. Deferred by decision (2026-11-13); follow-up: authenticate machine
intakes before they can authorize irreversible tools.

---

## Part B — One runtime store per host, incident-level progress

### What the code does today (verified)

- The Surreal connection is shared (`agent_package.rs:438,456`), but each agent wraps it in its own
  `SurrealRuntimeStore` → own `TaskSubgraphStore` → **own `TaskUpdateBroadcaster`**
  (`task_subgraph_store.rs:182-187`); `DefaultTaskHandler` subscribes on that.
- `agent_id` / `boot_event_id` are fixed in the wrapper; every `upsert`, task-carrying
  `apply_task_chunk`, and scoped user-message insert re-emits an executing-agent binding.
  `bind_task_executing_agent` appends with no conflict check (`store.rs:138`).
- `internal_a2a` mints a **fresh child task per delegation** (`TaskId::for_delegated_child`) and
  records `DelegationLinked { parent_task_id, child_task_id, caller, target }` (`routing.rs:507`).
  So one incident = one `context_id` + a tree of per-agent tasks.
- `AgentBooted` is written after build/init and its failure is only logged (`agent_package.rs:592-597`).

### Task model decision: one task per agent, one tree per incident

Keep one executing agent per task. A task's lifecycle (`WORKING → COMPLETED`) describes one
agent's unit of work; "investigator finished" is not "incident resolved". Sharing a single
task-id across coordinator, investigator and slack-notify would make the state ambiguous (who
sets `COMPLETED`?) and attribution meaningless.

The shared tracking unit already exists: the **incident context** plus the **delegation tree**
rooted at the coordinator's task. What is missing is a way to *watch* it as one thing.

Consequence for cross-agent writes: **reject** writes to a task by a principal other than its
bound agent. Contributions happen through child tasks.

### Is the current implementation correct?

Per-agent tasks are routed to their owner, so subscribe works **per task**. It breaks for
multi-agent tracking:

- `tasks.subscribe` handled by a non-owner agent (`handlers.rs:174-290` accepts any task in the
  shared graph) replays history, then listens on its own broadcaster — which never receives the
  owner's frames **and never retires** — so it hangs.
- There is no way to follow the whole incident (root task + all delegated children) live.

### Target design

- **`HostRuntimeStore`**: one per runner, owns the single `TaskSubgraphStore` and single
  `TaskUpdateBroadcaster`. Built in the runner, injected with `A2aAgentBuilder::with_host_store`.
- **`AgentStoreView`**: per-agent handle (`Arc<HostRuntimeStore>` + `ExecutionPrincipal { agent_id,
  boot_event_id }`) implementing the existing traits. Per-agent *state* goes away.
- **Bind once at task creation; reject foreign writes.**
- **Incident view**: the existing context-level observation stream, fed from the shared host
  broadcaster (B4). `tasks.subscribe` stays per-task.
- **Cross-runner**: subscribe is forwarded to the lease owner, like other requests.
- **Boot first, fatally**: `AgentBooted` written before any phase that can bind; failure aborts
  boot; `ExecutionPrincipal` only constructible from a committed boot.

### PR breakdown

**PR B1 — prove the failure modes (tests only)**
- [ ] Two agents, one host: B writes a task, `tasks.subscribe` via A → expect live frames + close (fails today).
- [ ] Grafana-shaped flow: coordinator → investigator + slack-notify; assert one binding per task
      and a correct delegation tree under one context.
- [ ] Injected `AgentBooted` write failure → boot must fail (fails today).

**PR B2 — host store + shared broadcaster**
- [ ] `HostRuntimeStore` in `baml-rt-a2a`; runner builds one; `with_host_store` replaces
      `with_surreal_store` + `with_boot_event_id`. `HostRuntimeStore::in_memory()` for tests.
- [ ] `SurrealRuntimeStore` → `AgentStoreView`; `DefaultTaskHandler` uses the host broadcaster.

**PR B3 — binding, boot ordering**
- [ ] Bind at creation only; reject writes from a non-bound principal.
- [ ] `AgentBooted` before `register_tools_phase`; fatal on failure. Drop `Option` boot plumbing.

**PR B4 — incident view via the existing observation stream (no new A2A method)**

`tasks.subscribe` stays per-task (A2A spec). The context-level view is the operator observation
stream (`ObservationUpdate` on one host `broadcast` channel, consumed by SSE handlers in
`baml-rt-api/src/handlers.rs` with a 600 s per-client limit — keep that).

Current producers (verified):

| Producer | Covers | Kinds |
|---|---|---|
| `NotifyingProvenanceWriter` (wraps the agent's writer at `a2a_transport.rs:1496`) | Writes via `agent.provenance_writer()` / `wire_provenance_subsystems`: LLM/tool completions, plans, compaction, dispatch context link, delegation lineage | per event (`observation/invalidate.rs`) |
| Forwarder (`services/observation_events.rs`) | Every per-agent `update_tx` `TaskUpdateEvent` (status/artifact) | hardcoded `TRANSCRIPT \| OPS` |
| — none — | `MessageReceived`/`MessageSent` (`SurrealRuntimeStore` writes on the raw provenance Arc), `HostDispatchAccepted/Rejected`, `HostSourcePollRecorded` (`HostIngressRecorderImpl` on the raw store), boot | — |

Problems: two producers with different kind semantics; `invalidate.rs` has arms for message
and ingress events that never fire; the forwarder dies after 620 s (it is built once per
process, `lib.rs:594`) and snapshots the agent list at start. Per-agent `update_tx` exists only
to feed the forwarder, and is a second task-update broadcast next to `TaskUpdateBroadcaster`.

- [ ] Notify at one seam: the host store's commit path (B2) emits `ObservationUpdate` for every
      committed `ProvEvent` using `observation_kinds_for_event`. Covers task, message, ingress,
      LLM/tool writes uniformly.
- [ ] Route `HostIngressRecorderImpl` and runner-level writes through the same host store.
- [ ] Delete `NotifyingProvenanceWriter`, the forwarder, `subscribe_task_update_receivers`, and
      per-agent `update_tx` / `BroadcastEventEmitter` if nothing else needs them.
- [ ] Test: each event family produces exactly one `ObservationUpdate` with the mapped kinds,
      including after 10+ minutes uptime and for an agent deployed after boot.
- [ ] Update `docs/reference/agent-runner.md` if wire shapes change.

---

## Open questions

None currently.
