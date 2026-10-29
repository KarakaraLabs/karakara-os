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
class ListTeamsInput {
}

class ListSpacesInput {
  team_id string @description("ClickUp team (workspace) ID \u{2014} obtain from ListTeams.")
}

class ListListsInput {
  space_id string @description("ClickUp space ID \u{2014} obtain from ListSpaces.")
}

class ListTasksInput {
  list_id string @description("ClickUp list ID \u{2014} obtain from ListLists.")
}

class GetTaskInput {
  task_id string @description("ClickUp task ID \u{2014} obtain from ListTasks.")
}

class CreateTaskInput {
  list_id string @description("ClickUp list ID where the task will be created \u{2014} obtain from ListLists.")
  name string @description("Task title / name.")
  description (string | null)? @description("Optional long-form task description (Markdown supported).")
  priority (int | null)? @description("ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")
}

class UpdateTaskInput {
  task_id string @description("ClickUp task ID to update \u{2014} obtain from ListTasks or GetTask.")
  status (string | null)? @description("New task status string (e.g. \'in progress\', \'complete\').")
  description (string | null)? @description("Updated task description (Markdown supported).")
  priority (int | null)? @description("ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")
}

class DeleteTaskInput {
  task_id string @description("ClickUp task ID to delete \u{2014} obtain from ListTasks.")
  confirm_delete bool @description("Must be true to proceed. Always confirm deletion with the user first.")
}

type ClickUpInput = ListTeamsInput | ListSpacesInput | ListListsInput | ListTasksInput | GetTaskInput | CreateTaskInput | UpdateTaskInput | DeleteTaskInput

class ClickUpTaskSummary {
  id string
  name string
  status string
  description (string | null)?
  url string
  assignees string[]?
  priority (string | null)?
  due_date (string | null)?
  @@dynamic
}

class ClickUpItem {
  id string
  name string
  kind string @description("The kind of resource: team, space, or list.")
  @@dynamic
}

class ClickUpOutput {
  tasks ClickUpTaskSummary[]?
  items ClickUpItem[]? @description("Teams, spaces, or lists returned by navigation actions.")
  message string
  @@dynamic
}

class ConversationPart {
  text (string | null)? @description("Plain text content for this conversation part.")
  data (OpaqueJson | null)? @description("Structured JSON content for this conversation part.")
  raw (string | null)? @description("Raw provider-native content when plain text would be lossy or unavailable.")
  url (string | null)? @description("Remote URL for this part when the content points to a web or attachment resource.")
  filename (string | null)? @description("Attachment filename when this part refers to a file.")
  mediaType (string | null)? @description("MIME type for this part, for example text/plain or image/png.")
  @@dynamic
}

class ConversationMessage {
  role (string | null)?
  parts ConversationPart[]
  @@dynamic
}

class ConversationChunk {
  message (ConversationMessage | null)?
  task (string | null)?
  statusUpdate (string | null)?
  artifactUpdate (string | null)?
  @@dynamic
}

