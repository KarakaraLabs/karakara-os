# Citable history and ref-addressed archives

**Agent onboarding:** [How to write agents](how-to-write-agents.md) § history and refs. This note is the **detailed contract** for the `#N` / `@N` vocabulary, where model-produced refs are still accepted, and why they were removed from session steps and replies.

## Ref vocabulary (still in force)

Prompt projection builds a **ref table** and renders it into `ctx.tags['conversation_transcript']`:

- `#N` addresses a **history** line (user, assistant, tool-call summary, session step).
- `@N`, `@N:L`, `@N:L1-L2` address an **archived tool result** and lines inside it.
- `!#N` / `!@N` negate a ref (planning refs only).

`@N` handles are **operational**. The model passes them to **SearchRead** / **PageRead** to read archived tool results. That addressing is unchanged.

## Where model-produced refs are accepted

Only planning carries model-produced refs:

- **JS / execution session:** `intent.citations`, `start_step` / `complete_step` payload `citations` (optional arrays).
- **Core bus:** `EffectEvent::IntentResolved { citations, .. }`, `PlanStepStatusChanged { citations, .. }`.
- **Provenance:** `ProvEventData::IntentResolved { citations, .. }`, `PlanStepStatusChanged { citations, .. }`.

Intents are valid without citations. **Message lineage** (`derived_from_message_ids`) is never accepted from agent JSON; the host binds it from the Rust invocation scope (`emit_scope.message_id()` in `baml_registration.rs`).

## What was removed, and why

Generated `Send` steps, session-plan decisions, and `StructuredReply` no longer carry a `citations` field, and the `LlmCompleted` provenance subscriber no longer audits model output for citations.

1. **Cost.** The audit ran inside the awaited `LlmCompleted` subscriber. It re-read up to 320 conversation items and rebuilt a ref table before the completion and transcript writes, so the next hop waited on it. The read's cost grew with the size of the whole provenance store, and citing hops stalled for 30 to 40 seconds in production.
2. **Validity.** The audit rebuilt a wider, later conversation view instead of using the exact ref table shown to the model. A citation could resolve during validation even if it was not in that prompt.
3. **Meaning.** A model citation is a claim the model makes after the fact about what it used, not an observed fact. Provenance records observed facts: prompts, outputs, tool arguments and results, tasks, agents, and delegation. Operators investigate what informed an output through that graph instead of through tokens attached to replies.

The result was diagnostic only. It logged a warning on a strict violation and never blocked a hop or changed agent behaviour.

## Compatibility

- Stored `LlmCallCompleted` events keep optional `citation_integrity`, `citations`, and `resolved_citations` fields so historic rows decode; new events leave them empty.
- Historic assistant messages may still carry `citations` (CITED edges); projection and the UI still render them.
- `KarakaraPolicy.strict_citation_anchors` is no longer consulted on the completion path.

## References in code

- Ref grammar and parsing: `crates/baml-rt-tools/src/citations.rs` (`ParsedCitation`, negation, line ranges).
- Ref table and projection: `crates/baml-rt-tools/src/archive_refs.rs`, `prompt_projection.rs`.
- Planning FSM: `crates/baml-rt-quickjs/src/planning.rs`, `execution_session_types.rs`, `quickjs_bridge/baml_registration.rs`.

## Related docs

- **Intent/plan prompting:** `docs/assertions/intent-based-planning-and-session-prompting.md`.
- Drift / injection framing: `docs/assertions/drift-catalogue.md`
