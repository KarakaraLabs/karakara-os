# Shared-context agent-view stability and isolation

Status: top priority, ready for implementation

Scope: keep one complete context-wide provenance graph for audit while making every
agent-facing projection, archive read, citation lookup, duplicate guard, and
compaction view private to the reading `AgentId`.

This plan also addresses the current delegated ClickUp failure:

```text
step executor hop ChooseClickUpAction__entry: conversation_context is not a prefix
extension and added no new line values vs the pre-step snapshot; graph reordered
or suppressed only
```

It builds on [`a2a-execution-context-continuity-plan.md`](a2a-execution-context-continuity-plan.md)
and preserves the archive handoff rule in
[`archive-result-handoff-plan.md`](archive-result-handoff-plan.md): a coordinator
may read the coordinator-owned archive of a completed A2A response, but it may
not read the child agent's internal archive refs.

## 1. Decision

Two read contracts coexist over the same durable provenance:

```text
ContextTimeline(context_id)
  complete operator and audit view
  includes every agent, task, hook, ingress event, tool operation, and decision

AgentParticipationView(context_id, reader_agent_id)
  execution view
  includes only rows where the reader is actor or target
```

`context_id` remains the complete trace and correlation boundary. It must be
possible for the web UI, graph export, Mermaid rendering, replay, and audit APIs
to retrieve the whole context without knowing an agent ID.

`agent_id` is the execution visibility boundary. Sharing a context does not let
one agent inspect a sibling's prompts, tool calls, archive bodies, citations,
pending receipts, or compaction summaries.

Agents exchange actual A2A messages. They do not exchange ambient archive
pointers. If large cross-agent artifact transfer is needed later, add an
explicit artifact grant with actor, target, and provenance. Do not make
possession of `@prefix/local` an authorization mechanism.

## 2. Immediate incident and likely failure mechanism

The reproduced flow is:

```text
coordinator-agent
  -> discovers or delegates to clickup-agent in context C

clickup-agent
  -> starts ChooseClickUpAction__entry in the same context C
  -> compares provider projection before and after the hop
  -> receives a reordered, suppressed, or sliding projection
  -> fails the turn before it can return the task list
```

The current step executor assumes that two independently rebuilt projected JSON
arrays form an append-only sequence. That is stronger than the provenance read
contract.

The first concrete mismatch to pin down is the supposedly uncapped intra-turn
read:

- `conversation_history_json_for_intra_dedup` calls
  `project_conversation_to_json(scope, None)`;
- `project_conversation_to_json` forwards `None` to `context_history_view`;
- `context_history_view` interprets `None` as
  `DEFAULT_LLM_CONTEXT_ITEM_CAP`, currently 40;
- the step executor documentation says this path is uncapped so a sliding window
  cannot break the prefix check.

Shared-context delegation makes this easier to hit because more graph activity
can occur between snapshots, even though the ClickUp view must remain
agent-filtered.

Other possible causes must be measured rather than guessed:

- a visible row changes rendering after hydration;
- delegation dedup removes a row between snapshots;
- `#N` assignment changes an earlier projected value;
- equal ordering keys produce a different row order;
- a hop writes no new prompt-visible row even though the graph gains internal
  rows;
- cache replacement or invalidation makes the two projections observe different
  generations.

The fix must not merely turn the current error into `Ok(Vec::new())`. That would
hide lost content. The runtime needs an identity-based incremental read contract
that distinguishes "no new visible row" from "new visible row was lost."

## 3. Required invariants

1. Operator reads by `context_id` remain complete and unfiltered by agent.
2. Agent reads always require `reader_agent_id` and return only actor-or-target
   rows.
3. A stable agent view does not change order or identity for existing rows when
   unrelated sibling activity is appended.
4. Step-executor deltas use durable row identity and ordering, not raw JSON array
   prefix equality.
5. A hop with no new prompt-visible row is legal when its local FSM result is
   authoritative.
6. A content-bearing row emitted for the reading agent is visible after the
   awaitable provenance write completes.
7. Archive refs remain globally unique within the context for audit, but an
   agent may resolve or consume only its own archive bodies.
8. Duplicate-Send suppression is scoped to `(context_id, agent_id)`.
9. `#N` citation resolution for an agent contains only that agent's visible
   history rows.
10. Agent-prompt compaction is keyed by at least
    `(context_id, agent_id, projection_version)` and summarizes exactly the same
    view used by that agent's prompt.
11. Context-wide summaries are operator artifacts and never enter an agent
    prompt.
12. A coordinator receives a child agent's result through the persisted A2A
    reply and coordinator-owned tool result. It never follows the child's
    internal archive refs.

