// --- Coordinator classification (before discovery) ---
// Exactly one variant: delegatable task ready, need user input to form a task, or meta-only (no routing).

class CoordinatorReadyIntent {
  kind "ready"
  inferred_intent string @description("Single goal-level sentence for agent discovery — coordinated outcome only. No agent names, tools, or delegation mechanics.")
}

class CoordinatorNeedTaskClarification {
  kind "clarify"
  question string @description("One concrete question whose answer would yield a delegatable specialist task.")
}

class CoordinatorMetaOnly {
  kind "meta"
  reason string @description("Short label for logging: e.g. greeting, capabilities_list, casual_chat — no specialist work request.")
}

function ClassifyCoordinatorTurn(user_message: string) -> CoordinatorReadyIntent | CoordinatorNeedTaskClarification | CoordinatorMetaOnly {
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
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You classify this turn for a **coordinator agent** that routes **real work** to specialist agents (after discovery). **Discovery of specialists runs only after** a delegatable task exists OR the user explicitly asks what agents or tools exist (capabilities orientation).

    Classify the latest turn using the conversation history for context.

    Concrete specialist work means a real delegatable task: lookups, searches, summaries scoped to a system,
    ticket or issue work, analysis with a defined goal, or multi-step investigation with identifiable deliverables.
    When that exists, `inferred_intent` must be one sentence describing the goal-level outcome only
    (no agent names, tools, or \"tell X to …\").

    When the user may want help but you still cannot name a routable task, ask one sharp question that would
    unlock concrete work on the next turn. Prefer that over inventing a fake actionable intent.

    Greetings, thanks, jokes, pure social chat, capabilities browsing, and other no-work turns should stay meta-only.

    Rules:
    - **Bare greetings** (\"hi\", \"hello\", \"hey\") with no other ask should trigger one clarification question, not a ready-to-route task.
    - If torn between ready work and clarification, prefer clarification.
    - Do not treat the turn as ready-to-route when the user gave no actionable anchor.

    {{ _.role('user') }}
    {{ user_message }}

{{ ctx.output_format }}

Return exactly one JSON object.
Select the object shape with discriminator `kind`:
- `kind: "ready"` -> CoordinatorReadyIntent
- `kind: "clarify"` -> CoordinatorNeedTaskClarification
- `kind: "meta"` -> CoordinatorMetaOnly
Set `kind` exactly. Do not mix fields from different object shapes.
Do not add text before or after the JSON object.
"#
}

// StructuredReply: shared synthesis shape in `_baml_runtime.baml` (builder prelude).

// Professional summary of what agents can do (no delegation).
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
function FormatCapabilities(user_message: string) -> StructuredReply {
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
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are the coordinator agent: calm, precise, and brief. Use prior turns from the transcript below when present for continuity.

    If the session history includes a tool discovery **Read** result, summarize those agents in clear professional language (what each is for, major capabilities).
    If there is **no** discovery listing in history (meta-only turn without specialist search), answer briefly: greet if appropriate and explain you are ready when they name a task or ask what coordination does — **do not invent** agent package names.
    Match the output schema exactly (StructuredReply: parts + citations). Two to five sentences in the text part; no internal tool jargon.

    {{ _.role('user') }}
    {{ user_message }}

{{ ctx.output_format }}

Return exactly one `StructuredReply` JSON object.
If `parts[]` is present, choose each item with discriminator `type`:
- `type: "text"` -> TextPart
- `type: "data"` -> DataPart
Set `type` exactly for each `parts[]` item.
Do not add text before or after the JSON object.
"#
}

// Planning step: output a structured plan against an explicit discovered-agent list.
// Never plan a step for coordinator-agent. For capability/meta requests, return plan_steps as [].
class CoordinatorAgentCandidate {
  agent_package string
  name string
  description string?
  capabilities string[]
  tools string[]
}

function MakeStructuredPlan(
  user_message: string,
  available_agents: CoordinatorAgentCandidate[],
) -> StandardStructuredPlan {
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
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are PLANNING what to do; you do not perform the task yourself.
    AVAILABLE AGENTS is the complete registry result for this routing decision. Use only exact `agent_package` values from this list. Never infer available agents from session history and never invent an agent package.

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    **Coordinator rule:** Non-empty `plan_steps` means at least one specialist conversation is justified by the user's message and the available-agent list. If the user gave a concrete task matching an available agent, include at least one step. If the user only wants orientation (capabilities, greetings, small talk), or no suitable agent is available, set `plan_steps` to `[]`.

    Output a structured plan:
    (1) intent_description — one sentence describing the user's high-level intent in concrete terms (not generic "delegate request").
    (2) objective — one sentence describing what we are achieving for the user.
    (3) plan_steps — an ordered list of agent chats. Each step is one conversation with one agent: which agent (agent_package from AVAILABLE AGENTS) and what to send (sub_message). Output the key as "plan_steps" (never "step").
    Intent/plan separation contract:
    - intent_description and objective MUST stay goal-level (user outcome), not execution mechanics.
    - plan_steps/sub_message MUST contain the execution mechanics.
    - Do not repeat the same sentence across intent_description, objective, and step descriptions.
    CRITICAL sub_message quality rules:
    - sub_message is a GOAL DESCRIPTION for the conversation with the specialist agent — it guides what the conversation should achieve, NOT a verbatim message to forward.
    - Be concrete and specific: include key terms, entities, desired output format.
    - Never include playful persona voice or "Delegate to X:" prefixes.
    Capability/meta rule:
    - If the user is asking what is available, what you can do, greetings/help, casual social chat, set plan_steps to [].
    A task may need one agent chat (one step) or multiple (multiple steps). Never plan a step for coordinator-agent. Output only the structured plan.

    {{ _.role('user') }}
    {{ user_message }}

{{ ctx.output_format }}

Return exactly one `StandardStructuredPlan` JSON object.
Do not add text before or after the JSON object.
"#
}

/// Coordinate with a specialist agent to achieve a goal.
/// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function DecideDelegationAction(
  goal: string,
  agent_package: string,
) -> SystemInternalA2aSessionPlan | string? {
  client DefaultClient
  prompt #"
    Goal: Coordinate with agent {{ agent_package }} to achieve: {{ goal }}
    Return `null` when the goal is satisfied or after 2+ exchanges.
  "#
}

// Synthesize the user-visible reply after delegation step(s).
// The session history carries the Read results from the preceding DecideDelegationAction calls.
function CoordinatorSynthesizeReply(
  user_message: string,
  plan_objective: string,
) -> StructuredReply {
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
Current step instructions:

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are the coordinator agent: professional, concise, and accurate. Use prior turns from the transcript below when present for continuity.

    The user asked: {{ user_message }}
    Plan objective: {{ plan_objective }}

    The transcript below contains the results from your specialist agent(s).
    Match the output schema exactly (StructuredReply: parts + citations).
    Synthesize across steps when needed; prefer bullet structure in markdown only when it improves clarity.

{{ ctx.output_format }}

Return exactly one `StructuredReply` JSON object.
If `parts[]` is present, choose each item with discriminator `type`:
- `type: "text"` -> TextPart
- `type: "data"` -> DataPart
Set `type` exactly for each `parts[]` item.
Do not add text before or after the JSON object.
"#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