enum InternalA2aCompletion {
  DONE
  COMPLETED
  FAILED
  CANCELED
  REJECTED
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  AUTHREQUIRED
    @alias("AUTH_REQUIRED")
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

class InternalA2aNextOutput {
  chunks ConversationChunk[]
  completion (InternalA2aCompletion | null)?
  historyContext (HistoryContextV1 | null)?
  @@dynamic
}

class InternalA2aTarget {
  agent_package string
  @@dynamic
}

class InternalA2aOpenInput {
  target InternalA2aTarget
  @@dynamic
}

class InternalA2aSendInput {
  parts ConversationPart[]
  @@dynamic
}

class SystemOneUsage {
  input_tokens (int | null)?
  output_tokens (int | null)?
}

class ChoiceAnswer {
  choice string
  probabilities map<string, float>
  confidence float
  type "choice"
}

class ScoreAnswer {
  score float
  probabilities map<string, float>
  confidence float
  type "score"
}

class NoulAnswer {
  noul float
  type "noul"
}

type SystemOneAnswer = ChoiceAnswer | ScoreAnswer | NoulAnswer

class SystemOneOutput {
  model string
  answers map<string, SystemOneAnswer>
  usage (SystemOneUsage | null)?
}

class ChoiceQuestion {
  instructions OpaqueJson @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  criteria map<string, OpaqueJson>
  type "choice"
}

class ScoreQuestion {
  instructions OpaqueJson @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  criteria OpaqueJson[]
  type "score"
}

class NoulCriteria {
  field_true OpaqueJson @alias("true") @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  field_false OpaqueJson @alias("false") @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
}

class NoulQuestion {
  instructions OpaqueJson @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  criteria (NoulCriteria | null)?
  type "noul"
}

type SystemOneQuestion = ChoiceQuestion | ScoreQuestion | NoulQuestion

class SystemOneInput {
  state OpaqueJson @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  questions map<string, SystemOneQuestion>
  model (string | null)?
  timeout_ms (int | null)?
}

/// Tool card for support/clickup: metadata for polymorphic Open selection.
class SupportClickupToolCard {
  tool_name "support/clickup"
  description "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks."
  session_policy "Strict"
  capability "OneShot"
  invocation_mode "single_shot"
  input_summary "ClickUpInput"
  tags string[] @description("Tool tags: support, clickup")
}

class SupportClickupOpenStep {
  op "Open"
  tool_name "support/clickup" @description("Tool to open")
}

class SupportClickupSendStep {
  op "Send"
  tool_name "support/clickup" @description("Tool this Send targets")
  input ClickUpInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class SupportClickupSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

class SupportClickupPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SupportClickupFinishStep {
  op "Finish"
}

class SupportClickupAbortStep {
  op "Abort"
}

type SupportClickupSessionStep = SupportClickupOpenStep | SupportClickupSendStep | SupportClickupSearchReadStep | SupportClickupPageReadStep | SupportClickupFinishStep | SupportClickupAbortStep

class SupportClickupSessionPlan {
  step SupportClickupSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: write.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for typesafe/system_one: metadata for polymorphic Open selection.
class TypesafeSystemOneToolCard {
  tool_name "typesafe/system_one"
  description "Ask a TypeSafe System One model independently evaluated Choice, Score, and Noul questions about supplied JSON state."
  session_policy "Strict"
  capability "OneShot"
  invocation_mode "single_shot"
  input_summary "SystemOneInput"
  tags string[] @description("Tool tags: typesafe, system_one, read")
}

class TypesafeSystemOneOpenStep {
  op "Open"
  tool_name "typesafe/system_one" @description("Tool to open")
}

class TypesafeSystemOneSendStep {
  op "Send"
  tool_name "typesafe/system_one" @description("Tool this Send targets")
  input SystemOneInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class TypesafeSystemOneSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

class TypesafeSystemOnePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class TypesafeSystemOneFinishStep {
  op "Finish"
}

class TypesafeSystemOneAbortStep {
  op "Abort"
}

type TypesafeSystemOneSessionStep = TypesafeSystemOneOpenStep | TypesafeSystemOneSendStep | TypesafeSystemOneSearchReadStep | TypesafeSystemOnePageReadStep | TypesafeSystemOneFinishStep | TypesafeSystemOneAbortStep

class TypesafeSystemOneSessionPlan {
  step TypesafeSystemOneSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/internal_a2a: metadata for polymorphic Open selection.
class SystemInternalA2aToolCard {
  tool_name "system/internal_a2a"
  description "Opens a conversational session to another agent. Send a message (text or parts), Read the response, then Send follow-ups or Finish. Multiple Send/Read rounds are allowed within one session — use this for multi-turn conversations with the delegated agent."
  session_policy "MultiSend"
  capability "Streaming"
  invocation_mode "session"
  input_summary "InternalA2aSendInput"
  tags string[] @description("Tool tags: system, a2a")
}

class SystemInternalA2aOpenStep {
  op "Open"
  tool_name "system/internal_a2a" @description("Tool to open")
  initial_input InternalA2aOpenInput @description("Required open payload.")
}

class SystemInternalA2aSendStep {
  op "Send"
  tool_name "system/internal_a2a" @description("Tool this Send targets")
  input InternalA2aSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class SystemInternalA2aSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. A matching field inside a rendered array item materializes the complete record; non-record content returns matching lines. Continue with the returned next_offset when more results remain. For contiguous unfiltered text, use PageRead instead.")
}

class SystemInternalA2aPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemInternalA2aFinishStep {
  op "Finish"
}

class SystemInternalA2aAbortStep {
  op "Abort"
}

type SystemInternalA2aSessionStep = SystemInternalA2aOpenStep | SystemInternalA2aSendStep | SystemInternalA2aSearchReadStep | SystemInternalA2aPageReadStep | SystemInternalA2aFinishStep | SystemInternalA2aAbortStep

class SystemInternalA2aSessionPlan {
  step SystemInternalA2aSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes above — do not paraphrase them in prose. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. For A2A Send, `input.text` is required when sending. Access: write.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Entry step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function ChooseClickUpAction__entry(
  goal: string,
  step_description: string,
  operation_kind: string,
  prior_results: string?,
  session_context: SessionContext?
) -> ArchivePageReadStep | ArchiveSearchReadStep | FinalResponse | ReadOnlyFinishStep | SupportClickupSendStep {

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

You are a ClickUp workspace assistant executing one plan step.
Stay focused on this step only and use prior tool evidence from this turn.

Overall goal: {{ goal }}
Current step: {{ step_description }}
Operation kind: {{ operation_kind }}

Domain rules:
- Reuse IDs from prior_results / history; do not invent IDs for required fields.
- For read steps, gather only the data needed for this step and then complete.
- For write steps, perform the requested change and then confirm what changed.
- For delete operations: require explicit confirmation flow. First send confirm_delete=false; only send confirm_delete=true after clear user confirmation in prior results/history.
- For final formatting steps, return FinalResponse with a user-ready message grounded in retrieved results.

{% if prior_results %}
Results from previous steps (use IDs and data from here):
{{ prior_results }}
{% endif %}

Return exactly one JSON object of type `ArchivePageReadStep | ArchiveSearchReadStep | FinalResponse | ReadOnlyFinishStep | SupportClickupSendStep`.

Choose the matching object type.
- `ArchivePageReadStep` uses fields like `input`, `op`
- `ArchiveSearchReadStep` uses fields like `input`, `op`
- `FinalResponse` uses fields like `citations`, `message`, `structured_json`
- `ReadOnlyFinishStep` uses fields like `op`, `reply`
- `SupportClickupSendStep` uses fields like `citations`, `input`, `op`, `tool_name`
Do not mix fields from different object shapes.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: eligible one-shot tools may emit Send directly (runtime auto-opens and auto-finishes); Open remains valid during migration. Finish and Abort are excluded on entry.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function ChooseClickUpAction__active__support_clickup(
  goal: string,
  step_description: string,
  operation_kind: string,
  prior_results: string?,
  session_context: SessionContext?
) -> SupportClickupSendStep | SupportClickupSearchReadStep | SupportClickupPageReadStep | SupportClickupFinishStep | SupportClickupAbortStep {

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

You are a ClickUp workspace assistant executing one plan step.
Stay focused on this step only and use prior tool evidence from this turn.

Overall goal: {{ goal }}
Current step: {{ step_description }}
Operation kind: {{ operation_kind }}

Domain rules:
- Reuse IDs from prior_results / history; do not invent IDs for required fields.
- For read steps, gather only the data needed for this step and then complete.
- For write steps, perform the requested change and then confirm what changed.
- For delete operations: require explicit confirmation flow. First send confirm_delete=false; only send confirm_delete=true after clear user confirmation in prior results/history.
- For final formatting steps, return FinalResponse with a user-ready message grounded in retrieved results.

{% if prior_results %}
Results from previous steps (use IDs and data from here):
{{ prior_results }}
{% endif %}

Return exactly one JSON object of type `SupportClickupAbortStep | SupportClickupFinishStep | SupportClickupPageReadStep | SupportClickupSearchReadStep | SupportClickupSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
- After Send completes with status done, the next hop is a consume phase.
"#

}

/// Result-consumption executor: archive reads or terminal synthesis after Send.
function ChooseClickUpAction__consume__support_clickup(
  goal: string,
  step_description: string,
  operation_kind: string,
  prior_results: string?,
  session_context: SessionContext?
) -> FinalResponse | ReadOnlyFinishStep | SupportClickupPageReadStep | SupportClickupSearchReadStep {

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

You are a ClickUp workspace assistant executing one plan step.
Stay focused on this step only and use prior tool evidence from this turn.

Overall goal: {{ goal }}
Current step: {{ step_description }}
Operation kind: {{ operation_kind }}

Domain rules:
- Reuse IDs from prior_results / history; do not invent IDs for required fields.
- For read steps, gather only the data needed for this step and then complete.
- For write steps, perform the requested change and then confirm what changed.
- For delete operations: require explicit confirmation flow. First send confirm_delete=false; only send confirm_delete=true after clear user confirmation in prior results/history.
- For final formatting steps, return FinalResponse with a user-ready message grounded in retrieved results.

{% if prior_results %}
Results from previous steps (use IDs and data from here):
{{ prior_results }}
{% endif %}

Return exactly one JSON object of type `FinalResponse | ReadOnlyFinishStep | SupportClickupPageReadStep | SupportClickupSearchReadStep`.

Choose the matching object type.
- `FinalResponse` uses fields like `citations`, `message`, `structured_json`
- `ReadOnlyFinishStep` uses fields like `op`, `reply`
- `SupportClickupPageReadStep` uses fields like `input`, `op`
- `SupportClickupSearchReadStep` uses fields like `input`, `op`
Do not mix fields from different object shapes.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this is a result-consumption phase and excludes `Send` and `Open`.
- Use PageRead or SearchRead with the completed archive receipt when result lines are needed.
- Otherwise emit a terminal response to explicitly consume the receipt.
"#

}