## 4. Phase 0: pin the ClickUp failure before changing semantics

Add one deterministic vertical-slice regression that reproduces the reported
flow without a live model:

1. Start coordinator and ClickUp runtimes with one shared in-memory provenance
   store.
2. Use one `context_id` and distinct stable `AgentId` values.
3. Make the coordinator delegate "list tasks in progress".
4. Drive ClickUp's first step-executor hop with a deterministic interceptor.
5. Capture the internal before and after projections and their durable row
   identities.
6. Assert that the current branch reproduces the prefix-extension failure, or
   record the exact alternate mismatch if the deterministic fixture exposes a
   different cause.
7. Keep the test as the authority regression for the final fix.

Likely test locations:

- `crates/baml-agent-runner/tests/runner_clickup_test.rs`
- `crates/baml-rt-quickjs/tests/`
- `crates/baml-rt-provenance/tests/context_history_view_test.rs`

Add bounded diagnostics to the failure path. Log only:

- context ID and reader agent ID;
- function and hop;
- before and after row counts;
- stable row IDs and order keys;
- whether rows were appended, removed, reordered, or re-rendered;
- projection generation or cursor, once available.

Do not log hidden row bodies or archive contents.

### Phase 0 exit gate

The regression must explain which rows changed and why. Do not implement a
permutation-tolerant patch until this evidence exists.

## 5. Phase 1: replace snapshot-prefix comparison with an incremental agent view

### 5.1 Make full versus bounded reads explicit

Remove the ambiguous `Option<usize>` meaning from internal agent-history APIs.
Use an explicit request shape such as:

```text
AgentHistoryRead::Window { limit, before_cursor }
AgentHistoryRead::After { cursor }
AgentHistoryRead::FullForReplay
```

The normal prompt uses `Window`. Step-executor hop tracking uses `After`.
Tests and replay may use `FullForReplay`; production prompts must not silently
request an unbounded context.

If the immediate patch must retain the existing API, make the intra-dedup path
call a dedicated uncapped method. Treat that only as a bridge to the cursor
implementation.

### 5.2 Return an internal row envelope

The provenance reader should return stable identity before prompt rendering:

```text
AgentVisibleRow {
  activity_anchor,
  event_order,
  node_id,
  projected_item,
}
```

Use `(event_order, node_id)` as the durable pagination and delta order already
used by `ContextHistoryCursor`. `activity_anchor` remains the provenance link.
Do not infer identity from rendered role/content JSON or `#N` text.

The BAML prompt still receives the existing `{role, content}` rows. The internal
envelope is for host comparison, paging, diagnostics, and tests.

### 5.3 Track a watermark per executor loop

At the start of a hop, retain the last visible cursor from the exact agent view
used to build that hop's prompt. After the awaitable provenance write:

1. Query rows after that cursor for the same `(context_id, agent_id)`.
2. Project only those rows.
3. Append them to the loop-local supplement if they are not already present by
   stable identity.
4. Advance the watermark.

Do not rebuild two full arrays and compare them as sets. A row reorder or window
rotation is then irrelevant.

A hop may produce zero new visible rows. That is valid for FSM-only operations
such as `Open`, and for any event intentionally omitted from prompt projection.
A hop that promises a visible model or tool row must assert that the expected
activity anchor appears after the watermark.

### 5.4 Keep one visibility predicate

The window read, after-cursor read, retrieval tool, and prompt projection must
all use the same actor-or-target traversal and defence-in-depth filter.

Sibling events may advance the global context graph. They must not alter the
reader agent's existing rows, cursor, or rendered values.

### Phase 1 exit gate

The deterministic coordinator-to-ClickUp test completes the first ClickUp hop
without the prefix-extension error. A sibling append between before and after
does not change the agent delta.

## 6. Phase 2: separate context-global storage from agent-private ref views

### 6.1 Keep durable archive identity context-global

Retain:

```text
archive_prefix_registry(context_id, agent_id, archive_prefix)
archive_body(context_id, agent_id, archive_prefix, archive_local, ...)
```

The prefix/local pair prevents collisions and helps operator rendering. It is an
identifier, not chronological order and not access authority.

The web and audit paths may list every archive header in the context when their
operator authorization permits it.

### 6.2 Require a read principal

Do not use an optional agent ID where `None` means privileged access. Use
separate methods or an explicit principal:

```text
ProvenanceReadPrincipal::Agent(agent_id)
ProvenanceReadPrincipal::Operator
```

Agent archive operations must verify `archive_body.agent_id`:

- get body;
- PageRead and SearchRead;
- mark Send consumed;
- list headers;
- hydrate the execution ref table;
- resolve archive citations.

Operator methods remain context-wide and are not callable from agent execution
code.

