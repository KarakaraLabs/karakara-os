<!--
SPDX-FileCopyrightText: 2026 Karakara Labs

SPDX-License-Identifier: Apache-2.0
-->

# Plan: schema-driven tool TypeScript declarations

Status: implemented on `feat/tool-typescript-codegen` (Tasks 1–4).

The canonical schema graph has landed on `main`. This branch adds the graph's TypeScript renderer and uses it to generate the imperative host tool-session contract in `baml-runtime.d.ts`.

## Decision

Generate tool DTOs from `ToolSchemaGraph`, not from `ToolTypeSpec.ts_decl`, `extra_ts_decls`, BAML text, or a synthetic BAML function.

The build flow becomes:

```text
manifest allowlist
      |
      v
resolved ToolFunctionMetadata
      |
      v
ToolSchemaGraph, built once in WorkspaceReady
      |
      +--> BAML DTO renderer, already implemented
      |
      +--> TypeScript DTO renderer, this branch
                  |
                  v
        TypeScript declaration registry
          /          |           \
 runtime types   graph DTOs   BAML IR types
                  |
                  v
          baml-runtime.d.ts
```

Static, external, MCP, sandbox, and WASM tools use the same graph renderer. Tool origin and backend must not select a different TypeScript path.

## What the merged graph provides

The implementation on `main` already provides:

- `ToolSchemaGraph::from_tools(&[ToolFunctionMetadata])`;
- deterministic declaration IDs and symbols;
- `ToolSchemaContract` for each qualified tool name;
- `OpenInputContract::{Absent, Present}`;
- `OpenInputRequirement::{Optional, Required}`;
- object requiredness separate from nullability;
- objects, enums, aliases, arrays, maps, literals, unions, `VecOrOne`, and `JsonValue`;
- declaration dependencies, ownership, fingerprints, and source locations;
- deterministic collision handling;
- one graph instance carried to `RuntimeFinalized` as `Arc<ToolSchemaGraph>`.

`RuntimeFinalized::emit_typescript_declarations` does not use the graph yet. It still passes `tool_names` to `render_ts_declarations`, which discards them and calls `render_tool_typescript(&[])`.

## Goals

1. Render every graph-owned tool DTO into TypeScript with wire-accurate optionality, nullability, names, literals, and unions.
2. Generate tool-name and Open/Input/Output maps for the exact manifest allowlist.
3. Generate a runtime-accurate `openToolSession` declaration and session handle.
4. Make required OpenInput schemas required at compile time.
5. Prevent duplicate symbols when a tool DTO is also reachable through BAML IR.
6. Preserve typed DTOs for external and MCP tools. Do not reduce them to `unknown` when the graph is typed.
7. Keep explicitly opaque schemas as `JsonValue`.
8. Remove local `declare function openToolSession` shims from fixtures covered by generated declarations.
9. Leave System One migration to `feat/integrate_jev` after this generic branch merges.

## Non-goals

- Changing `ToolSchemaGraph` lowering or BAML rendering unless a TypeScript renderer exposes a genuine graph contract bug.
- Reading or parsing `ToolTypeSpec.ts_decl` or `extra_ts_decls`.
- Removing compatibility metadata fields from serialized catalogs.
- Adding a synthetic BAML `tool_call()` function.
- Adding per-tool globals such as `openTypesafeSystemOneSession`; the runtime does not install them.
- Adding a one-call `callTool` helper.
- Changing tool execution, session FSM behavior, concurrency, quota, secrets, HTTP, or provenance.
- Migrating the unmerged System One helper in this branch.

## Runtime contract that TypeScript must match

### Session handle

`register_tool_session_wrapper` installs one global function:

```ts
openToolSession(toolName, openInput)
```

The returned object has:

```ts
{
  sessionId,
  phase(),
  send(input),
  continue(readInput),
  finish(),
  abort(reason),
}
```

The observable phase strings are:

