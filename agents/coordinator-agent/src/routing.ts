/// <reference path="./baml-runtime.d.ts" />
/**
 * Routing decisions for the coordinator: parse the classifier's route, decide whether it can be
 * executed directly, and build the generic routing metadata forwarded to the specialist.
 *
 * Pure functions, no globals: importing this module has no side effects, so it can be tested
 * directly.
 */
import type { StructuredReply } from "./baml-runtime";

export type CoordinatorRouteTarget = {
  agent_package: string;
  capability: string;
};

export type CoordinatorRoute = {
  shape: "single" | "sequence" | "foreach" | "aggregate";
  targets: CoordinatorRouteTarget[];
  goal: string;
  expected_output: string | null;
  unsupported: string[];
};

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
 * The message for one plan step. A specialist cannot see other agents' conversations, so every
 * earlier answer travels in the message: a downstream step that depends on upstream output (e.g.
 * "get the titles, then create tasks from them") receives it, and a step that happens not to need it
 * simply ignores the extra context. This keeps multi-step plans correct without the coordinator
 * having to classify, per step, whether it is dependent — a distinction the planner does not encode
 * reliably. The first step (no earlier answers) gets exactly the planned goal.
 */
export function planStepMessage(goal: string, earlier: SpecialistAnswer[]): string {
  if (earlier.length === 0) return goal;
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