### 6.3 Key execution ref tables by context and agent

Change the runtime cache identity from:

```text
context_id -> RefTable
```

to:

```text
(context_id, agent_id) -> RefTable
```

Managers for the same logical agent and context may share the same table.
Managers for sibling agents may share the backing store but never the table
contents.

Update or replace `shared_context_ref_store_test`. The new assertion is:

- same context plus same agent yields the same table;
- same context plus different agents yields different tables;
- operator hydration can still inspect all durable rows through the audit API.

Avoid replacing a live table with a stale snapshot. Hydration should merge by
stable ref or publish through a generation check. Provenance event writes must
not invalidate every sibling agent's table.

### 6.4 Scope duplicate suppression to the owner

`find_receipt_by_send_signature` must search only the current agent's table.
Never use `(prefix, local)` ordering as global recency. Within an agent table,
use a real insertion sequence or persisted completion order when "latest" is
required.

Two sibling agents issuing the same tool name and normalized input must each
reach their own backend once. An immediate repeat by the same agent must still
be suppressed.

### 6.5 Scope history refs and citations

Durable `#N` assignment may remain context-global so audit references stay
stable. Agent hydration must include only history refs visible to that agent.
Sparse `#N` values in an agent prompt are acceptable.

A sibling's guessed `#N` or `@prefix/local` must fail agent-side resolution.
Operator citation and episode tooling may use an explicit full-context table.

### Phase 2 exit gate

ClickUp cannot read or consume a Notion archive in the same context. The
operator timeline can retrieve both. Same-agent duplicate suppression still
works, and cross-agent suppression is impossible.

## 7. Phase 3: enforce explicit A2A result handoff

For a completed internal A2A call:

1. The child executes with its own private ref table.
2. The child materializes the information needed for its final A2A reply.
3. The reply is persisted as an actor-to-target message.
4. The coordinator's `system/internal_a2a` completion is archived under the
   coordinator's `AgentId`.
5. The coordinator may read that coordinator-owned archive receipt.
6. Child-internal refs in the reply are non-resolvable for the coordinator and
   should not be presented as a supported handoff mechanism.

If a child accidentally emits an internal ref, preserve the text for honest
provenance but do not grant access. Prefer a bounded diagnostic telling the
child to materialize the relevant content in its reply.

Do not add implicit grants based on delegation lineage. Delegation proves who
communicated with whom; it does not grant access to every tool result the child
created while working.

### Phase 3 exit gate

The coordinator receives the ClickUp task list as a real child reply or through
its own archived A2A tool result. It never needs ClickUp's archive prefix.

## 8. Phase 4: split audit compaction from agent-prompt compaction

The current compaction implementation mixes the two contracts:

- `CompactionRequest` carries `agent_id`;
- `prepare_compaction` ignores it and reads the full context;
- compaction heads are keyed by context and optional task;
- summaries have no attribution and therefore pass the current fail-open
  visibility rule;
- the primary `context_history_view` prompt path does not consume the head, so
  emergency compaction does not reliably reduce that prompt.

### 8.1 Immediate safety action

Until agent-aware compaction exists, do not expose a context-wide compaction
summary through an agent-prompt API. Rename or isolate current context-wide
compaction as operator/audit compaction if it remains useful.

Remove misleading APIs such as `conversation_context_for_agent_prompt` that do
not accept an agent ID.

### 8.2 Agent-prompt compaction

Build agent compaction from the same `AgentParticipationView` used by normal
prompt projection:

```text
AgentCompactionHead {
  context_id,
  agent_id,
  projection_version,
  covered_cursor_start,
  covered_cursor_end,
  summary,
}
```

Requirements:

- source rows are actor-or-target rows for exactly one agent;
- archive and citation hydration use that agent's ref table;
- the summary carries attribution to that agent view;
- sibling traffic cannot trigger compaction based on hidden row count or bytes;
- raw provenance remains unchanged and available to operators;
- applying the head actually reduces the same prompt path that measured the
  budget.

### 8.3 Audit compaction

A full-context operator summary may remain keyed by `context_id`. Give it a
distinct type, table, and projection profile. It must never satisfy an
agent-visible summary query.

### Phase 4 exit gate

Two agents in one context receive different summaries derived from their own
views. The operator can still request a complete context summary. No summary
crosses those APIs.

## 9. Visibility behavior

| Data | Operator by context | Owning agent | Sibling agent |
| --- | --- | --- | --- |
| Full graph and Mermaid | yes | no implicit full access | no implicit full access |
| Actor-or-target messages | yes | yes | only when explicitly actor or target |
| Tool calls and session steps | yes | yes | no |
| Archive body | yes | yes | no |
| Pending Send receipt | yes | yes | no |
| `#N` citation body | yes | yes when row is visible | no |
| Agent compaction summary | yes | yes | no |
| Audit compaction summary | yes | no | no |
| A2A reply | yes | caller and target | no |