```ts
type ToolSessionPhase =
  | "Open"
  | "Send"
  | "Continue"
  | "Finish"
  | "Abort";
```

The declaration should model the actual handle, not a richer typestate API that the runtime does not return.

### Read input

`OneShotSession::read` and `MultiSendSession::read` merge the object passed to `continue()` over the stored Send object. Non-object read input is not merged.

There is no separate read-input schema in metadata. Use a conservative generic type:

```ts
export type ToolReadInput<I> =
  I extends readonly unknown[]
    ? Record<string, never>
    : I extends object
      ? Partial<I>
      : Record<string, never>;
```

Then:

```ts
continue(readInput?: ToolReadInput<I>): Promise<ToolStep<O>>;
```

If renderer tests show that distributive conditional behavior is misleading for `JsonValue` or object unions, introduce a graph-derived `ToolReadInputMap` instead. Do not use `I extends JsonObject`; TypeScript interfaces without an index signature do not reliably satisfy that constraint.

### Tool steps

`tool_step_to_value` projects four statuses:

```ts
export type ToolStep<O> =
  | { status: "streaming"; output: O }
  | { status: "suspended"; output: O }
  | { status: "done"; output: O | null }
  | { status: "error"; error: ToolFailure };
```

`done.output` is required. `serde_json::json!` always emits the key and serializes `None` as `null`.

### Failure envelope

The imperative error projection is:

```ts
export type ToolFailureKind =
  | "InvalidInput"
  | "ExecutionFailed"
  | "NotAuthorized"
  | "RateLimited"
  | "Cancelled"
  | "Unknown";

export type ErrorDisposition =
  | "host_retriable"
  | "llm_correctable"
  | "inform_and_continue"
  | "fatal";

export interface ToolFailure {
  kind: ToolFailureKind;
  message: string;
  retryable: boolean;
  disposition: ErrorDisposition;
  code: string;
  hint: string | null;
  retry_after_ms: number | null;
}
```

`ErrorDisposition` currently appears only inside the conditional step-executor block. Move it to shared runtime declarations and remove the conditional duplicate.

The existing A2A event declaration also references `ToolFailure`. Verify its runtime shape before making the extra fields required globally. If A2A failures do not use the classified tool envelope, introduce a separate `ToolSessionFailure` and use it in `ToolStep` rather than making the A2A declaration inaccurate.

## Graph to TypeScript model

Add:

```text
crates/baml-rt-builder/src/builder/tool_schema_graph/typescript.rs
```

The renderer must consume graph nodes only. It must not revisit raw JSON Schema.

### Proposed output types

```rust
#[derive(Debug, Clone)]
pub struct ToolTypescriptSurface {
    pub declarations: Vec<TsDeclaration>,
    pub tools: BTreeMap<String, ToolTypescriptContract>,
}

#[derive(Debug, Clone)]
pub struct ToolTypescriptContract {
    pub tool_name: String,
    pub open_input: Option<String>,
    pub open_requirement: Option<OpenInputRequirement>,
    pub input: String,
    pub output: String,
}

pub fn render_tool_typescript_surface(
    graph: &ToolSchemaGraph,
) -> Result<ToolTypescriptSurface>;
```

The contract strings are TypeScript expressions rendered from the graph root references. `open_input == None` means unit/absent OpenInput, not opaque JSON.

### Type mapping

| Graph type | TypeScript |
| --- | --- |
| `String` | `string` |
| `Integer`, `Number` | `number` |
| `Boolean` | `boolean` |
| `Null` | `null` |
| `JsonValue` | `JsonValue` |
| string/integer/number/boolean/null literal | exact TypeScript literal |
| `Array(T)` | `T[]`, parenthesizing union elements |
| `Map(T)` | `Record<string, T>` |
| `Named(id)` | graph declaration symbol |
| `Union(arms)` | stable `A | B` union |
| `VecOrOne(T)` | `T | T[]`, parenthesized where embedded |

Use JSON string escaping for string literals and quoted object keys.

### Objects

