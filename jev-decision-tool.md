<!--
SPDX-FileCopyrightText: 2026 Karakara Labs

SPDX-License-Identifier: Apache-2.0
-->

# Plan: opt-in `typesafe/system_one` tool (TypeSafe Jev)

Status: proposal. This document describes work to do; it does not describe a tool that exists today.

## Goal and boundary

Give agent authors a host-mediated way to ask TypeSafe's System One models (Jev today) small, typed questions about a supplied state. One call accepts a state and independently evaluated Choice, Score, and Noul questions, and returns their structured answers; **agent code decides what happens next**. An agent opts in by declaring `"typesafe/system_one"` in its manifest `tools` array. The platform never changes an existing BAML function or coordinator decision implicitly.

The tool is generic. Routing, guardrails on LLM output, extraction verification, rubric scoring, triage, and ranking are all author concerns expressed through state, questions, and criteria. Nothing domain-specific (Grafana, coordinator, support) lives in the host adapter.

Jev is not a BAML chat client, a planner, an A2A agent, or an MCP gateway. It cannot generate an investigation, a user-facing explanation, or arbitrary tool arguments. Agentium continues to own execution, permissions, task state, history, and provenance. Provider semantics used here come from [`jev_docs.md`](jev_docs.md), the live TypeSafe documentation, and the API gate evidence recorded below.

Jev answers are snap judgments over the state they are given. They do not replace operating criteria: Jev should not decide, without thresholds, units, or a baseline in the state, whether `power: 50 W` and `pressure: 6 Pa` mean equipment is safe. Missing timestamps, units, baselines, thresholds, or asset identity are checked deterministically in agent code or sent to an investigator.

## Naming and placement

- **Tool name:** `typesafe/system_one`. The endpoint is `POST /v1/systemone`; `jev-*` is the `model` parameter. Naming the tool after the endpoint survives future System One models, and avoids "decide", which contradicts the rule that agent code decides.
- **Crate:** a new standalone tool crate `crates/tools/typesafe` (package `baml-tools-typesafe`), with its own `BundleType` (`typesafe`). It is **not** part of `system/*` or `SystemBundle`. System tools exist because they need runtime internals injected at construction (agent lister, tool registry, A2A handler, provenance query). This tool needs none of those; it is a third-party HTTP provider like `support/notion` and `support/slack_notify`, and follows their pattern.
- **Cargo feature:** `typesafe`, wired the same way as `slack-notify` — several crates and files, not just the runner:
  - `crates/baml-agent-runner/Cargo.toml`: optional dependency + feature `typesafe = ["dep:baml-tools-typesafe", "baml-rt-api/typesafe"]`, and `use baml_tools_typesafe as _;` in `src/lib.rs` (the runner link list).
  - `crates/baml-rt-builder/Cargo.toml` (optional dependency + feature) and `use baml_tools_typesafe as _;` in `src/lib.rs`, `src/baml-agent-builder.rs`, and `src/bin/regen_fixtures.rs`. Without this, `agentium build` and `just regen-fixtures` do not see the tool.
  - `crates/baml-rt-api/Cargo.toml`: forward `typesafe = ["baml-rt-builder/typesafe"]` (exactly as `slack-notify` already forwards to the builder), so the API surfaces the tool.
  - `crates/baml-tool-links/Cargo.toml` and `src/lib.rs`: optional dependency, feature, re-export, and a `force_link_all_tools!` entry.
  - Root `Cargo.toml`: add `"crates/tools/typesafe"` to `workspace.members`.
  Add `typesafe` to `http-tools` in every crate that defines that aggregate feature. Builds without the feature do not contain the tool.

**Core changes:** `BamlTool::execute` is now the single, mandatory execution method with signature `async fn execute(&self, ctx: &ToolSessionContext, args: Self::Input) -> Result<Self::Output>`. `ToolWrapper::open_session` already holds the Open-time `ToolSessionContext` and calls `tool.execute(&ctx, args)`. Context-free tools ignore the context by naming the parameter `_ctx`. `ToolSessionContext` also carries a host-owned `ToolSecrets` accessor restricted to the secret names declared in that tool's metadata. `ToolRegistry` builds the scope from its shared `ToolSecretResolver`; tools never construct a concrete fnox resolver. There is no context-free compatibility method and no `Option<ToolSessionContext>`. No changes to `SystemBundle` or the session FSM are needed. Configuration is bundle-wide and resolved by bundle name; it is not scoped by `agent_id`, `task_id`, or `context_id`, and the tool does not identify or distinguish calling agents.

## Existing Agentium rules to preserve

| Invariant | Consequence for this tool |
| --- | --- |
| Host-mediated effects ([runtime thesis](docs/assertions/agentium-runtime-thesis.md)) | Rust makes the TypeSafe HTTP request through the tool session. Guest TypeScript never makes a provider call. |
| Manifest tool allowlist ([agent author guide](docs/assertions/how-to-write-agents.md)) | Only manifest-listed tools are registered (`register_manifest_tools`), and the allowlist is checked again at session open. Generated types alone do not grant access. |
| One tool session FSM | Use the existing Open, Send, Read, Finish/Abort machinery through `openToolSession`. No new JS-to-Rust shortcut. |
| Intent before tool hops ([intent and session guide](docs/assertions/intent-based-planning-and-session-prompting.md)) | Where an agent's workflow uses execution sessions, commit intent/plan before a Jev hop that informs routing or retrieval. Jev answers are evidence for the next step, not a substitute for a plan. |
| Graph-backed conversation and citable reads ([conversation spec](docs/assertions/baml-rt-conversation-spec.md)) | Preserve ordinary tool-call/result and session-step recording. Do not append hand-written pseudo-history or mint citation refs in the adapter. |
| Event intake is source-owned ([event delivery](docs/assertions/host-to-agent-event-delivery.md)) | An external event reaches a subscribed intake agent first. Adding Jev does not turn a coordinator into a universal raw-webhook sink. |
| MCP approval and manifest pinning ([MCP reference](docs/reference/agentium-mcp-support.md)) | A Jev Choice cannot discover, install, approve, or invoke an MCP method. Callers act only on previously approved, allowlisted tools with validated arguments. |
| Consequential action policy ([semiotic gate](docs/assertions/semiotic-gate.md)) | A Jev answer cannot bypass tool access tiers, grounding, authorization, or A2A delegation checks. |

## Host implementation shape