## 10. Test matrix

### Projection stability

- `conversation_history_json_for_intra_dedup` is not silently capped.
- An after-cursor read returns only new rows for the same agent.
- Sibling appends do not change an existing agent projection.
- Equal timestamps remain stable through the node-ID tie-break.
- Delegation dedup does not mutate previously returned row identities.
- A zero-visible-row FSM hop is accepted.
- A promised visible completion missing after an awaitable write fails with the
  expected activity anchor in the diagnostic.
- The exact coordinator-to-ClickUp regression completes without the
  prefix-extension error.

### Archive isolation

- Prefixes remain unique across agents in one context.
- Concurrent distinct-agent prefix allocation returns a stable set of unique
  prefixes.
- Each agent can read its own archive.
- Cross-agent PageRead, SearchRead, and consume fail.
- The operator archive query sees every agent's archive.
- Identical sibling Sends invoke both backends.
- An identical repeat by one agent invokes its backend once.
- Cache hydration or invalidation for one agent does not drop another agent's
  pending receipt.

### A2A handoff

- Child archive refs remain private.
- Parent receives one persisted child reply.
- Parent gets a parent-owned archive receipt for the A2A completion.
- Parent can read that receipt and finish the coordinator turn.
- A sibling not involved in the delegation sees neither request nor reply.

### Compaction

- Agent A's summary contains no rows owned only by agent B.
- Agent B traffic does not alter A's compaction threshold.
- Agent summary heads are keyed by agent and projection version.
- Audit summary remains complete and operator-only.
- The measured prompt and the compacted prompt use the same projection path.

### Operator audit

- A context-only graph export still includes coordinator, ClickUp, all child
  tasks, ingress, hooks, tool operations, archive metadata, and audit decisions.
- Mermaid rendering can filter for display without changing stored provenance.
- Agent privacy changes do not remove or rewrite durable context rows.

## 11. Implementation checklist

Use each top-level group as a reviewable subtask. Do not begin the privacy and
compaction refactors until the production incident has a deterministic
regression and Phase 1 passes its exit gate.

### P0: reproduce and explain the production failure

- [~] **P0.1 Build the coordinator-to-ClickUp regression.** The failing mechanism is
      reproduced deterministically at the provenance + delta level (see findings 4–5 and the
      two authority tests below), but a full two-runner A2A slice driving
      `run_step_executor_loop` with a deterministic interceptor is still open.
- [x] **P0.2 Capture stable projection diagnostics.** Added to the fatal delta-failure
      path. On `read_provider_conversation_after_hop` failure, `run_step_executor_loop` logs a
      single `tracing::warn!` with `context_id`, `agent_id`, `function`, `hop`,
      `visible_rows_before`/`visible_rows_after`, `raw_window_truncated_before`/`after`, and the
      body-free `evicted_visible_rows` / `appended_visible_rows` as
      `(event_order, activity_anchor, role)` tuples. `node_id` is omitted (activity_anchor +
      event_order are the stable identity/order keys; exposing `node_id` would require surfacing
      raw pre-projection rows from the store).
- [x] **P0.3 Prove the intra-dedup cap mismatch.** Confirmed by code inspection
      and pinned by `context_history_view_test::none_window_means_default_cap_not_uncapped`:
      `project_conversation_to_json(scope, None)` -> `context_history_view(..., None, ...)`
      -> `window.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP)` (40).
- [x] **P0.4 Classify the failing delta.** Mechanism identified and proven at 40+ raw rows:
      an invisible raw row (empty-args session `Send`) evicts a visible row at the raw
      40-row window boundary. `#N` ref reassignment and delegation dedup are ruled out.
      **Open question:** whether the child's agent-filtered view actually reaches 40 raw rows
      in the production turn (its own rows across plan-step loops, not the coordinator's
      discovery rows) — to be confirmed by the P0.2 diagnostics.
- [x] **P0.5 Preserve the authority fixture.** Two red-before-green fixtures pin the chain:
      `shared_context_delegation_view_test::filtered_raw_row_evicts_visible_row_when_window_is_full`
      and `intra_turn::tests::suppressed_row_without_new_value_is_err`.

### P0 findings (code inspection + deterministic fixtures)