Render graph objects with requiredness and nullability kept separate:

```ts
export interface Example {
  required_value: string;
  optional_value?: string;
  required_nullable: string | null;
  optional_nullable?: string | null;
}
```

For `AdditionalProperties::AllowedAny`, add:

```ts
[key: string]: JsonValue | undefined;
```

All graph field values are JSON-compatible, so known fields remain assignable to the index signature.

For `AdditionalProperties::Forbidden`, emit no index signature.

The production BAML path rejects a fixed object mixed with typed additional properties. The TypeScript renderer should return the same source-located error if such a graph reaches it. Pure typed maps are already represented as `SchemaTypeRef::Map` aliases.

### Enums

Render graph enums as literal unions:

```ts
export type Status = "open" | "closed";
```

Do not emit TypeScript `enum`. Preserve numeric literals where the graph contains integer variants.

### Aliases and opaque JSON

Render aliases directly:

```ts
export type SearchResult = Foo | Bar;
export type Metadata = JsonValue;
```

An opaque schema is `JsonValue`, not `Record<string, unknown>`. `JsonValue` includes primitives and arrays, which `Record<string, unknown>` would reject.

### Ordering

TypeScript declarations do not require dependency order, but output must be byte-stable. Sort declarations by emitted symbol. Sort fields and enum variants according to graph order, which is already deterministic and preserves the canonical wire projection.

## Structured declaration registry

The final declaration file has legitimate inputs from:

- static runtime and A2A declarations;
- tool schema graph declarations;
- BAML IR declarations;
- step-executor support declarations.

Only one component should decide symbol ownership and conflicts.

Add a builder-owned module such as:

```text
crates/baml-rt-builder/src/builder/ts_declarations.rs
```

Proposed model:

```rust
#[derive(Debug, Clone)]
pub struct TsDeclaration {
    pub symbol: String,
    pub body: String,
    pub dependencies: BTreeSet<String>,
    pub origin: TsDeclarationOrigin,
}

#[derive(Debug, Clone)]
pub enum TsDeclarationOrigin {
    Runtime,
    ToolSchema {
        owners: BTreeSet<SchemaOwner>,
        source: SchemaLocation,
    },
    BamlIr,
    StepExecutor,
}

#[derive(Debug, Default)]
pub struct TsDeclarationRegistry {
    declarations: BTreeMap<String, TsDeclaration>,
    reserved: BTreeMap<String, TsDeclarationOrigin>,
}
```

### Registry rules

1. Runtime declarations reserve their exported symbols.
2. Graph declarations own every symbol in `ToolSchemaGraph.symbols`.
3. BAML IR declarations own BAML-authored symbols not owned by the graph.
4. A graph symbol matching the IR copy generated from the same tool DTO is emitted from the graph only.
5. Identical declarations from the same ownership class may deduplicate.
6. Two graph declarations cannot conflict because graph lowering already resolves or rejects them.
7. Any other conflicting body is a build error naming both origins.
8. The registry renders symbols in stable lexical order within the relevant file section.

The registry should not parse generated TypeScript text to discover symbols.

### Static runtime declarations

`baml-rt-tools/src/ts_gen.rs` currently returns one large raw TypeScript block. Keep that block if restructuring it is outside scope, but make its symbol inventory explicit in the owning crate:

```rust
pub fn runtime_typescript_symbols() -> &'static [&'static str];
pub fn render_runtime_typescript() -> Result<String>;
```

The builder registers those names as reserved before accepting graph and IR declarations. Do not duplicate the runtime symbol list in two crates.

The current `render_tool_typescript(tools)` parameter is unused. Rename the main API if practical, or keep a compatibility wrapper and add the accurate parameterless function.

### BAML IR declarations

Refactor `emit_type_declarations_tokens` so declaration collection and text rendering are separate:

```rust
pub fn collect_ir_type_declarations(
    ir: &IRSignature,
    needed: &HashSet<String>,
) -> Result<Vec<TsDeclaration>>;
```