| Need | Mechanism |
| --- | --- |
| Metadata, codegen, config schema | `#[baml_tool(name = "typesafe/system_one", access = Read, config = { bundle = "typesafe", schema = SystemOneConfig, default = ..., validator = validate_system_one_config }, secrets = [...])]` with a plain `#[baml_tool] impl BamlTool`. The config API runs the typed parser and semantic validator before storing a canonical value. The question and answer DTOs stay on the JSON Schema codegen path because their hand-written schemas carry tagged-union discriminators; setting `baml_types` would make the builder skip that schema conversion. |
| Executor with access to resolved config and scope | `BamlTool::execute(&self, ctx, args)` receives `ToolSessionContext` (`config`, `config_version`, scoped secrets, `context_id`, `task_id`). Parsed System One config is cached process-wide by canonical value, so each runner parses a new stored version once rather than once per invocation. |
| Control over history and archive text | `ToolWrapper<T: BamlTool>` already turns `BamlTool::describe_invocation`, `action_identity`, and `describe_result` into the three `ToolHandler` hooks (`describe_invocation`, `describe_archive_action`, `describe_result_value`) and already parses both payload shapes (`{"step":{"op":…}}` and `{"op":…}`). The Jev tool overrides those three `BamlTool` methods; no custom `ToolHandler` wrapper or shared extractor is needed (see History and projection). |
| One-shot lifecycle | `ToolCapability::OneShot` + `SessionPolicy::Strict` (defaults). |
| Typed failures | A typed `SystemOneError` enum converted into `BamlRtError::ToolClassified(ClassifiedToolError)` inside `execute`. `classify_for_session` passes `ToolClassified` through unchanged. One `classify_execution_error` override is still required: malformed input that fails `ToolWrapper` deserialization before `execute` runs (see Failure contract). |
| API key | `TYPESAFE_API_KEY` declared with `secrets = [...]` and resolved per call through the runner-injected, metadata-scoped `ToolSecrets` accessor (see Credentials). |
| Call controls | A crate-owned, process-wide limiter (see Call controls). |
| Arbitrary JSON fields | `OpaqueJson` for state, instructions, and criteria entries. |
| Tagged unions | `#[serde(tag = "type")]` with explicit `TsType`/`JsonSchemaType`, following `CallbackToolInput` in `crates/tools/system/src/tools.rs`. |

The tool constructor must not read the API key or touch the network. It runs during agent registration; resolving the key there would make every agent that allowlists the tool fail to boot when the key is missing. The key is resolved per call inside `execute`, after local validation and call controls and before network I/O.

## Tool contract

One host tool, read access, no event sources, no mutable session state, one-shot Send returning one result. A provider call incurs cost but grants no write authority over other tools. Do not add the tool to any manifest by default.

Agent-facing semantics (TypeScript view; Rust DTOs mirror this):

```ts
type EntryType = JsonValue; // any JSON value: string | number | boolean | object | array | null

type SystemOneInput = {
  state: JsonValue; // string, object, or array; nested values may include numbers
  questions: Record<string,
    | { type: "choice"; instructions: EntryType; criteria: Record<string, EntryType> }
    | { type: "score"; instructions: EntryType; criteria: EntryType[] } // ordered low → high
    | { type: "noul"; instructions: EntryType; criteria?: { true: EntryType; false: EntryType } }
  >;
  model?: string; // omitted: configured default; supplied: must be in the operator allowlist, else invalid_argument
  timeout_ms?: number; // optional per-call deadline; must be ≤ the configured timeout_ms ceiling (higher is rejected, not capped)
};

type SystemOneOutput = {
  model: string; // resolved model version returned by TypeSafe, e.g. "jev-1.13.0"
  answers: Record<string, ChoiceAnswer | ScoreAnswer | NoulAnswer>;
  usage?: { input_tokens?: number; output_tokens?: number };
};

type ChoiceAnswer = {
  type: "choice";
  choice: string;
  probabilities: Record<string, number>;
  confidence: number;
};
type ScoreAnswer = {
  type: "score";
  score: number; // position in [0, levels - 1]; may fall between levels
  probabilities: Record<string, number>; // keyed by level index as string
  confidence: number;
};
type NoulAnswer = { type: "noul"; noul: number }; // probability of "yes"; no separate confidence
```

**Forward compatibility:** the question and answer shapes are additive. A future provider feature adds new `type` variants and new fields inside existing answers; it never renames or removes a Phase 1 field, so existing agent code keeps compiling and running. The question and answer enums are marked `#[non_exhaustive]`, so adding a variant later is not a breaking change for Rust code that matches on them. Instructions and criteria are `OpaqueJson` now and stay that way. Keeping `OpaqueJson` is a known cost, not a free lunch: a future LLM caller authoring questions would have to emit stringified JSON for instructions and criteria, since `OpaqueJson` holds one JSON value rather than a nested schema. That trade-off is accepted in Phase 1, and the generated BAML and TypeScript types do not change.

The output shape is designed for agent code, not for the transcript (see History and projection). The provider's Score `legend` is validated (keys only) and **not** surfaced: the author already holds the criteria array. Level `i` in `probabilities` corresponds to `criteria[i]` as submitted.

The request maps to TypeSafe `POST /v1/systemone` with `Authorization: Bearer ...`. The caller controls `state`, question IDs, instructions, criteria, and optionally the model from an operator allowlist. It does **not** control the endpoint, headers, or bearer token. The adapter sends all questions in one call. Questions are independent: one answer is never context for another. A dependent follow-up is a second call with the earlier answer or new evidence in its state.

Question IDs are not sent to the model. Author docs and the helper must say that `instructions` must be self-contained.

### Request validation (before network I/O)

