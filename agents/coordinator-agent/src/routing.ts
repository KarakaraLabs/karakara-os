/// <reference path="./baml-runtime.d.ts" />
/**
 * Routing decisions for the coordinator: parse the classifier's route, decide whether it can be
 * executed directly, and build the payload a specialist needs to skip its own classifier.
 *
 * Pure functions, no globals: importing this module has no side effects, so it can be tested
 * directly.
 */
import type { StructuredReply } from "./baml-runtime";

export type CoordinatorRouteTarget = {
  agent_package: string;
  capability: string;
};

export type RoutePriority = "urgent" | "high" | "normal" | "low";

/** A typed write the specialist can execute deterministically, with no re-classification. */
export type CoordinatorRouteOperation = {
  name: "update_task" | "delete_task";
  task_id: string | null;
  target_status: string | null;
  target_priority: RoutePriority | null;
};

export type CoordinatorRoute = {
  shape: "single" | "sequence" | "foreach" | "aggregate";
  targets: CoordinatorRouteTarget[];
  goal: string;
  filters: Record<string, unknown> | null;
  operation: CoordinatorRouteOperation | null;
  expected_output: string | null;
  unsupported: string[];
};

export type DeclaredRoutePayload = {
  schema: "agent-route/v1";
  capability: string;
  goal: string;
  raw_user_text: string;
  shape: CoordinatorRoute["shape"];
  filters: Record<string, unknown> | null;
  operation: CoordinatorRouteOperation | null;
  unsupported: string[];
};

const ROUTE_PRIORITIES: readonly RoutePriority[] = ["urgent", "high", "normal", "low"];

function optionalRouteText(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : null;
}

/**
 * Tolerant parse of a typed write operation. Returns `null` unless it names a supported operation
 * with at least one actionable field, so an empty or malformed operation falls back to the goal.
 */
export function parseRouteOperation(v: unknown): CoordinatorRouteOperation | null {
  if (!isObject(v)) return null;
  if (v.name === "delete_task") {
    return {
      name: "delete_task",
      task_id: optionalRouteText(v.task_id),
      target_status: null,
      target_priority: null,
    };
  }
  if (v.name !== "update_task") return null;
  const target_status = optionalRouteText(v.target_status);
  const target_priority =
    typeof v.target_priority === "string" &&
    ROUTE_PRIORITIES.includes(v.target_priority as RoutePriority)
      ? (v.target_priority as RoutePriority)
      : null;
  if (!target_status && !target_priority) return null;
  return {
    name: "update_task",
    task_id: optionalRouteText(v.task_id),
    target_status,
    target_priority,
  };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function firstTextPart(message: unknown): string | null {
  if (!isObject(message) || !Array.isArray(message.parts)) return null;
  for (const part of message.parts) {
    if (isObject(part) && typeof part.text === "string" && part.text.trim()) {
      return part.text.trim();
    }
  }
  return null;
}

/** The status-update prompt `awaitInput` attaches, carried as a JSON string on the chunk. */
function statusUpdatePrompt(statusUpdate: unknown): string | null {
  if (typeof statusUpdate !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(statusUpdate);
    return isObject(parsed) && isObject(parsed.status)
      ? firstTextPart(parsed.status.message)
      : null;
  } catch {
    return null;
  }
}

/** The newest question a suspended specialist asked, from a message or its input-required status. */
export function delegatedPromptFromOutput(output: unknown): string | null {
  if (!isObject(output) || !Array.isArray(output.chunks)) return null;
  for (const chunk of [...output.chunks].reverse()) {
    if (!isObject(chunk)) continue;
    const prompt = firstTextPart(chunk.message) ?? statusUpdatePrompt(chunk.statusUpdate);
    if (prompt) return prompt;
  }
  return null;
}

/** One specialist's final answer, as the coordinator keeps it for later steps and synthesis. */
export type SpecialistAnswer = { agent_package: string; goal: string; answer: string };

/** A delegated reply as plain text: text parts as written, data parts as their JSON string. */
export function replyText(reply: StructuredReply): string {
  return reply.parts
    .map((part) => (part.type === "text" ? part.text : part.data))
    .join("\n");
}

/**
 * The message for one plan step. `sequence` and `aggregate` steps are independent by definition, so
 * they get exactly the planned goal. Other steps may act on what came before (`foreach`, or a plan
 * with no declared route), and a specialist cannot see other agents' conversations, so earlier
 * answers travel in the message.
 */
export function planStepMessage(
  goal: string,
  earlier: SpecialistAnswer[],
  shape: CoordinatorRoute["shape"] | null,
): string {
  if (earlier.length === 0 || shape === "sequence" || shape === "aggregate") return goal;
  const answers = earlier.map((item) => `[${item.agent_package}]\n${item.answer}`).join("\n\n");
  return `${goal}\n\nResults from earlier steps:\n\n${answers}`;
}

/** Tolerant parse of the classifier's routing decision; invalid input becomes `null`. */
export function parseCoordinatorRoute(v: unknown): CoordinatorRoute | null {
  if (!isObject(v)) return null;
  const shape = v.shape;
  if (
    shape !== "single" &&
    shape !== "sequence" &&
    shape !== "foreach" &&
    shape !== "aggregate"
  ) {
    return null;
  }
  const goal = typeof v.goal === "string" ? v.goal.trim() : "";
  if (!goal) return null;
  const targets = Array.isArray(v.targets)
    ? v.targets
        .map((t) => {
          if (!isObject(t)) return null;
          const agent_package =
            typeof t.agent_package === "string" ? t.agent_package.trim() : "";
          const capability =
            typeof t.capability === "string" ? t.capability.trim() : "";
          return agent_package && capability ? { agent_package, capability } : null;
        })
        .filter((t): t is CoordinatorRouteTarget => t !== null)
    : [];
  const unsupported = Array.isArray(v.unsupported)
    ? v.unsupported.filter(
        (x): x is string => typeof x === "string" && x.trim().length > 0,
      )
    : [];
  return {
    shape,
    targets,
    goal,
    filters: isObject(v.filters) ? v.filters : null,
    operation: parseRouteOperation(v.operation),
    expected_output: typeof v.expected_output === "string" ? v.expected_output : null,
    unsupported,
  };
}

/**
 * Validate a declared single-target route against the discovered registry.
 *
 * A route is only usable when it names exactly one target, that target advertised the capability,
 * and the classifier did not report a constraint it could not encode. Anything else goes to the
 * planner, which is slower but can express multi-step work.
 */
export function directRouteTarget(
  route: CoordinatorRoute | null,
  agents: { agent_package: string; capabilities?: string[] | null }[],
): CoordinatorRouteTarget | null {
  if (!route || route.shape !== "single") return null;
  if (route.targets.length !== 1 || route.unsupported.length > 0) return null;
  const [target] = route.targets;
  const agent = agents.find((a) => a.agent_package === target.agent_package);
  if (!agent) return null;
  const advertised = Array.isArray(agent.capabilities) ? agent.capabilities : [];
  if (!advertised.includes(target.capability)) return null;
  return target;
}

/** Wire payload sent as an A2A data part so the specialist can skip its own classifier. */
export function declaredRoutePayload(
  route: CoordinatorRoute,
  target: CoordinatorRouteTarget,
  rawUserText: string,
): DeclaredRoutePayload {
  return {
    schema: "agent-route/v1",
    capability: target.capability,
    goal: route.goal,
    raw_user_text: rawUserText,
    shape: route.shape,
    filters: route.filters ?? null,
    operation: route.operation ?? null,
    unsupported: route.unsupported,
  };
}