Keep the existing transitive dependency closure. Submit every resulting declaration to the registry.

Do not try to prune the closure at a graph root. The closure is computed before rendering, and unrelated dependencies may still be required. Resolve ownership independently for each collected symbol.

Replace `BOOTSTRAP_TS_RUNTIME_TYPES` with registry-owned runtime reservations, or retain it only as a temporary compatibility assertion that agrees with the runtime symbol inventory.

## Tool maps and global API

Render the tool catalog from `ToolTypescriptSurface.tools`, sorted by qualified name:

```ts
export type ToolName =
  | "support/slack"
  | "typesafe/system_one";

export interface ToolInputMap {
  "support/slack": SupportSlackInput;
  "typesafe/system_one": SystemOneInput;
}

export interface ToolOutputMap {
  "support/slack": SupportSlackOutput;
  "typesafe/system_one": SystemOneOutput;
}

export interface ToolOpenInputMap {
  "support/slack": Record<string, never>;
  "typesafe/system_one": Record<string, never>;
}

export type ToolInput<T extends ToolName> = ToolInputMap[T];
export type ToolOutput<T extends ToolName> = ToolOutputMap[T];
export type ToolOpenInput<T extends ToolName> = ToolOpenInputMap[T];
```

For absent OpenInput, use `Record<string, never>` in the map. For present opaque OpenInput, use `JsonValue`. These are different graph contracts even if both can accept `{}` at runtime.

### Required OpenInput

Generate requiredness from the graph's existing `OpenInputRequirement`:

```ts
export type ToolWithRequiredOpenInput =
  | "some/session_tool";

export type ToolOpenArgs<T extends ToolName> =
  T extends ToolWithRequiredOpenInput
    ? [openInput: ToolOpenInput<T>]
    : [openInput?: ToolOpenInput<T>];
```

When no tool requires OpenInput:

```ts
export type ToolWithRequiredOpenInput = never;
```

Declare the real global:

```ts
declare global {
  function openToolSession<T extends ToolName>(
    toolName: T,
    ...args: ToolOpenArgs<T>
  ): Promise<ToolSession<ToolInput<T>, ToolOutput<T>>>;
}
```

Do not emit `open<Class>Session` declarations.

### Agents with no tools

Do not emit `ToolName`, maps, `ToolSession`, or `openToolSession` when the graph contains no tools. The runtime/A2A block may still change because shared `ToolFailure` and `ErrorDisposition` are corrected. Snapshot that intentional shared change rather than keeping an inaccurate zero-tool special case.

## Builder plumbing

Change the declaration entry point to use the objects already held by `RuntimeFinalized`:

```rust
pub fn render_ts_declarations(
    ir_signature: &IRSignature,
    tool_metadata: &[ToolFunctionMetadata],
    tool_schema_graph: &ToolSchemaGraph,
    session_plan_functions: &SessionPlanFunctionsMap,
    unified_step_executors: &UnifiedStepExecutorFunctionsMap,
) -> Result<String>
```

`tool_metadata` is available for diagnostics and consistency checks. DTO roots and OpenInput classification come from the graph.

Update `RuntimeFinalized::emit_typescript_declarations` to pass `&self.tool_metadata` and `&self.tool_schema_graph`. Remove the `dead_code` expectation from the graph field.

No catalog, inventory, manifest, or JSON Schema lookup is allowed in TypeScript rendering.

## File-level changes