- Nonempty state and question set; bounded nesting depth for state, instructions, and criteria. The criteria object or array counts as one of the 64 levels.
- Question count within the configured limit; question IDs nonempty, bounded in length, free of control characters, and without leading or trailing whitespace. The guest bridge parses Send payloads into `serde_json::Value` before tool deserialization, so duplicate raw JSON object keys have already collapsed and cannot be detected by this adapter. This applies to question IDs and Choice option keys; ordinary map semantics still guarantee one value per key after canonicalization.
- `model`, when supplied, is in `allowed_models`; otherwise `invalid_argument` (no silent fallback, so a typo is visible). The configured default applies only when `model` is omitted.
- `timeout_ms`, when supplied, is positive and at most the configured `timeout_ms` ceiling; otherwise `invalid_argument` (rejected, not silently capped — matching how a disallowed model is handled).
- **Request size:** after the checks above, the adapter serializes the outbound `POST /v1/systemone` body once and rejects it with `invalid_argument` if it exceeds `max_body_bytes`. This bounds state, instructions, and criteria together; there is no separate state-size limit. The same serialized bytes are sent on every attempt. The check runs before any call control, so an oversized request never takes a concurrency permit or a rate token.
- Choice: 1–255 options (provider maximum); option keys follow the same nonempty, length, control-character, and whitespace rules as question IDs. Option values may be `null` (the provider's extraction and taxonomy examples rely on this).
- Score: 2–10 levels (provider range).
- Noul: `criteria` absent, or an object with exactly `true` and `false`.
- `instructions` must be present and non-empty. This is deliberately stricter than the provider (which accepts `null`): every answer must be reviewable against the exact prompt it answered, and a `null` or empty `instructions` string cannot be traced to an author intent. Reject it as `invalid_argument` rather than sending it. If a use case later genuinely needs provider-`null` instructions, it is a separate proposal.
- Values are JSON only (no non-finite numbers).
- No content-based rejection of field names in `state` or instructions. Heuristics such as "reject keys named `token` or `password`" would break documented use cases (for example, phishing and credential-request detection, where the state is about credentials). Data minimization is the author's job, and egress is documented for operators (see Data egress and retention).

Limits are versioned in config and covered by boundary tests. Configured limits may be lower than provider maxima, never higher.

### Response validation

Validate what agent code branches on. Provider rounding is telemetry, not an outage. The tolerances below use the provider's recorded examples, the supplied Playground observation, and the documented confidence formulas from the API gate.

| Check | Hard failure (`typesafe_provider_contract`) | Drift metric only (`provider_invariant_drift`) |
| --- | --- | --- |
| Answer set | Missing or extra question IDs; `type` differs from the question | — |
| Choice selection | `choice` is not a submitted option, or `p(choice) < max(p) − ε_tie` (ε_tie = 0.01) | — |
| Ties | Never a failure. When several options share the top probability (within ε_tie), the provider's `choice` is accepted | — |
| Choice probability keys | Not exactly the submitted options | — |
| Score keys | `probabilities` (and legend, if present) keys not exactly `"0".."n-1"` | — |
| Score range | `score` outside `[0, n-1]` | — |
| Score weighted mean | — | `\|score − Σ i·pᵢ\| > 0.05`. Not a gate: with probabilities rounded to 2 decimals the error can reach 0.005·n(n−1)/2 (0.225 at 10 levels) |
| Distribution sum | `\|Σp − 1\| > 0.05` | `0.01 < \|Σp − 1\| ≤ 0.05` |
| Value range | Any probability, confidence, or `noul` non-finite or outside `[0, 1]` | — |
| Legend values | Never compared with submitted criteria (they may be structured or `null`) | — |

- Unknown **additional** fields (for example new `usage` keys) are ignored and not surfaced. Missing required fields or wrong types fail. Never reinterpret a changed shape silently.
- Never synthesize confidence for Noul. Choice/Score `confidence` describes the shape of the distribution, not correctness; agents combine it with the selected option's probability and their own fallback policy.

### Failure contract

The tool can run on two paths, and the disposition is chosen so both behave safely:

- **Imperative (`openToolSession`)** — Phase 1's author surface. No host loop retries `HostRetriable` failures here (that loop belongs to the generated step-executor path in `tool_session_plan.rs`), and a tool failure does not reject the JavaScript promise: `continue()` resolves to `{ status: "error", error: { code, disposition, message, retryable, retry_after_ms, … } }` (`tool_step_to_value`). The TypeScript helper never retries; it surfaces the failure to agent code.
- **Step executor (compatibility only)** — any agent that allowlists the tool can also reach the generated step-executor path. There, a `Fatal` disposition **ends the entire plan run** (`tool_session_plan.rs` returns the error); every other disposition returns an error result to the step and the run continues. The tool therefore never returns `Fatal` for a well-defined provider, config, or validation failure.

Consequences:

- **The adapter owns all retries**, for transient failures only, within `max_attempts` and the overall call deadline. `timeout_ms` is measured from the moment the executor starts, which is when the one-shot session's Read runs (`OneShotSession::send` only stores the input). The deadline covers validation, limiter wait, every attempt, and retry backoff; time between Send and Read is not counted. A 429 is retried only when `Retry-After` (or a computed backoff) still fits the remaining deadline; otherwise it fails immediately.
- **Nothing the adapter returns is `Fatal`.** Every failure is `LlmCorrectable` or `InformAndContinue`, so the step-executor path surfaces an error result instead of aborting the run, and the imperative path surfaces the same envelope as before. `Fatal` is reserved for defects the tool cannot classify.
- **`retryable` is always `false`.** Only `HostRetriable` failures are marked retryable, and this tool never uses `HostRetriable` (the adapter already retried). Even a provider 429 surfaces with `retryable: false`; the TypeScript helper and author docs must tell authors to branch on `code` and `retry_after_ms`, not on `retryable`.

| Failure | Code | Disposition |
| --- | --- | --- |
| Local validation failure | `invalid_argument` | `LlmCorrectable` (bad or out-of-range input is returned as an error result; harmless now and already correct for Phase 2 when an LLM may author questions) |
| Tool disabled or config invalid | `typesafe_disabled` | `InformAndContinue` |
| No usable API key | `typesafe_missing_credential` | `InformAndContinue` |
| Provider 401/403 (not retried) | `typesafe_unauthorized` | `InformAndContinue` |
| Local concurrency limit not acquired before deadline | `typesafe_capacity_exceeded` | `InformAndContinue` |
| Local request-rate limit | `typesafe_rate_limited_local` | `InformAndContinue` |
| Local token budget exhausted | `typesafe_quota_exceeded` | `InformAndContinue` |
| Provider 429 after retries / deadline | `typesafe_rate_limited` | `InformAndContinue`, with `retry_after_ms` when known |
| Timeout, transport failure, 5xx after retries | `typesafe_provider_unavailable` | `InformAndContinue` |
| Malformed or invariant-violating response | `typesafe_provider_contract` | `InformAndContinue` |
| Provider 4xx client error (400/413/415/422, not retried) | `typesafe_provider_contract` | `InformAndContinue` |

**Malformed input** (a payload that fails `ToolWrapper`'s `deserialize_tool_input` before `execute` runs) is classified correctly as `invalid_argument` / `LlmCorrectable`, but serde's raw message quotes the offending value (for example, `invalid type: string "…", expected a sequence`), which would leak criteria or instructions text into the transcript error line. The tool overrides `classify_execution_error` to rewrite such messages to the field path and expected type only, preserving `invalid_argument`. This is the one `classify_execution_error` override the tool needs.

`HostRetriable` is not used: the adapter already retries transient failures, and a host retry loop would stack with it. `InformAndContinue` for auth and not-found matches existing convention (Notion and ClickUp already use it). The code list is frozen contract: `typesafe_*` for tool-specific failures, `invalid_argument` as the shared core code, and new codes are additive only. No fallback to a generic LLM or another model without an explicit agent-side policy.

## Author experience (Phase 1: TypeScript-driven)

Phase 1 targets agents whose **code** decides when to ask Jev, with developer-authored, tested, and versioned questions and criteria. This matches TypeSafe's model ("code owns control flow") and keeps answers evaluable.

### Helper and distribution path

- **Canonical file:** `crates/tools/typesafe/ts/typesafe.ts`. It contains the hand-written `SystemOneInput`/`SystemOneOutput`/answer type declarations (the generated `baml-runtime.d.ts` does **not** declare tool output types — see below), a `SystemOneError` class carrying `code`, `disposition`, `message`, and `retryAfterMs`, and `systemOne(input): Promise<SystemOneOutput>`. It declares `openToolSession` locally as the existing fixtures do.
- **Supported path:** authors copy the file into their agent's `src/` and `import { systemOne } from "./typesafe";`. The builder inlines sibling `./` imports into the entry script (`crates/baml-rt-builder/src/builder/compiler/quickjs_post_process.rs`), so the helper becomes part of the content-addressed bundle. `../` imports are not inlined; the file must live inside `src/`.
- **Helper behaviour:** `openToolSession("typesafe/system_one")` → `send(input)` → `continue()`. On `status: "done"`, unwrap `output`, call `finish()`, and return it. On `status: "error"` (an adapter or executor failure), call `abort()` and throw `SystemOneError` with the envelope's fields. On any other status, abort and throw. Never retry.
- **Rejections before Read are not envelopes.** `openToolSession` and `send()` can reject the promise before the executor runs, for example when an interceptor blocks Send. If `openToolSession` rejects, there is no session; rethrow that error. If `send()` or `continue()` rejects after the session opened, `abort()` the session and rethrow the original error. Do not wrap it as `SystemOneError`. A failed `abort()` must not replace the original error.
- **Why hand-written:** the generated `baml-runtime.d.ts` declares neither `openToolSession` nor tool output types. `extra_ts_types` stores declarations in tool metadata, but nothing in the builder reads `extra_ts_decls`, so they never reach the `.d.ts`. Notion's `BlockRenderMode` appears in `agents/notion-agent/src/baml-runtime.d.ts` only because that agent uses a step executor — the type comes from the generated BAML session plan (`baml_src/_baml_runtime.baml`), not from `extra_ts_types`. `tests/fixtures/agents/slack-smoke-tool` uses `support/slack` imperatively and its `.d.ts` has no Slack types at all. So `typesafe.ts` hand-writes the input/output/answer types, and a schema-sync snapshot test keeps them in sync: render `ts_decl()` for each DTO type (they already implement `TsType`) and compare against the type block in `typesafe.ts`.
- **Later:** an `agentium` CLI command that copies the helper, or a builder-generated helper alongside `baml-runtime.d.ts`.

Optional helpers in the same file: build Choice/Score/Noul questions, check that a selected option is in a runtime allowlist, and apply an author-supplied threshold or three-way split (act / escalate / reject). No platform-wide threshold such as `confidence >= 0.8`; thresholds reflect the cost of a wrong action and are tested against labeled examples. Helpers never perform A2A calls, MCP calls, or writes.

**Not in Phase 1:** letting an LLM step executor author Jev questions (including the tool's generated session plan in a step-executor union). Runtime-invented criteria undermine calibration and evaluation. The instructions/criteria type is already settled as `OpaqueJson` (see Tool contract): enabling LLM-authored questions later is additive to the author surface and does not change the generated BAML or TypeScript types. The recorded cost is that an LLM caller would emit instructions and criteria as stringified JSON. The failure mapping stays as written; no `string | OpaqueJson` union is ever introduced.

### Example: routing (intake agent)

The criteria are agent-defined; the options are a snapshot of permitted candidates, not names accepted blindly from the provider.

```json
{
  "state": {
    "alert": { "asset": "pump-12", "power_w": 50, "pressure_pa": 6 },
    "candidates": {
      "grafana-investigator": "Investigates metrics and alert timelines",
      "incident-coordinator": "Coordinates an incident after its scope is known"
    }
  },
  "questions": {
    "next_agent": {
      "type": "choice",
      "instructions": "Which candidate in `candidates` should first investigate `alert`?",
      "criteria": {
        "grafana-investigator": "Telemetry diagnosis and missing metric context",
        "incident-coordinator": "Cross-team incident response",
        "none": "No candidate is clearly suitable"
      }
    },
    "has_comparison_window": {
      "type": "noul",
      "instructions": "Does `alert` provide a time window or baseline to compare its readings against?"
    }
  }
}
```

`has_comparison_window` cannot see `next_agent`'s answer. The agent can check for a structured window in code first and drop that question. Routing proceeds only when `next_agent` names an allowed candidate **and** the agent's tested policy accepts the result; `none`, low probability, and missing evidence take a fallback path. The tool never makes the A2A call.

```text
Check required alert fields and known thresholds in code.
Get candidate agent cards with system/discover_agents if routing is needed.
Build state from the alert plus concise candidate descriptions.
Ask a Choice among allowed candidate IDs plus "none"; optionally ask
independent Noul questions about evidence present in the alert.
If routing is uncertain or "none" wins, escalate to the LLM or operator.
Otherwise validate the ID against discovered candidates and call
system/internal_a2a under the normal execution-session plan.
A specialist may query an approved Grafana tool, then ask a new
question about the resulting evidence.
```

For a coordinator, a Jev answer can replace a *bounded routing judgment*, not `MakeStructuredPlan`, `DecideDelegationAction`, or final synthesis. Preserve `submitIntent`, `submitPlan`, step transitions, and `awaitInput`. `agents/coordinator-agent/src/index.ts` is a candidate for a separate opt-in experiment, not an implicit migration.

### Example: guardrail on an LLM draft (non-routing)

```json
{
  "state": {
    "user_request": "Summarize the outage for the customer.",
    "draft_reply": "…",
    "policy": "Do not disclose internal hostnames, credentials, or unreleased dates."
  },
  "questions": {
    "discloses_internal_detail": {
      "type": "noul",
      "instructions": "Does `draft_reply` disclose anything `policy` forbids?"
    },
    "answers_request": {
      "type": "score",
      "instructions": "How completely does `draft_reply` address `user_request`?",
      "criteria": [
        "Does not address the request",
        "Partially addresses it; key points missing",
        "Fully addresses the request"
      ]
    }
  }
}
```

The agent sends the draft only below its tested `discloses_internal_detail` threshold, and regenerates or escalates otherwise.

## Credentials and configuration

### Credential

- The key is declared as a secret on the tool: `#[baml_tool(secrets = [{ name = "TYPESAFE_API_KEY", description = "TypeSafe API key", reason = "Required to authenticate with the TypeSafe System One API" }], ...)]`.
- It is resolved on each call inside `execute`, after local validation and call controls and before network I/O, through `ctx.secrets`. The runner owns one shared resolver and restricts each tool to its declared secret names. A dedicated `resolve_credential` path handles tool credentials: the overlay first (an explicit unlink wins), then fnox, then the process environment only when `BAML_FNOX_CONFIG` is absent. The ordinary `resolve` path stays fnox-only, so LLM keys, MCP secrets, and the secrets-overview `satisfied` flag keep their existing no-env semantics. The TypeSafe crate has no dependency on fnox.
- Unset or empty fails with `typesafe_missing_credential`; no request is sent. A provider 401/403 fails with `typesafe_unauthorized` and is not retried.
- The key never appears in session input, config JSON, model context, logs, metrics, archives, or provenance payloads. Errors never echo request headers.

### Phase 1 config (non-secret, bundle `typesafe`)

Resolved by `ToolRegistry` at session open and delivered through `ctx.config` with `config_version` for provenance linkage. Proposed default:

```json
{
  "enabled": false,
  "default_model": "jev-1.13.0",
  "allowed_models": ["jev-1.13.0"],
  "timeout_ms": 10000,
  "max_attempts": 3,
  "limits": {
    "max_body_bytes": 262144,
    "max_questions": 64,
    "max_choice_options": 255,
    "max_score_levels": 10
  },
  "controls": {
    "max_concurrent_requests": 16,
    "requests_per_minute": 600,
    "max_tokens_per_hour": null
  }
}
```

- These are **Agentium limits**, not TypeSafe limits. The live Models page confirms `jev-1.13.0` as the stable version behind `jev-latest`; `jev-preview` points to `jev-1.14.0`. A pinned default keeps evaluations reproducible. Operators may add an alias deliberately.
- TypeSafe documents no separate question-count limit. Its limits are 64k tokens per request, 32k tokens per question, and 32 MiB for the HTTP body. Agentium keeps the lower 64-question and 256 KiB defaults to bound cost, latency, and retained provenance. Request limits must still leave room for speculative fan-out, many-option Choice, and multi-question scoring. There is no fixed per-task call cap; operational protection comes from Call controls.
- The 10-second overall deadline matches the SDK's default timeout for one HTTP operation. `max_attempts = 3` matches the SDK default of one initial attempt plus two retries, but the overall deadline may stop later attempts. Per-call deadlines may be lower.
- The operator surface for `enabled` and limits is `PUT /config/{bundle}`. The handler runs JSON Schema validation, deserializes `SystemOneConfig`, applies cross-field and provider-limit checks, and stores the canonical serialized form. Each runner caches the parsed config and reuses it while that value is current. A replica that did not handle the upload, or a runner recovering after restart, parses the stored value once on first use. Corrupt or legacy stored data still fails closed as `typesafe_disabled`, with a log line naming the bad field rather than its value. With `enabled: false` as the default, a store outage also fails closed.
- The typed validator is opt-in via `validator = ...` on the tool attribute. Bundles that do not declare one keep the previous behaviour of storing the JSON-schema-validated body as submitted. Two generic handler changes ride along: a bundle schema that fails to compile now returns 500 instead of being skipped silently, and a declared validator's canonical output is what gets stored.
- The config structs use `#[serde(default, deny_unknown_fields)]`. New fields take safe defaults when older stored configs are read, while misspelled or retired fields remain errors. `enabled` therefore still defaults to `false` after an upgrade.
- Per-request counts, retry attempts, concurrency, and request rate use `u32`; `timeout_ms` and the rolling token count use `u64`. Runtime config validation rejects zero where it has no useful meaning and enforces the provider maxima for Choice and Score.
- Endpoint: pinned to `https://api.typesafe.ai/v1/systemone`. A `TYPESAFE_API_BASE_URL` environment override exists for local mocks and tests only (same pattern as `SLACK_NOTIFY_API_BASE_URL`). It is operator-controlled, never set from Send input or stored config.
- Both the manifest opt-in and operator enablement are required.

**Per-call customization** is the agent's flexibility surface: state, questions, criteria, a model from `allowed_models`, and an optional per-call `timeout_ms` that must not exceed the configured `timeout_ms` ceiling (interactive guardrails and batch triage need different deadlines). All within operator limits.

### Call controls

Manifest opt-in authorizes an agent to use the provider; it does not protect the provider quota. A faulty agent looping on 64-question calls could otherwise exhaust it. The adapter enforces tool-level limits, shared by all callers, in this order after request validation (including the request-size check) and before any network I/O:

1. **Concurrency:** a process-wide semaphore (`max_concurrent_requests`). The call waits up to its deadline, then fails with `typesafe_capacity_exceeded`.
2. **Request rate:** a token bucket (`requests_per_minute`). The call waits up to its deadline — the same way the concurrency limiter does — then fails with `typesafe_rate_limited_local`. Waiting avoids spurious failures during fan-out bursts, which TypeSafe recommends; a burst above the configured rate is deferred, not rejected.
3. **Token guard (optional, best-effort):** when `max_tokens_per_hour` is set, a rolling counter fed by provider `usage` after each call completes. Once the counter passes the budget, new calls fail with `typesafe_quota_exceeded` until the window rolls over. This is **not a spend cap**: calls admitted concurrently all pass before their usage arrives, so the budget can be overshot by up to `max_concurrent_requests` calls; responses without `usage` are not counted; and the in-memory counter resets on restart. A strict cap would require reserving a worst-case token cost before each request, which Phase 1 does not do.
4. **Spend alerts:** usage and error-code metrics feed alert rules under `observability/`. Alerts have no runtime effect. Provider-side quota is the hard backstop.

Concurrency and request rate bound load on the provider quota; the token guard limits sustained runaway usage on a best-effort basis. None of these divide capacity fairly between agents: a busy agent can use the whole envelope, and others then receive the limit errors above. That is an accepted trade-off. Configuration stays at the tool level, and the tool does not identify callers.

Limits are per runner process. With N replicas the effective limits are N times the configured values; operators divide provider quota accordingly. A limiter that spans replicas is out of scope for Phase 1. A tool interceptor was considered and rejected: interceptors are wired by hand in `a2a_transport.rs` and fire on every session phase.

## Observability, history, and data handling

### History and projection

On the imperative path, `__tool_session_read` records provenance effects but does not create an archive entry; `describe_archive_action_for` and `describe_result_for` run only on the step-executor path (`tool_session_send_blocking`). Each surface is controlled separately:

| Surface | Path | Control |
| --- | --- | --- |
| Tool-call line in the transcript | Both | `describe_invocation`. The transcript passes `{"op":"Send","input":{...}}` (`prompt_projection.rs`; also accept `{"step":{...}}`, as the other session handlers do). Render question IDs, types, and the model when supplied, otherwise `model=default` — e.g. `typesafe/system_one: 3 questions (next_agent:choice, has_comparison_window:noul, …) model=jev-1.13.0`. Never empty: an empty description makes the episode projection fall back to pretty-printed args (`tool_call_fallback_json`), which include the state. |
| Tool-result line in the transcript | Both | The first 40 rendered lines of the raw output (`DEFAULT_TOOL_RESULT_INLINE_LINES` in `prompt_projection.rs`); no handler hook applies, and the tool does not control which answers appear. The adapter never copies the `state` field, instructions, or criteria values into the output. Answers do echo author-chosen option keys, which may legitimately be values taken from the state (for example, candidate IDs). |
| Error line in the transcript | Both | The error envelope. Error messages never include state, instructions, or criteria text. |
| `@N` archive header | Step executor only (compatibility) | `describe_archive_action`, which `ToolWrapper` implements via `BamlTool::action_identity`. `tool_session_send_blocking` passes the raw Send input, not the `{"op","input"}` wrapper. `action_identity` renders the same question IDs, types, and the model when supplied (otherwise `model=default`). The default projection would show the first 32 characters of a string state. |
| Archive summary fallback | Step executor only (compatibility) | `describe_result_value`, which `ToolWrapper` implements via `BamlTool::describe_result`, overridden to a one-line decision summary. This is not used for a normal Send: `tool_session_send_blocking` prefers `describe_archive_action` and only reaches the result fallback when neither an action identity nor Send arguments are available. Test the method directly. |

The step-executor rows are not a Phase 1 author capability. They exist because the tool is registered like any other tool once allowlisted, so the step-executor path must not leak state if an agent reaches it; their tests are compatibility coverage.

Payload-shape handling is not re-implemented: `ToolWrapper` already accepts both shapes (`{"step":{"op":"Send","input":{...}}}` and `{"op":"Send","input":{...}}`), deserialises the typed `Self::Input` once, and passes the typed value to the tool's `describe_invocation` / `action_identity` / `describe_result`. The Jev tool overrides those three methods; it never parses JSON payloads itself.

**Phase 1 makes no compact-transcript promise.** Phase 1 agents branch on the typed output in code; no LLM is required to read the result from history. The transcript shows whatever prefix fits in 40 lines. Two facts rule out controlling that prefix through output field order:

- `render_to_lines` (`crates/baml-rt-tools/src/archive_read/render.rs`) iterates a `serde_json::Map`, which sorts keys unless serde_json's `preserve_order` feature is on. The workspace does not enable it. Today every build containing `baml-rt-tools` gets it transitively from the BAML fork crates (`baml-types`, `baml-runtime`, …), and tool payloads round-trip through `payload_json` strings, so field order currently survives. That is an incidental property of a dependency, not a contract this plan may rely on.
- Even with a fixed order, a compact per-question view takes three to four rendered lines per question, so batches above roughly nine questions truncate inside it (`max_questions` is 64).

**Decision:** no compact transcript view in Phase 1. The output carries no transcript-only fields. History tests check two things: the call line and archive header name the question IDs, types, and the model when supplied (otherwise `model=default`), and the adapter does not copy input into the rendered surfaces. A bounded view (opt-in probabilities, a question cap tied to the window, or a core hook for rendered results) is a follow-up if an LLM later needs Jev results from history.

The contract spike proves this with rendered output, not metadata: an `insta` snapshot of `format_conversation_history_transcript` after an imperative call (following `crates/baml-rt-a2a/tests/conversation_history_snapshot.rs`), plus, as compatibility coverage, a snapshot of the archive header from a step-executor Send. A unit test calls the tool's `describe_invocation` (typed input) and `action_identity` (typed input) directly, and asserts the rendered description / identity contain each question ID, its type, and the model when supplied (otherwise `model=default`). A constant safe string fails that assertion. The tests also put a distinct sentinel string in `state` (and separate sentinels in instructions and criteria values) that is never used as a question ID or option key, and assert that no sentinel appears in the tool-call line, tool-result line, error line, or archive header. A malformed-input case is added: a sentinel embedded in a wrongly-typed field (for example, a Score `criteria` sent as a string) must not appear in the error line, because the `classify_execution_error` override rewrites serde's message to the field path and expected type. A failed Send creates no archive entry, so the error-line assertion alone covers the malformed-input case. A separate unit test calls `describe_result` and checks its one-line summary; a normal step-executor Send cannot exercise that fallback. They cannot assert that answer content never matches state text, since option keys may come from the state. Neither test asserts which answers fall inside the 40-line window.

Each imperative call costs four bridge round trips (open, send, continue, finish) with provenance writes. The evaluation measures this overhead against Jev's ~150 ms latency. If iterative loops such as hierarchical classification are dominated by open/finish overhead, `SessionPolicy::MultiSend` is a later option (it gives up entry-hop auto-finish).

### Data egress and retention

- State and question text leave Agentium for TypeSafe. Document this for operators. Ingestion agents select and minimize state, especially when it comes from untrusted webhooks.
- The normal Send recording **persists the full input, including state, in provenance**. That is subject to the standard retention and redaction policy; operators should know Jev inputs are retained like any other tool input. Keep state out of metric labels and log messages.
- Metrics: call count, duration, question count, usage tokens, error code, limiter rejections, and `provider_invariant_drift`, labelled by tool and model.

### Safety

Treat provider output as untrusted and validate it before branching. A Choice recommends only an ID; the host still enforces destination existence and existing A2A/tool policies. Unknown options, low confidence, provider outages, and exceeded limits all take explicit non-executing paths. Never call Jev inside an LLM or tool interceptor that silently changes existing function results.

## API reference gate findings

Reviewed on 2026-10-16 from the live [documentation index](https://docs.typesafe.ai/llms.txt), [API reference](https://docs.typesafe.ai/api.md), [Models page](https://docs.typesafe.ai/models.md), and SDK retry and exception references.

### Request limits

| Item | Provider contract | Phase 1 decision |
| --- | --- | --- |
| Body | 32 MiB maximum. The limit applies to compressed and decompressed bytes. | Keep `max_body_bytes = 262144` as an Agentium limit. |
| JSON | UTF-8 without BOM, finite numbers, at most 64 nested object or array levels in state, instructions, and criteria. | Validate before call controls and network I/O. |
| Questions | No separate count limit. Requests are instead bounded by 64k tokens per request and 32k tokens per question. | Keep `max_questions = 64` as an Agentium cost and retention limit. |
| Choice | At most 255 options. `null` option descriptions are documented. | Keep 255. |
| Score | Between 2 and 10 levels. | Keep 10. |
| Timeout | The endpoint reference publishes no server-side timeout. The SDK default is 10 seconds per HTTP operation. | Use a 10-second overall deadline, with lower per-call values allowed. |

The long-context header raises both token limits to 262k, but Phase 1 will not send it. The dedicated Score page describes strings, objects, and arrays, while the advanced structure reference and JavaScript SDK's shared `EntryType` explicitly include `null`. Phase 1 accepts `null` Score levels based on that more specific type contract. Box and image questions now appear in the documentation index but remain outside this Phase 1 proposal.

### Models

The pinned `jev-1.13.0` model is still accepted and is the stable version behind `jev-latest`. `jev-1.14.0` is available behind `jev-preview`. Since aliases move, the default and initial allowlist remain pinned to `jev-1.13.0`. `GET /v1/models` lists aliases rather than every accepted versioned ID.

### Errors and retries

The public reference lists `400`, `401`, `413`, `415`, `422`, `429`, and `529`. The SDK also models `403` as permission denied and retries `408`, `429`, connection failures, timeouts, and all `5xx` responses by default. TypeSafe says to retry `429` and `529` with exponential backoff. Organization rate limits are measured in requests per second and tokens per second.

`Retry-After` may be delta seconds or an HTTP date. TypeSafe SDKs also honor `retry-after-ms`, preferring it when both headers are present. The adapter will accept both forms, reject negative or invalid values, and use its own bounded backoff when the requested delay does not fit the remaining call deadline.

TypeSafe does not publish one formal error-body schema. SDKs retain the body as arbitrary JSON or text and classify by HTTP status. Live unauthenticated probes returned these two observed shapes:

```json
{
  "status": 403,
  "body": {
    "detail": {
      "error_type": "authentication_error",
      "message": "Must supply an API key! Check your request and try again."
    }
  }
}
```

```json
{
  "status": 401,
  "body": {
    "detail": {
      "error_type": "authentication_error",
      "message": "Cannot authenticate with the server. Please check your API key and try again."
    }
  }
}
```

The first response came from an omitted bearer token, even though the API table says a missing key returns `401`. The second came from an invalid bearer token. The adapter therefore treats both `401` and `403` as `typesafe_unauthorized`, uses status rather than provider message text for classification, and never puts the provider body in the transcript.

### Fixtures and calibration

The gate fixtures are under `crates/tools/typesafe/tests/fixtures/reference/`. They include the supplied Playground Score observation, TypeSafe's recorded structured-Score and null-Choice examples, and contract cases derived from the advanced structure and confidence references. Each fixture names its source and distinguishes a recorded answer from a contract-derived validation case.

The supplied Score observation reports probabilities `0.30`, `0.69`, and `0.01`, score `0.71`, and confidence `0.53`. The probabilities sum to 1, the weighted mean is exactly `0.71`, and the documented Score confidence formula gives `0.535`, which rounds to the displayed `0.53`. The other recorded examples also use two decimal places and distributions that sum to 1.

Calibration decisions:

- Keep `epsilon_tie = 0.01` so a one-unit difference at the documented display precision does not reject an otherwise tied Choice.
- Keep a drift threshold of `0.01` and a hard distribution-sum threshold of `0.05`. The provider contract says the sum is 1; the wider hard threshold prevents harmless display rounding from becoming an outage.
- Keep weighted-mean differences as drift telemetry rather than a hard failure. Score and probabilities may be rounded independently, especially on longer scales.
- Use `timeout_ms = 10000` and `max_attempts = 3`, aligned with the SDK's 10-second operation timeout and two-retry default. The single overall Agentium deadline still bounds limiter waits, attempts, and backoff.
- Keep `max_body_bytes = 262144`, `max_questions = 64`, and the pinned model as local policy rather than provider maxima.

The optional [`scripts/capture_typesafe_system_one_fixtures.sh`](scripts/capture_typesafe_system_one_fixtures.sh) can refresh these cases against an authenticated account, but it is not required to define the Phase 1 wire contract.

## Delivery sequence

1. **API reference gate.** Complete. The limits, models, errors, retry behavior, provider examples, supplied Playground observation, response tolerances, and config defaults are recorded above and in the reference fixtures.
2. **Contract spike (blocking).** Settle the two places where the runtime does not provide behaviour automatically:
   - **Imperative failures:** adapter retry and deadline behaviour, and the `status: "error"` envelope as seen from TypeScript.
   - **History projection:** the three `BamlTool` overrides (`describe_invocation`, `action_identity`, `describe_result`) carried by `ToolWrapper`, plus the transcript snapshot and the step-executor archive-header snapshot (compatibility coverage).

   Also prove JSON Schema and TS generation for all three question/answer variants, and runtime deserialization of raw JSON and `OpaqueJson` wrapper forms.
3. **Crate and adapter.** Create `crates/tools/typesafe` with DTOs, `#[baml_tool]` metadata (including `secrets`), the `typesafe` cargo feature wired through `baml-agent-runner`, `baml-rt-builder`, `baml-rt-api`, and `baml-tool-links` (and the root workspace member). Implement request/response validation, `SystemOneError` → `ToolClassified`, config parsing, and call controls. Tests run against a local mock server (axum, as in `slack-notify`). No network at build time.
4. **Author surface.** Canonical `typesafe.ts` helper with a schema-sync snapshot (render `ts_decl()` for each DTO type and compare against the type block in `typesafe.ts`). Two read-only fixture agents that import a copy: alert routing (handles `none`/uncertainty, delegates only to an allowed agent, asks a second question only after new evidence) and an LLM-draft guardrail. Run `just regen-fixtures`.
5. **Evaluation.** Compare against the existing LLM routing path on labeled incidents: correct route, abstention quality, false confident routes, latency including bridge/provenance overhead, provider tokens and cost, and end-to-end cost. Choose thresholds from that dataset, not from documentation examples.
6. **Follow-ups (separate proposals):** a bounded transcript view of results; builder emission of `extra_ts_decls` and `openToolSession` into `baml-runtime.d.ts` (with de-duplication — Notion's `BlockRenderMode` would otherwise be declared twice and break its TypeScript compile); an `agentium` command or builder step that installs the helper; LLM-authored questions; a limiter that spans replicas.

## Verification and acceptance

- **Build and opt-in:** with the `typesafe` feature, an agent listing `"typesafe/system_one"` registers the tool; an agent without it cannot open a session. Builds without the feature do not contain the tool. Run `just fmt`, `just clippy`, and targeted crate tests (`just test-crate baml-tools-typesafe`).
- **Contract matrix (`*_matrix` tables / `insta` snapshots):** valid Choice/Score/Noul and mixed batches; string, object, and array state; `null` option values; structured instructions and criteria; empty questions; serialized request at and above `max_body_bytes`, including a small state with large criteria; Choice >255 options; Score <2 or >10 levels; malformed Noul criteria; omitted model uses the default; supplied disallowed model fails with `invalid_argument`; `timeout_ms` zero, at the ceiling, and above the ceiling (rejected). Duplicate provider-response keys are rejected from raw response bytes; duplicate input keys are not observable after the guest bridge canonicalizes the Send payload.
- **Response matrix (from live fixtures where possible):** missing or extra answer IDs; type mismatch; choice not in options; tied top options accepted; choice below max − ε_tie rejected; rounded probabilities and weighted-mean drift accepted with a drift metric; sums outside tolerance rejected; score out of range; non-finite values; unknown additive fields ignored; structured and `null` legend values accepted and not surfaced.
- **Credential matrix:** unset or empty `TYPESAFE_API_KEY` fails with `typesafe_missing_credential` and sends no request; a set key is used for the request; 401/403 fails with `typesafe_unauthorized` without a retry; no bearer token in logs, history, provenance exports, or error messages.
- **Failure and HTTP matrix (local mock):** timeout measured from Read (a delay between Send and Read does not consume the deadline; limiter wait and retry backoff do), connection failure, 401/403, 429 with `Retry-After` inside and beyond the deadline, 5xx, invalid JSON, attempt budget, cancellation. Adapter and executor failures reach TypeScript as `status: "error"` with the expected `code`; the helper throws `SystemOneError` and does not retry. Rejected session operations are tested separately: a blocked `send()` rejects the promise, the helper aborts the opened session, and the original error is preserved; a rejected `openToolSession` leaves no session to abort. A live-provider smoke test runs only when `TYPESAFE_API_KEY` is set explicitly.
- **Call-control matrix:** concurrency saturation (`typesafe_capacity_exceeded` after the deadline); rate limit waits until the deadline then fails (`typesafe_rate_limited_local`); token guard tripping after recorded usage passes the budget, and window rollover (`typesafe_quota_exceeded`); responses without `usage` are not counted; an oversized request takes no permit or rate token; limits apply across all callers.
- **History matrix:** transcript snapshot after an imperative call and error-line snapshot; compatibility coverage: archive-header snapshot after a step-executor Send and a direct unit test of `BamlTool::describe_result` (the `describe_result_value` fallback). The call line and archive header contain the question IDs, types, and the model when supplied (otherwise `model=default`), from the tool's `describe_invocation` and `action_identity`. Distinct sentinels in state, instructions, and criteria values (never used as IDs or option keys) appear in none of the rendered lines or the directly tested summary; a malformed-input sentinel (a wrongly-typed field) appears in no error line.
- **Helper distribution:** the fixture agents' `src/typesafe.ts` is byte-identical to the canonical file; the schema-sync snapshot confirms the hand-written `SystemOneInput`/`SystemOneOutput`/answer types match the Rust DTOs' `ts_decl()` output; the packaged fixture runs the helper inside QuickJS.
- **One authority E2E:** dispatch a Grafana-style event to a subscribed intake fixture, commit intent and plan, run the tool through the host against the mock provider, inspect task-scoped conversation and provenance (no state sentinel in the call or result line, no key), and verify that only an allowed A2A target is called or the request safely abstains. Reuse `crates/test-support` and the testing handbook's vertical-slice and matrix patterns.
- **Product gate:** lower *measured* cost and latency on the pilot without an unacceptable increase in incorrect automatic routing; LLM/human fallback preserved; observed abstention rate published.

## Out of scope for Phase 1

Placement in `system/*` or `SystemBundle`; transparent replacement of BAML LLM functions; LLM-authored Jev questions and the step-executor path as an author capability (only its no-leak behaviour is tested); `HostRetriable` host retries; key management (the tool only resolves its declared secret); a strict token spend cap; per-agent or per-caller configuration and limits; limiters that span replicas; automatic routing of all host events to the coordinator; autonomous MCP discovery or invocation; provider-specific Grafana policy in the tool; platform-level claims that high Jev confidence makes an action safe. Each is a separate proposal with its own authority and security review.

When this plan is finalized, move it to `docs/plans/jev-decision-tool.md` and update the `docs/README.md` entry (path and tool name) in the same change.

## Implementation checklist

Derived from the plan above (no design changes). Phase 1 tasks only; the Delivery-step-6 follow-ups are separate proposals.

### 1. API reference gate
- [x] Fetch the live TypeSafe API reference: error body schema, 401/403/429 semantics and `Retry-After`, max questions per request, max request body size, timeouts.
- [x] Confirm the pinned model (`jev-1.13.0`) against the Models page; check aliases.
- [x] Capture provider evidence fixtures: tie behavior, rounded probabilities, structured and `null` Score levels, and Choice options with `null` descriptions.
- [x] Calibrate response tolerances and config defaults from the fixtures and SDK defaults.

### 2. Contract spike (blocking)
- [x] Prove the `status: "error"` envelope as seen from TypeScript on the imperative path.
- [x] Prove adapter retry + deadline behaviour (validation, limiter wait, attempts, backoff; Send→Read gap not counted).
- [x] Prove the three `BamlTool` overrides (`describe_invocation`, `action_identity`, `describe_result`) carried by `ToolWrapper`.
- [x] Prove the transcript snapshot and step-executor archive-header snapshot (compatibility coverage).
- [x] Prove JSON Schema + TS generation for all three question/answer variants.
- [x] Prove runtime deserialization of raw JSON and `OpaqueJson` wrapper forms.

### 3. Core change (`crates/baml-rt-tools`)
- [x] Replace `BamlTool::execute` with a single mandatory `execute(&self, ctx: &ToolSessionContext, args)` method (no `execute_with_context`).
- [x] Switch `ToolWrapper::open_session` to call `tool.execute(&ctx, args)` with the captured Open-time context.

### 4. Crate, DTOs, and metadata (`crates/tools/typesafe`)
- [x] Create the crate and a `typesafe` `BundleType`.
- [x] Define `SystemOneInput`/`SystemOneOutput` and the Choice/Score/Noul question + answer types.
- [x] Model question/answer variants as `#[serde(tag = "type")]` tagged unions with explicit `TsType`/`JsonSchemaType`.
- [x] Mark the question and answer enums `#[non_exhaustive]`.
- [x] Add `#[baml_tool(name = "typesafe/system_one", access = Read, config = {...}, secrets = [...])]` on a plain `impl BamlTool` (no `build_with`). Keep the tagged DTOs on schema-driven BAML generation so their discriminators are preserved.
- [ ] Implement `execute(&self, ctx, args)` (resolve config, validate, control, request, validate response).

### 5. Validation and failure mapping
- [x] Request validation: nonempty state/questions, nesting depth, question count/IDs, model allowlist, `timeout_ms` (positive, ≤ ceiling), request size, Choice 1–255, Score 2–10, Noul `criteria`, non-empty `instructions`, JSON-only values.
- [x] Response validation: answer set, choice selection, probability keys, score keys/range, distribution sum, value range, legend keys; ignore unknown additive fields.
- [x] Emit `provider_invariant_drift` metric for drift-only checks; never synthesize Noul confidence.
- [x] Map `SystemOneError` → `BamlRtError::ToolClassified(ClassifiedToolError)` with `typesafe_*` codes and the dispositions from the table.
- [x] Override `classify_execution_error` to rewrite serde deserialization messages to field path + expected type (preserve `invalid_argument`).

### 6. Credential and config
- [x] Declare `TYPESAFE_API_KEY` via `secrets = [...]`.
- [x] Resolve per call through the runner-injected, metadata-scoped `ToolSecrets`; no TypeSafe-to-fnox dependency.
- [x] `typesafe_missing_credential` on unset/empty; `typesafe_unauthorized` on 401/403 (no retry).
- [x] Pin endpoint `https://api.typesafe.ai/v1/systemone` with `TYPESAFE_API_BASE_URL` override for mocks/tests only.
- [x] Parse and semantically validate config during `PUT`, cache it process-wide on each runner, and fail closed as `typesafe_disabled` for malformed stored data.

### 7. Call controls
- [ ] Concurrency semaphore (`max_concurrent_requests`): wait to deadline, then `typesafe_capacity_exceeded`.
- [ ] Request-rate token bucket (`requests_per_minute`): wait to deadline, then `typesafe_rate_limited_local`.
- [ ] Token guard (`max_tokens_per_hour`, best-effort): `typesafe_quota_exceeded` until window rollover.
- [ ] Oversized request takes no permit or rate token.

### 8. History projection
- [x] `describe_invocation`: question IDs, types, model when supplied otherwise `model=default`; never empty.
- [x] `action_identity`: same rendering for the `@N` archive header.
- [x] `describe_result`: one-line decision summary.

### 9. Cargo feature wiring
- [ ] `crates/baml-agent-runner/Cargo.toml` optional dep + feature, and `use baml_tools_typesafe as _;` in `src/lib.rs`.
- [ ] `crates/baml-rt-builder/Cargo.toml` optional dep + feature, and `use baml_tools_typesafe as _;` in `src/lib.rs`, `src/baml-agent-builder.rs`, `src/bin/regen_fixtures.rs`.
- [ ] `crates/baml-rt-api/Cargo.toml`: `typesafe = ["baml-rt-builder/typesafe"]`.
- [ ] `crates/baml-tool-links/Cargo.toml` + `src/lib.rs`: optional dep, feature, re-export, `force_link_all_tools!` entry.
- [ ] Root `Cargo.toml`: add `"crates/tools/typesafe"` to `workspace.members`.
- [ ] Add `typesafe` to `http-tools` in every crate that defines that aggregate.

### 10. Author surface
- [ ] Write canonical `crates/tools/typesafe/ts/typesafe.ts` (hand-written types, `SystemOneError`, `systemOne()`, local `openToolSession` declaration).
- [ ] Helper: `open` → `send` → `continue`; `done` → `finish()` + return; `error` → `abort()` + throw `SystemOneError`; never retry.
- [ ] Rejection handling: rethrow `openToolSession` rejection; on `send`/`continue` rejection after open, `abort()` and rethrow original error.
- [ ] Schema-sync snapshot test: render `ts_decl()` per DTO type and compare against the `typesafe.ts` type block.
- [ ] Two read-only fixture agents importing a copy: alert routing and LLM-draft guardrail.
- [ ] Run `just regen-fixtures`.

### 11. Verification matrices
- [ ] Contract matrix (incl. `timeout_ms` zero / at ceiling / above ceiling).
- [ ] Response matrix (from live fixtures where possible).
- [ ] Credential matrix (unset / empty / set; 401/403; no token in logs, history, provenance).
- [ ] Failure + HTTP matrix against the local mock (incl. malformed-input message rewrite).
- [ ] Call-control matrix.
- [ ] History matrix (incl. malformed-input sentinel in the error line).
- [ ] Helper distribution (byte-identical, schema-sync, runs in QuickJS).

### 12. E2E and evaluation
- [ ] One authority E2E: Grafana-style event → intent/plan → tool against mock provider → conversation/provenance → allowed A2A target or abstain.
- [ ] Compare against existing LLM routing on labeled incidents; choose thresholds from the dataset.
- [ ] Measure latency including bridge/provenance overhead vs Jev's ~150 ms.
- [ ] Product gate: lower measured cost/latency, no unacceptable wrong-routing increase, fallback preserved, abstention rate published.

### 13. Finalize
- [ ] Run `just fmt`, `just clippy`, `just test-crate baml-tools-typesafe`.
- [ ] Move this plan to `docs/plans/jev-decision-tool.md` and update `docs/README.md` (path and tool name) in the same change.