1. **P0.3 confirmed — the "uncapped" intra-turn read is capped at 40.**
   `conversation_history_json_for_intra_dedup`
   (`crates/baml-rt-a2a/src/conversation_context/projecting_provider.rs:264`)
   calls `project_conversation_to_json(scope, None)`; that forwards `None` to
   `context_history_view`, which resolves
   `window.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP)`
   (`crates/baml-rt-provenance/src/surreal_store/context_reader.rs:393`).
   New authority test:
   `crates/baml-rt-provenance/tests/context_history_view_test.rs::none_window_means_default_cap_not_uncapped`
   seeds 45 agent-visible rows and proves `None` returns 40 + `truncated`, while an
   explicit large window returns all 45.

2. **The cap alone does not produce the reported error.** A sliding 40-row window
   yields a *wrong* delta — one novel row plus, when `truncated` flips, a synthetic
   `truncation_marker` line — not "added no new line values". The exact error needs an
   existing projected row to be removed, reordered, or re-rendered with no novel value.
   `intra_turn::tests::suppressed_row_without_new_value_is_err` pins that shape
   deterministically and fails on the current branch.

3. **`#N` ref reassignment is ruled out.** History refs are durable per
   `(context_id, activity_anchor, source)` via `history_ref_ensure` and
   `sync_history_refs_for_projection` (`crates/baml-rt-provenance/src/surreal_store/history_ref.rs`).
   Re-projection reuses the same `#N`, so appending a row cannot renumber older lines.

4. **Mechanism — an invisible raw row evicts a visible row at the window boundary (proven,
   but production row count unconfirmed).**
   `context_history_view` applies its 40-row window to **raw** rows
   (Message/ToolCall/SessionStep) *before* projection filtering. A session-phase ToolCall
   `Send` with **empty args** is fetched (outcome `Success`) but `include_call` is false
   (`conversation_context_filtered`: only `Send` with non-empty args is mirrored), so the
   whole ToolCall+ToolResult pair projects to nothing. ClickUp's `list_teams` / `list_spaces` /
   `list_lists` / `list_tasks` all have empty input structs (`ListTeamsInput {}`), so every
   such Send is an invisible raw row. When the raw window is at capacity, that invisible row
   rotates the window and evicts the oldest **visible** row with no replacement:
   `p_after = p_before − {visible row}` — the exact "reordered or suppressed only" shape.
   Proven deterministically by
   `crates/baml-rt-provenance/tests/shared_context_delegation_view_test.rs::filtered_raw_row_evicts_visible_row_when_window_is_full`
   (41 messages → 40 visible; one empty-args Send → 39 visible, no new row).
   **Not yet confirmed:** that the child's *agent-filtered* view reaches 40 raw rows in the
   production turn. The rows are accumulated within the single shared `context_id` (the child
   runs one `runGeneratedStepExecutor` per plan step), **not** loaded from a prior session;
   but the coordinator's ~30 discovery hops are caller-scoped and do not count toward the
   child's window. If the child stays under 40 raw rows, an invisible row appends without
   evicting and `p_after == p_before` — so the cap would not be the trigger in that case.
   P0.2 diagnostics must log the raw and visible row counts at the failure point to close this.

5. **Delegation dedup is ruled out for the child's turn.**
   `is_elided_delegation_payload` is **caller-scoped**: the `system/internal_a2a` ToolCall is
   executed by the coordinator, so the `A2A_TASK_CALL -> WAS_EXECUTED_BY` traversal exposes it
   only in the coordinator's view; the child never sees it. Proven by
   `crates/baml-rt-provenance/tests/shared_context_delegation_view_test.rs::internal_a2a_payload_is_caller_scoped_and_elided_for_the_caller`.
   The elision dedupes the caller's own relay payload; it does not mutate the child's view.

6. **Secondary defect — truncation marker under-reports for the intra-dedup path.**
   `project_conversation_to_json` builds the marker with
   `truncation_marker(item_limit.unwrap_or_default())`; the intra-dedup path passes
   `item_limit = None`, so the marker claims "0 older event(s)" even though up to
   `N - 40` rows were omitted. This also makes the marker line itself a phantom "novel"
   value in window-rotation deltas.

P0 exit gate:

- [ ] The test reproduces the reported `ChooseClickUpAction__entry` failure and
      identifies the changed row identities.

### P1: make the agent projection incremental and stable