| File or module | Proposed change |
| --- | --- |
| `builder/tool_schema_graph/typescript.rs` | Render graph declarations and per-tool TypeScript contracts. |
| `builder/ts_declarations.rs` | Add origin-aware declaration registry and deterministic rendering. |
| `builder/ir_to_ts.rs` | Return structured IR declarations before rendering; preserve transitive closure. |
| `builder/ts_gen.rs` | Assemble runtime, graph, IR, tool maps/session API, BAML globals, and step-executor declarations. |
| `builder/compiler/codegen_pipeline.rs` | Pass resolved metadata and the retained graph to TypeScript generation. |
| `baml-rt-tools/src/ts_gen.rs` | Expose runtime symbol inventory; correct shared failure/disposition declarations; remove the unused tool input from the main renderer. |
| `baml-rt-builder/tests/` | Add graph-to-TS snapshots, ownership/conflict matrices, and mixed-origin build coverage. |
| fixture `src/index.ts` files | Remove manual `openToolSession` declarations where generated declarations now own the API. |
| generated `baml-runtime.d.ts` files | Regenerate and review all affected fixtures and product agents. |

## Implementation checklist

Complete these four tasks in order. Each task should end in a reviewable commit with its focused tests passing.

### Task 1: render `ToolSchemaGraph` into structured TypeScript declarations

- [x] Add `tool_schema_graph/typescript.rs` and the `ToolTypescriptSurface` / `ToolTypescriptContract` API.
- [x] Implement graph type-expression rendering for primitives, literals, arrays, maps, named references, unions, `VecOrOne`, and `JsonValue`.
- [x] Render objects with quoted wire keys, optional properties, nullable values, and allowed/forbidden additional-property behavior.
- [x] Render enums as literal unions and aliases as `export type` declarations.
- [x] Derive per-tool Open/Input/Output expressions directly from `ToolSchemaContract`.
- [x] Preserve `OpenInputContract::Absent` separately from present optional OpenInput.
- [x] Sort declarations and tool contracts deterministically.
- [x] Add one graph-to-TypeScript boundary snapshot using the representative schema graph corpus already present in builder tests.
- [x] Add a table-driven expression matrix for required versus optional, nullable versus non-null, literals, union arrays, maps, and `VecOrOne`.
- [x] Add static versus external parity evidence showing equivalent graph schemas render equivalent TypeScript after root-name normalization.

Task 1 is complete when graph TypeScript output is deterministic, origin-blind, and does not inspect metadata `ts_decl` strings or raw JSON Schema.

### Task 2: centralize declaration ownership and merge graph DTOs with BAML IR

- [x] Add `TsDeclaration`, `TsDeclarationOrigin`, and `TsDeclarationRegistry` in the builder.
- [x] Make `baml-rt-tools::ts_gen` expose the symbols owned by its static runtime block. Keep the symbol inventory next to the renderer.
- [x] Register runtime symbols before graph and IR declarations. Reject a graph root that collides with a runtime-owned TypeScript symbol.
- [x] Refactor `ir_to_ts.rs` to collect structured declarations after its existing transitive dependency closure.
- [x] Submit graph declarations first, then IR declarations. Suppress the IR copy of graph-owned symbols by ownership rather than by parsing or a root-only skip set.
- [x] Keep IR-only dependent declarations that are still reachable and not graph-owned.
- [x] Replace or assert the old `BOOTSTRAP_TS_RUNTIME_TYPES` skip list against the runtime registry.
- [x] Add a Notion regression asserting `BlockRenderMode` and every overlapping graph DTO symbol appears once.
- [x] Add conflict matrix rows for runtime-versus-graph, graph-versus-authored-IR, identical duplicate, and unrelated transitive dependency cases.
- [x] Produce clear diagnostics containing symbol, both origins, tool owner, and schema source when applicable.

Task 2 is complete when one registry owns final symbol selection and no component parses already-rendered TypeScript to coordinate deduplication.

### Task 3: generate the typed tool catalog and runtime-accurate session API

