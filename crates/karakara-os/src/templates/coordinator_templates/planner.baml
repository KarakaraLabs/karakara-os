enum NodeKind {
  CallAgent    @alias("call_agent") @description("Send a prompt to a specialist agent via A2A")
  Foreach      @alias("foreach") @description("Expand items from upstream output into child agent calls")
  Synthesize   @alias("synthesize") @description("Merge prior node outputs into final answer")
  Clarify      @alias("clarify") @description("Ask user for clarification before proceeding")
  DirectAnswer @alias("direct_answer") @description("Coordinator answers directly without delegation")
}

class WorkflowTarget {
  agent_package string
  agent_instance_id string
}

class ForeachTemplate {
  id_prefix string
  target WorkflowTarget
  prompt_template string @description("Interpolates {{item.title}}, {{item.description}}, etc.")
  max_items int? @description("Maximum number of expansions. Default 50.")
}

class WorkflowNode {
  id string
  kind NodeKind
  depends_on string[]
  target WorkflowTarget?
  prompt_template string?
  foreach_from string?
  foreach_template ForeachTemplate?
  synthesis_template string?
  rationale string?
}

class WorkflowPlan {
  goal string
  nodes WorkflowNode[]
  final_node_id string?
}

class AgentCandidate {
  agent_package string
  agent_instance_id string
  name string
  description string?
  capabilities string[]
  tools string[]
}

function PlanCoordinatorWorkflow(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?
) -> WorkflowPlan {
  client DefaultClient
  prompt #"
    You are a coordinator planner that decomposes user requests into a workflow DAG.
    Return a WorkflowPlan with explicit dependencies and node kinds.

    Rules:
    - Use call_agent for specialist calls.
    - Use foreach only when the user asks to apply an action to each item from upstream output.
    - Use clarify only when routing is genuinely ambiguous AND no reasonable best-effort execution path exists.
    - Use direct_answer for simple meta/system questions.
    - Use ONLY agent_package/agent_instance_id pairs that appear in AVAILABLE AGENTS.
    - If no suitable specialist exists, use direct_answer or clarify instead of inventing a target.
    - For direct_answer, prompt_template must be the literal final user-facing answer text.
    - Include a synthesize node as the final aggregation step when multiple call_agent nodes are present.
    - Nodes with empty depends_on run in parallel.
    - Never target the coordinator itself.
    - Cap nodes at 30.

    {{ ctx.output_format }}

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    {% if conversation_context %}
    ---CONVERSATION CONTEXT---
    {{ conversation_context }}
    ---END CONVERSATION CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---
  "#
}

// --- Routing (cheap, before planning) ---
// Resolves one user turn to the specialist capabilities it needs. A single-target route lets the
// host delegate directly; anything wider falls through to PlanCoordinatorWorkflow.

class CoordinatorRouteTarget {
  agent_package string @description("Exact agent_package from AVAILABLE AGENTS.")
  agent_instance_id string @description("Exact agent_instance_id from AVAILABLE AGENTS.")
  capability string @description("One capability that agent advertises. Copy it verbatim.")
}

class CoordinatorRouteFilters {
  status string? @description("Task status the user named, copied verbatim. Omit unless they filtered by status.")
  assignee string? @description("Person the user named for an assignee filter. Never for 'me'/'my tasks': leave null and list it in unsupported.")
  priority ("urgent" | "high" | "normal" | "low")? @description("Priority filter only when the user filtered by priority.")
  due ("overdue" | "today" | "this_week" | "next_week")? @description("Due-date window only when the user filtered by it.")
  text string? @description("Words the task name or description must contain, copied verbatim.")
}

class CoordinatorRoute {
  shape "single" | "sequence" | "foreach" | "aggregate" @description("single: one capability answers the whole request. sequence/foreach/aggregate need a workflow plan.")
  targets CoordinatorRouteTarget[] @description("Specialists this request needs. Exactly one for shape single.")
  goal string @description("Goal-level instruction for the target agent, keeping the user's own terms.")
  filters CoordinatorRouteFilters? @description("Optional task filters for task-tracking agents. Merely displaying a field is not a filter.")
  expected_output string? @description("What the answer must contain, in one short clause.")
  unsupported string[]? @description("Constraints no target capability can express. A non-empty value forces the planning path, so keep it empty unless needed.")
}

function RouteCoordinatorRequest(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?,
) -> CoordinatorRoute {
  client DefaultClient
  prompt #"
    You are a coordinator router. Decide which specialist capabilities this turn needs. Do not plan
    steps and do not write the answer.

    Rules:
    - AVAILABLE AGENTS is the complete registry result for this turn. Use only exact
      agent_package/agent_instance_id pairs and capability strings from it. Never invent either.
    - shape "single" only when one capability answers the whole request; then targets has exactly
      one entry.
    - shape "sequence" for several independent calls, "foreach" when an action applies to each item
      produced upstream (e.g. "create a task for each document"), "aggregate" when several results
      must be merged. The host will plan those.
    - goal must stay faithful to the user: keep their terms (status names, people, dates, titles) and
      do not add requirements they did not state.
    - filters applies to task-tracking agents, and only for real filters. Wanting a field shown is
      not a filter.
    - unsupported only for constraints no capability can express (custom fields, grouping,
      current-user identity, ordering by an unlisted property). It routes to the slower planner.

    Conversation continuity:
    - If an earlier turn established a task filter (status, assignee, priority, due, or an area) and the
      new message does not change it, carry that filter into `filters`. Example: after asking about
      in-progress tasks, "which should I start first?" still means in-progress.
    - Never invent a filter the user has not stated in this conversation.


    {{ ctx.output_format }}

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    {% if conversation_context %}
    ---CONVERSATION CONTEXT---
    {{ conversation_context }}
    ---END CONVERSATION CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