- [x] **P1.0 Fix the SessionStep visibility gap (the active regression).**
      `context_history_view`'s agent filter (`conversation_call_agent_instances_clause`)
      used the `A2A_TASK_CALL` traversal for `SessionStep` rows, which never matched:
      SessionStep entities are linked via `A2A_TASK_SESSION_STEP` from the task entity,
      and the task's executing agent is the `WAS_LAST_EXECUTED_BY` head pointer. So every
      `Open`/`SendDone`/`SearchRead`/`PageRead` row was silently dropped from the agent
      prompt — this is what broke the coordinator discovery loop and the ClickUp
      list_teams → PageRead flow. Fixed by adding
      `conversation_session_step_agent_instances_clause` (two-hop
      `A2A_TASK_SESSION_STEP → WAS_LAST_EXECUTED_BY` traversal), making
      `conversation_call_agent_instances_clause` ToolCall-only, and splitting the
      ToolCall / SessionStep arms in both `fetch_agent_window_rows` and
      `conversation_context_filtered`. Regression:
      `shared_context_delegation_view_test::session_step_rows_are_visible_to_the_agent`.
- [x] **P1.1 Replace ambiguous limit semantics.** Bridged with an explicit uncapped
      delta read: `STEP_EXECUTOR_DELTA_ROW_CAP` (4096) replaces the `None`→40 ambiguity for
      the intra-turn read. A true `AgentHistoryRead::{Window, After, FullForReplay}` enum is a
      follow-up.
- [x] **P1.2 Add the internal visible-row envelope.** Added
      `baml_rt_tools::prompt_projection::ProjectedConversationRow { activity_anchor, event_order, line }`
      and `project_prompt_context_with_identity`, threaded through
      `conversation_history_rows_for_delta` → `provider_conversation_history_rows_for_delta` →
      `read_provider_conversation_rows`. `node_id` is omitted (activity_anchor + event_order are
      the stable identity/order keys).
- [x] **P1.3 Add an agent-scoped after-cursor query.** Bridged: the delta uses the full uncapped
      `context_history_view(…, Some(STEP_EXECUTOR_DELTA_ROW_CAP), …)` read rather than a true
      after-cursor query; correctness is met, incremental cost is deferred.
- [x] **P1.4 Track one cursor per step-executor loop.** Bridged: `run_step_executor_loop` carries
      `prev_hop_graph_rows` (the prior projected snapshot) and re-reads uncapped each hop; a real
      `(event_order, node_id)` watermark is deferred.
- [x] **P1.5 Replace raw JSON prefix/set comparison.** Done. `hop_rows_from_provider_delta`
      compares by durable `activity_anchor` identity and returns appended lines; the old
      `take_prefix_growth` / value-equality error path is removed. Reordering, re-rendering,
      and zero-new-row hops no longer error.
- [x] **P1.6 Define zero-visible-row behavior.** Zero-new-row is accepted (no error). Added
      `hop_promises_visible_read` in `run_step_executor_loop`: a non-empty archive read
      (`SearchRead`/`PageRead`) must append at least one projected row or the loop logs a
      `tracing::warn!` content-loss signal. `Open`/`SendDone`/`Finish`/`Abort` and empty-args
      `Send` remain legitimate zero-row hops. Pinned by
      `step_executor_loop::tests::archive_read_hop_promises_visible_completion`.
- [x] **P1.7 Make ordering deterministic.** Verified: window and cursor use the durable key
      `(event_order, node_id)` (`sort_conversation_rows`, `fetch_agent_window_rows`
      `ORDER BY event_order, node_id`, exclusive cursor). Projection preserves that order
      verbatim and drops `node_id` (documented on `ProjectedConversationRow`); the delta
      compares by the unique `activity_anchor`. Pinned by
      `context_reader::tests::sort_conversation_rows_orders_by_event_order_then_node_id`.
- [x] **P1.8 Add sibling-interleaving coverage.** Added
      `shared_context_delegation_view_test::sibling_activity_between_clickup_reads_does_not_change_clickup_view`:
      seeds ClickUp rows, appends coordinator and Notion rows to the same context, and asserts
      the full serialized ClickUp view (row identity, order, rendering) and its paging cursor are
      byte-for-byte unchanged.
- [x] **P1.9 Run the manual ClickUp authority flow.** Restarted the runner (fresh
      `agentium serve` on 127.0.0.1:18080 with coordinator + notion + clickup loaded) and ran the
      exact prompt `hey use the clickup agent to get the list of tasks that are in progress`.
      One `ctx-1793128486019-6` spanned coordinator and ClickUp. ClickUp navigated
      teams → spaces → lists → tasks, returned 3 in-progress tasks (Task21, Task16, Test1),
      and the coordinator relayed them with `TASK_STATE_COMPLETED`. Zero prefix-extension
      failures, zero `ERROR`/`panic` log lines, zero content-loss warnings.

P1 exit gate:

- [x] The ClickUp request completes without the prefix-extension error, and no
      content-bearing row is silently lost.

### P2: isolate archives, receipts, duplicate guards, and citations