- [x] Change `render_ts_declarations` and `RuntimeFinalized::emit_typescript_declarations` to receive resolved metadata and the retained graph.
- [x] Remove the discarded `tool_names` path and the `render_tool_typescript(&[])` call.
- [x] Emit `ToolName`, `ToolInputMap`, `ToolOutputMap`, `ToolOpenInputMap`, and their generic lookup aliases for non-empty graphs.
- [x] Emit `ToolWithRequiredOpenInput` and `ToolOpenArgs<T>` from graph OpenInput classifications.
- [x] Emit `ToolSessionPhase`, `ToolReadInput<I>`, `ToolStep<O>`, and `ToolSession<I, O>` with the exact runtime members.
- [x] Declare generic global `openToolSession` with conditional OpenInput arguments.
- [x] Expand the imperative failure type to the seven runtime fields. Resolve whether it can remain shared `ToolFailure` with A2A or needs `ToolSessionFailure` based on runtime evidence.
- [x] Move `ErrorDisposition` to one shared declaration and remove the conditional step-executor duplicate.
- [x] Ensure `done.output` is required and nullable, and include `suspended`.
- [x] Omit phantom per-tool opener declarations.
- [x] Add reviewed contract snapshots against `register_tool_session_wrapper` and `tool_step_to_value`.
- [x] Add OpenInput matrix rows for absent, present optional, and present required tools.
- [x] Verify a typed external tool and typed MCP tool keep graph DTOs, while an opaque MCP schema maps to `JsonValue`.

Task 3 is complete when generated declarations accurately describe the global runtime handle for every allowlisted tool origin.

### Task 4: remove fixture shims, run the vertical slice, and finalize documentation

- [x] Remove local `declare function openToolSession` blocks from fixtures now covered by generated declarations, including `session-tool-eval`, `coordinator-smoke`, `slack-smoke-tool`, and `dispatch-echo` where applicable.
- [x] Add or extend one mixed-origin authority fixture with a static tool, external tool, typed MCP-style schema, opaque schema, and BAML function that references a graph-owned DTO.
- [x] Run `tsc` for that fixture and assert all graph symbols appear once.
- [x] Regenerate affected fixture and product-agent `baml-runtime.d.ts` files. Review shared zero-tool changes caused by corrected runtime declarations.
- [x] Confirm agents with no allowlisted host tools do not receive an unusable `openToolSession` catalog API.
- [x] Confirm the generated file contains no declaration sourced from `ToolTypeSpec.ts_decl` or `extra_ts_decls`.
- [x] Update `crates/baml-rt-builder/README.md`, `docs/assertions/how-to-write-agents.md`, and relevant host-tool reference text to describe schema-driven declarations and required OpenInput.
- [x] Add the authoritative TypeScript codegen coverage to the testing handbook's authority map if it introduces a new vertical behavior row.
- [x] Move this plan to `docs/plans/tool-typescript-codegen.md` and add it to `docs/README.md` when implementation is complete.
- [x] Run formatting, clippy, targeted builder/tool tests, fixture regeneration, and TypeScript compilation.

Task 4 is complete when hand-written session declarations are gone from covered fixtures, mixed-origin agents compile, generated output is deterministic, and the branch remains independent of System One.

## Verification strategy

Follow `docs/assertions/testing-handbook.md`. Prefer one vertical build plus boundary snapshots and matrices.

### Authority vertical slice

Build and type-check one agent containing:

- a static tool;
- an external tool;
- a typed MCP-style tool;
- an opaque MCP-style tool;
- a BAML function whose signature reaches a graph-owned DTO.

Assert:

- all allowlisted tool names appear in the maps;
- static and imported typed tools have concrete DTOs;
- opaque roots use `JsonValue`;
- required OpenInput changes the call signature;
- graph-owned symbols appear once;
- the generated global can be called without a local shim.

### Boundary snapshots

Keep snapshots for:

- graph-to-TypeScript declarations;
- the mixed-origin complete `baml-runtime.d.ts`;
- tool maps and OpenInput requiredness;
- `ToolStep`, failure envelope, and session handle.

### Contract matrices

Use table-driven `*_matrix` tests for:

- graph expression rendering;
- object requiredness and nullability;
- declaration ownership and conflicts;
- OpenInput argument selection;
- legal versus quoted TypeScript property names.

### Runtime evidence

Pin declarations against:

