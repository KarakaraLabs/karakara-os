// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan parent `prompt`: task + domain lines only; generated per-phase executors
// add archive prefix, optional tool_schema_prelude, narrowed union, transcript, and output binding.

// Shared standard planning types
class StandardAgentPlanStep {
  agent_package string @description("Exact agent_package from discovery results (e.g. 'extrospection-agent'). Copy verbatim — do not truncate or reformat.")
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
  citations string[]? @description("Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].")
}

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is a plan-local alias for `startStep` / `completeStep` — not a global provenance identifier;
/// the host derives canonical step entity ids from task scope + plan + this slug.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {
  step_id string @description("Plan-local slug (may be LLM-authored); not globally unique — host compounds with task/plan for canonical ids. Reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids (same alias namespace as step_id); omit or [] if none.")
}

class ArchiveLedgerEntry {
  archive_ref string @description("Archive handle, e.g. @7")
  header string @description("Compact tool/action identity for this Send, without result body")
}

class ArchiveReceipt {
  archive_ref string @description("Resolvable archive handle from a completed Send")
  tool_name string @description("Qualified tool name that produced this archive")
  action_identity string @description("Bounded Send action and input identity, without result body")
  line_count int @description("Rendered archive line count")
  byte_count int @description("Rendered archive byte count")
  size_display string @description("Human-readable rendered archive size")
  completion string @description("Completion state for the archived Send")
  search_hints string[]? @description("Trusted searchable field names when supplied by tool metadata")
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  status string @description("FSM status: awaiting_open | just_opened | result_ready | done. result_ready means previous direct Send completed and its archive is available; read it or move on, do not re-Send same input.")
  last_receipt ArchiveReceipt? @description("Latest completed Send receipt. The archive body is not in this object.")
  last_step_op string? @description("Previous FSM step op when known: open | send | duplicate_send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | duplicate | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
  archives ArchiveLedgerEntry[]? @description("Bounded ledger of Send archives already materialized this executor session. Use it to avoid identical Sends; PageRead/SearchRead existing refs when details are needed.")
}

/// Opaque JSON transport wrapper for host-managed tools and event payloads.
/// Generated BAML callers pass serialized JSON in `__baml_opaque_json`.
/// Direct host-side callers may still send arbitrary raw JSON.
class OpaqueJson {
  opaque_json string @alias("__baml_opaque_json") @description("Serialized JSON payload.")
}

// Structured reply types for synthesis functions
enum ReplyMediaType {
  TextPlain @alias("text/plain")
  TextMarkdown @alias("text/markdown")
  ApplicationJson @alias("application/json")
  TextCsv @alias("text/csv")
}

class TextPart {
  type "text"
  text string
}

class DataPart {
  type "data"
  data string @description("Serialised content (e.g. JSON string, CSV string).")
  media_type ReplyMediaType @description("MIME type of the data field.")
}

type ReplyPart = TextPart | DataPart

// Canonical synthesis + wire shape: same type in BAML and TypeScript (SessionResult.message, emitters).
class StructuredReply {
  parts ReplyPart[] @description("Ordered user-visible or machine-readable parts. A reply may contain TextPart (type \"text\"), DataPart (type \"data\", media_type ApplicationJson) with a JSON string, or both. Set media_type TextMarkdown on TextPart for rich answers.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Line-filtered archive read (emit op SearchRead).
class ArchiveSearchReadInput {
  archive_ref string @description("archive_ref names a Send output archive: set it to the exact @N shown on that Send line in the transcript (e.g. @1, @2). Do not use #N or any # prefix — # is for session history lines only and is invalid in this field. Do not invent @N. PageRead/SearchRead materialize line bodies from that handle; allowed with a real visible @N without Open or an active session.")
  grep string @description("Non-empty filter over rendered archive lines. When a match belongs to a rendered array record, SearchRead returns that complete record; other content falls back to matching lines. Literal substring by default; a bare `|` means regex alternation (e.g. `in progress|done`); prefix `-E` for full regex, `-F` to force literal, `-i` for case-insensitive. For contiguous unfiltered text, use PageRead.")
  offset int? @description("0-based offset over filtered result units. A unit is a complete record when record metadata applies, otherwise one matching line. First window: offset=0; continue with the returned next_offset.")
  limit int? @description("Max result units in this read window. SearchRead units may be complete records; PageRead units are lines. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("archive_ref names a Send output archive: set it to the exact @N shown on that Send line in the transcript (e.g. @1, @2). Do not use #N or any # prefix — # is for session history lines only and is invalid in this field. Do not invent @N. PageRead/SearchRead materialize line bodies from that handle; allowed with a real visible @N without Open or an active session.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.")
  limit int? @description("Max result units in this read window. SearchRead units may be complete records; PageRead units are lines. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

/// Terminal hop without a host tool session: answer from visible conversation archives only.
/// Do not emit when you need to invoke a tool — emit a Send instead (eligible one-shot tools Send directly on entry; tools needing configuration Open first).
class ReadOnlyFinishStep {
  op "ReadOnlyFinish"
  reply StructuredReply @description("User-visible answer; citations must include every @N archive line evidence relied on — omitting citations when grounding on archives is invalid.")
}
// Domain types generated from the canonical tool schema graph
enum MathOperation {
  Variant0
    @alias("+")
  Variant1
    @alias("-")
  Variant2
    @alias("*")
  Variant3
    @alias("/")
}

class Expression {
  left int @description("Left-hand integer operand.")
  operation MathOperation @description("Arithmetic operation to apply.")
  right int @description("Right-hand integer operand. Must not be 0 when operation is Divide.")
  @@dynamic
}

class CalculatorInput {
  expression Expression @description("The binary expression to evaluate.")
  @@dynamic
}

class CalculatorOutput {
  expression string
  result float
  formatted string
  @@dynamic
}

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  eventSources string[]?
  @@dynamic
}

enum HistoryContextSessionOp {
  SearchRead
  PageRead
}

enum HistoryContextStatus {
  Done
    @alias("done")
  Streaming
    @alias("streaming")
  Suspended
    @alias("suspended")
  Error
    @alias("error")
}

class HistoryContextV1 {
  hop int
  op HistoryContextSessionOp
  status HistoryContextStatus
  truncated bool?
  cursor (string | null)?
  payload (map<string, OpaqueJson> | null)?
  @@dynamic
}

class DiscoverToolsNextOutput {
  tools ToolDiscoveryRecordDto[]
  done bool
  historyContext (HistoryContextV1 | null)?
  @@dynamic
}

class DiscoverToolsOpenInput {
  reason (string | null)?
  @@dynamic
}

class DiscoverToolsSendInput {
  query (string | null)?
  limit (int | null)?
  @@dynamic
}

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscoverToolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  capability "Streaming"
  invocation_mode "session"
  input_summary "DiscoverToolsSendInput"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscoverToolsOpenStep {
  op "Open"
  tool_name "system/discover_tools" @description("Tool to open")
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
}

class SystemDiscoverToolsSendStep {
  op "Send"
  tool_name "system/discover_tools" @description("Tool this Send targets")
  input DiscoverToolsSendInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class SystemDiscoverToolsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

class SystemDiscoverToolsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemDiscoverToolsFinishStep {
  op "Finish"
}

class SystemDiscoverToolsAbortStep {
  op "Abort"
}

type SystemDiscoverToolsSessionStep = SystemDiscoverToolsOpenStep | SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsAbortStep

class SystemDiscoverToolsSessionPlan {
  step SystemDiscoverToolsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for support/calculate: metadata for polymorphic Open selection.
class SupportCalculateToolCard {
  tool_name "support/calculate"
  description "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division."
  session_policy "Strict"
  capability "OneShot"
  invocation_mode "single_shot"
  input_summary "CalculatorInput"
  tags string[] @description("Tool tags: support, calculate")
}

class SupportCalculateOpenStep {
  op "Open"
  tool_name "support/calculate" @description("Tool to open")
}

class SupportCalculateSendStep {
  op "Send"
  tool_name "support/calculate" @description("Tool this Send targets")
  input CalculatorInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class SupportCalculateSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

class SupportCalculatePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SupportCalculateFinishStep {
  op "Finish"
}

class SupportCalculateAbortStep {
  op "Abort"
}

type SupportCalculateSessionStep = SupportCalculateOpenStep | SupportCalculateSendStep | SupportCalculateSearchReadStep | SupportCalculatePageReadStep | SupportCalculateFinishStep | SupportCalculateAbortStep

class SupportCalculateSessionPlan {
  step SupportCalculateSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Entry step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function ChooseDiscoverToolsQuery__entry(
  user_message: string,
  session_context: SessionContext
) -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SystemDiscoverToolsOpenStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
{% if session_context.last_receipt %}
Completed tool result:
{{ session_context.last_receipt.archive_ref }} · {{ session_context.last_receipt.tool_name }}:{{ session_context.last_receipt.action_identity }} · {{ session_context.last_receipt.line_count }}L · {{ session_context.last_receipt.size_display }} · {{ session_context.last_receipt.completion }}
Result body is archived and is not visible yet.
{% if session_context.last_receipt.search_hints %}Search hints: {{ session_context.last_receipt.search_hints | join(', ') }}
{% endif %}Next: SearchRead using this archive ref with a concrete filter, PageRead using this archive ref with a small limit, or finish. Do not repeat the completed Send.
{% endif %}
{% if session_context.archives %}Recent archives:
{% for archive in session_context.archives %}{{ archive.archive_ref }} · {{ archive.header }}
{% endfor %}{% endif %}
Current step instructions:

Find tools that can help with: {{ user_message }}

Return exactly one JSON object of type `ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SystemDiscoverToolsOpenStep`.

Use `op` as the discriminator: "Open" | "PageRead" | "ReadOnlyFinish" | "SearchRead".
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this entry return union excludes `Send`, `Finish`, and `Abort`.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function ChooseDiscoverToolsQuery__active__system_discover_tools(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsAbortStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
{% if session_context.last_receipt %}
Completed tool result:
{{ session_context.last_receipt.archive_ref }} · {{ session_context.last_receipt.tool_name }}:{{ session_context.last_receipt.action_identity }} · {{ session_context.last_receipt.line_count }}L · {{ session_context.last_receipt.size_display }} · {{ session_context.last_receipt.completion }}
Result body is archived and is not visible yet.
{% if session_context.last_receipt.search_hints %}Search hints: {{ session_context.last_receipt.search_hints | join(', ') }}
{% endif %}Next: SearchRead using this archive ref with a concrete filter, PageRead using this archive ref with a small limit, or finish. Do not repeat the completed Send.
{% endif %}
{% if session_context.archives %}Recent archives:
{% for archive in session_context.archives %}{{ archive.archive_ref }} · {{ archive.header }}
{% endfor %}{% endif %}
Current step instructions:

Find tools that can help with: {{ user_message }}

Return exactly one JSON object of type `SystemDiscoverToolsAbortStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
- After Send completes with status done, the next hop is a consume phase.
"#

}

/// Result-consumption executor: archive reads or terminal synthesis after Send.
function ChooseDiscoverToolsQuery__consume__system_discover_tools(
  user_message: string,
  session_context: SessionContext
) -> ReadOnlyFinishStep | SystemDiscoverToolsAbortStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsSearchReadStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
{% if session_context.last_receipt %}
Completed tool result:
{{ session_context.last_receipt.archive_ref }} · {{ session_context.last_receipt.tool_name }}:{{ session_context.last_receipt.action_identity }} · {{ session_context.last_receipt.line_count }}L · {{ session_context.last_receipt.size_display }} · {{ session_context.last_receipt.completion }}
Result body is archived and is not visible yet.
{% if session_context.last_receipt.search_hints %}Search hints: {{ session_context.last_receipt.search_hints | join(', ') }}
{% endif %}Next: SearchRead using this archive ref with a concrete filter, PageRead using this archive ref with a small limit, or finish. Do not repeat the completed Send.
{% endif %}
{% if session_context.archives %}Recent archives:
{% for archive in session_context.archives %}{{ archive.archive_ref }} · {{ archive.header }}
{% endfor %}{% endif %}
Current step instructions:

Find tools that can help with: {{ user_message }}

Return exactly one JSON object of type `ReadOnlyFinishStep | SystemDiscoverToolsAbortStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsSearchReadStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "ReadOnlyFinish" | "SearchRead".
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this is a result-consumption phase and excludes `Send` and `Open`.
- Use PageRead or SearchRead with the completed archive receipt when result lines are needed.
- Otherwise emit a terminal response to explicitly consume the receipt.
"#

}