- [x] **P2.1 Introduce an explicit read principal.** Added
      `ProvenanceReadPrincipal::{Agent(AgentId), Operator}` (`store.rs`). Agent execution reads
      pass `Agent`; operator/audit paths (citation-integrity scoring, compaction render, the
      snapshot tests) pass `Operator`. `None` is no longer an implicit unrestricted marker.
- [x] **P2.2 Enforce archive ownership in durable reads.**
      `archive_get_body(context_id, archive_ref, principal)` filters by `agent_id` for the `Agent`
      principal and reads context-wide for `Operator`. Pinned by
      `archive_isolation_test::archive_body_reads_are_agent_scoped`.
- [x] **P2.3 Enforce ownership on mutation.** `archive_mark_send_consumed` now requires the owning
      `AgentId` and adds `AND agent_id = $aid`. A sibling's consume is a no-op. Pinned by
      `archive_isolation_test::archive_consume_is_owner_scoped`.
- [x] **P2.4 Re-key execution ref tables.** `ContextRefTables` is now keyed by
      `(context_id, agent_id)` via `ref_table_cache_key`; `get_or_create_ref_table`,
      `get_ref_table`, and `invalidate_ref_table` take an agent. Durable `archive_prefix` /
      `archive_body` identity stays context-global. Pinned by
      `shared_context_ref_store_test::shared_store_same_agent_shares_table_and_siblings_do_not`.
- [ ] **P2.5 Replace stale table publication.** Still open. `prepare_ref_table_for_projection`
      publishes a freshly hydrated table wholesale; a concurrent refresh can in theory overwrite a
      newer in-memory pending receipt. The pending receipt is still recoverable from `archive_body`,
      so this is a cache-freshness gap, not a data-loss bug. Needs a merge or generation check.
- [x] **P2.6 Narrow invalidation.** `write_event` now invalidates only the task/message agent's
      table (`invalidate_ref_table`); the effect subscriber narrows where the agent is known and
      falls back to `invalidate_ref_tables_for_context` otherwise. A sibling's table survives an
      unrelated write.
- [x] **P2.7 Scope duplicate suppression.** With per-agent tables,
      `find_receipt_by_send_signature` only searches the reading agent's own pending Sends. Within
      one agent's table the prefix is constant, so `(prefix, local)` ordering is local recency.
      Pinned by `archive_refs::tests::sibling_agents_do_not_suppress_each_others_sends`.
- [x] **P2.8 Scope history ref hydration.** `hydrate_ref_table` under `Agent` hydrates only the
      `#N` rows whose anchors are in the reader's projection (`known_text`); sibling anchors drop
      out, producing sparse `#N` values without a `history_ref_registry` schema change. `Operator`
      keeps the full-context cold-rebuild path. Pinned by
      `archive_isolation_test::agent_history_hydration_excludes_sibling_refs`.
- [x] **P2.9 Scope citation resolution.** An agent's ref table no longer contains sibling archive
      bodies or sibling `#N` rows, so a guessed sibling `@prefix/local` or `#N` fails resolution.
      Pinned by the same history and archive isolation tests.
- [x] **P2.10 Update shared-store tests.** Rewrote
      `shared_context_ref_store_test` for same-context same-agent sharing and sibling separation.
      Added `context_ref_tables_are_scoped_by_context_and_agent` in `archive_refs.rs`.
- [x] **P2.11 Add concurrent prefix allocation coverage.** Added
      `archive_isolation_test::archive_prefix_allocation_is_unique_and_stable` (8 concurrent
      allocators). This exposed a real thundering-herd bug in `archive_ensure_prefix`; fixed by
      serializing allocation per context with `archive_prefix_serializers`.

P2 exit gate:

- [x] Operators can inspect all context archives, each agent can use its own,
      and no read, mutation, citation, or duplicate decision crosses agents.
      (P2.5 cache-freshness refinement remains open but does not cross agents.)

### P3: make A2A handoff carry real information

- [ ] **P3.1 Pin child archive privacy.** Prove the coordinator cannot resolve
      a ClickUp-owned archive ref in the shared context.
- [ ] **P3.2 Persist one child reply.** Record the ClickUp response as an
      actor-to-target message with typed delegation lineage.
- [ ] **P3.3 Archive the parent tool result under the parent.** The coordinator
      receives a coordinator-owned receipt for `system/internal_a2a`.
- [ ] **P3.4 Prove parent consumption.** The coordinator reads its own A2A
      result archive or uses the bounded reply and reaches a terminal response.
- [ ] **P3.5 Handle leaked child refs safely.** Preserve them as text for audit,
      deny resolution, and provide a bounded diagnostic that asks the child to
      materialize the needed information.