- `register_tool_session_wrapper` for handle members and phase strings;
- `tool_step_to_value` for statuses, output presence, and failure fields;
- the existing imperative error envelope snapshot;
- `OneShotSession::read` and `MultiSendSession::read` for refinement behavior.

### Commands

Run focused tests while implementing, then:

```bash
just fmt
just clippy
just test-crate baml-rt-builder
just regen-fixtures
```

Also run the TypeScript compiler for all modified fixture agents and the targeted QuickJS tests when runtime declaration assumptions are changed or clarified. Use `cargo insta review` for intentional snapshot updates.

## Acceptance criteria

- Tool DTO declarations come only from `ToolSchemaGraph`.
- Static, external, MCP, sandbox, and WASM metadata use the same TypeScript renderer.
- Typed imported tools do not degrade to `unknown`.
- Opaque schemas use `JsonValue`.
- One registry owns runtime, graph, and IR symbol conflicts.
- Graph-owned DTOs are not duplicated through BAML IR.
- `ToolName` and all maps include only the resolved manifest allowlist.
- Required OpenInput is required by TypeScript; absent and optional OpenInput remain optional.
- `ToolSession` includes `sessionId`, `phase`, `send`, `continue`, `finish`, and `abort`.
- `ToolStep` includes `streaming`, `suspended`, `done`, and `error`.
- `done.output` is `O | null` and is not optional.
- The imperative failure declaration matches the runtime projection.
- No `open<Class>Session` declarations are generated.
- Covered fixtures no longer carry local `openToolSession` declarations.
- A mixed-origin fixture passes `tsc` and emits every owned symbol once.
- No TypeScript generator consumes `ToolTypeSpec.ts_decl` or `extra_ts_decls`.
- This branch contains no System One-specific code or helper migration.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Runtime raw declarations collide with graph symbols | Runtime renderer exports an explicit symbol inventory; registry rejects conflicts before rendering. |
| IR transitive closure leaks duplicate tool dependencies | Collect all IR declarations, then resolve each symbol through registry ownership. |
| Optional and nullable fields collapse | Graph renderer uses `SchemaField.required` separately from `SchemaTypeRef::Null`. |
| Read-input conditional type rejects interfaces or accepts arrays | Use `object` with an array guard; switch to a graph-derived read-input map if matrix evidence shows ambiguity. |
| Full `ToolFailure` breaks A2A typing | Verify A2A runtime shape; split `ToolSessionFailure` if the contracts differ. |
| Shared runtime corrections churn zero-tool snapshots | Treat accurate shared declarations as intentional and review regenerated snapshots. |
| Imported tools regress to fallback types | Mixed-origin vertical fixture includes typed external/MCP and opaque MCP cases. |
| Branch starts depending on System One | Keep System One migration out of this branch; use existing tools and synthetic metadata fixtures. |

## Handoff notes for the implementing agent

Read these files completely before editing:

- `crates/baml-rt-builder/src/builder/tool_schema_graph/mod.rs`
- `crates/baml-rt-builder/src/builder/tool_schema_graph/baml.rs`
- `crates/baml-rt-builder/src/builder/tool_schema_graph/lower.rs`
- `crates/baml-rt-builder/src/builder/tool_schema_graph/names.rs`
- `crates/baml-rt-builder/src/builder/ir_to_ts.rs`
- `crates/baml-rt-builder/src/builder/ts_gen.rs`
- `crates/baml-rt-builder/src/builder/compiler/codegen_pipeline.rs`
- `crates/baml-rt-tools/src/ts_gen.rs`
- `crates/baml-rt-quickjs/src/quickjs_bridge/tools.rs`
- `crates/baml-rt-quickjs/src/quickjs_bridge/types.rs`
- `crates/baml-rt-tools/src/tools.rs`, especially `OneShotSession::read` and `MultiSendSession::read`
- `docs/plans/tool-schema-graph.md`

Do not start by restoring the historical metadata renderer. Start with graph-to-TypeScript declarations and structured symbol ownership. The runtime API is added only after those foundations are tested.
