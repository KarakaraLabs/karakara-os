/// <reference path="./baml-runtime.d.ts" />
/**
 * Routing decisions for the coordinator: parse the classifier's route, decide whether it can be
 * executed directly, and build the payload a specialist needs to skip its own classifier.
 *
 * Pure functions, no globals: importing this module has no side effects, so it can be tested
 * directly.
 */

export type CoordinatorRouteTarget = {
  agent_package: string;
  capability: string;
};

export type RoutePriority = "urgent" | "high" | "normal" | "low";

/** A typed write the specialist can execute deterministically, with no re-classification. */
export type CoordinatorRouteOperation = {
  name: "update_task";
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
  if (!isObject(v) || v.name !== "update_task") return null;
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