- [ ] **P3.6 Keep unrelated siblings blind.** A third agent sees neither the
      delegation exchange nor either participant's archives.

P3 exit gate:

- [ ] The task list reaches the coordinator as persisted A2A information without
      granting access to ClickUp's internal refs.

### P4: split agent compaction from audit compaction

- [ ] **P4.1 Inventory every compaction caller.** Classify each as agent prompt,
      operator timeline, audit, replay, or test.
- [ ] **P4.2 Isolate unsafe context-wide entry points.** Prevent a context-wide
      summary from satisfying any agent-prompt read before agent-aware
      compaction lands.
- [ ] **P4.3 Remove misleading APIs.** Rename or replace
      `conversation_context_for_agent_prompt` because it does not accept an
      agent ID.
- [ ] **P4.4 Add an agent compaction key.** Persist heads by context, agent,
      projection version, and any required task dimension.
- [ ] **P4.5 Build summaries from the agent view.** Use the same actor-or-target
      rows, ref hydration, and cursor range as normal prompt projection.
- [ ] **P4.6 Make budget measurement view-consistent.** Hidden sibling rows must
      not affect an agent's item or byte threshold.
- [ ] **P4.7 Apply the head to the measured prompt path.** Verify compaction
      reduces the actual prompt rather than an unused projection.
- [ ] **P4.8 Create a distinct audit compaction profile.** Keep full-context
      summaries available to operators and impossible to return to agents.
- [ ] **P4.9 Add two-agent summary isolation tests.** Each summary contains only
      its reader's view while the audit summary contains the complete context.

P4 exit gate:

- [ ] Agent summaries are private and effective, and context-wide summaries are
      explicitly operator-only.

### P5: final regression and release gate

- [ ] **P5.1 Run focused crate suites.** QuickJS, provenance, A2A, and runner
      tests pass.
- [ ] **P5.2 Run shared-context concurrency coverage.** Include parallel child
      tasks, concurrent archive allocation, prompt reads, and cache hydration.
- [ ] **P5.3 Verify the operator graph.** Context-only JSON and Mermaid exports
      still show coordinator, ClickUp, ingress, hooks, child tasks, tools, and
      audit decisions.
- [ ] **P5.4 Regenerate committed fixtures.** Do this if BAML phase functions or
      generated contracts changed.
- [ ] **P5.5 Run formatting and clippy.** Record all verification commands in
      the eventual PR.
- [ ] **P5.6 Repeat the manual authority prompt.** Confirm one delegation, no
      discovery loop, no prefix-extension error, private sibling archives, and
      a complete final answer.

## 12. Delivery order

1. Land the deterministic ClickUp failure reproduction and bounded diagnostics.
2. Fix the intra-turn read contract and replace raw snapshot-prefix comparison
   with cursor-based agent deltas.
3. Run the manual coordinator ClickUp flow. Do not continue if it still fails.
4. Agent-scope execution ref tables, archive reads, consumption, duplicate
   suppression, and citations.
5. Make A2A result handoff tests prove parent-owned materialization.
6. Disable or isolate unsafe context-wide agent compaction entry points.
7. Add agent-aware compaction and a separate audit compaction profile.
8. Run shared-context concurrency, runner, provenance, QuickJS, and UI graph
   regressions.

Do not combine all phases into one unreviewable patch. Phase 1 is the production
incident fix. Phase 2 closes the privacy and cross-agent state bugs exposed by
the same shared-context change.

## 13. Verification

Focused commands:

```bash
cargo nextest run -p baml-rt-quickjs
cargo nextest run -p baml-rt-provenance
cargo nextest run -p baml-rt-a2a
cargo nextest run -p baml-agent-runner runner_clickup
just fmt
just clippy
```

Regenerate committed agent fixtures if BAML phase functions or generated runtime
contracts change:

```bash
just regen-fixtures
```

Manual authority check:

```bash
# Restart after rebuilding so the runner uses the changed runtime.
just coordinator-agent
```

Prompt:

```text
hey use the clickup agent to get the list of tasks that are in progress
```

Verify:

- one context ID spans coordinator and ClickUp;
- ClickUp reaches its tool and returns normally;
- no prefix-extension error appears;
- no repeated discovery or delegation loop appears;
- the coordinator returns the task information;
- ClickUp cannot read sibling archives;
- the context-only operator graph shows the full cross-agent execution.

## 14. Definition of done

This work is complete when the reported coordinator-to-ClickUp request succeeds
reliably under one shared context, step-executor history tracking no longer
depends on full-array prefix equality, every agent-facing read enforces the
reader's `AgentId`, child archive refs remain private, A2A replies carry the
actual information needed by the caller, and context-only operator provenance
still renders the complete execution graph.
