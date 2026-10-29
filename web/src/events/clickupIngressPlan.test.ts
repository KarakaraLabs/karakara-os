// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  PKG_CLICKUP_EXECUTE,
  PKG_CLICKUP_FORMAT,
  collapseReadOnlyExecuteSteps,
  isDeterministicTaskRead,
  validateClickUpPlanForExecution,
} from "../../../agents/clickup-agent/src/clickupExecution.ts";

type PlanInput = Parameters<typeof validateClickUpPlanForExecution>[0];

function plan(steps: PlanInput["plan_steps"]): PlanInput {
  return {
    intent_description: "Test",
    objective: "Test",
    plan_steps: steps,
    citations: null,
  };
}

describe("validateClickUpPlanForExecution", () => {
  it("accepts execute then format", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "List tasks",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Summarize",
        },
      ]),
    );
    expect(Array.isArray(result)).toBe(true);
  });

  it("rejects zero format steps", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "Only execute",
        },
      ]),
    );
    expect(result).toBe("plan must include exactly one clickup-format step");
  });

  it("rejects two format steps", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          agent_instance_id: "default",
          sub_message: "Execute",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format 1",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format 2",
        },
      ]),
    );
    expect(result).toBe("plan must include exactly one clickup-format step");
  });

  it("rejects discovery agent_package names", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: "clickup-agent",
          agent_instance_id: "default",
          sub_message: "Wrong package",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          agent_instance_id: "default",
          sub_message: "Format",
        },
      ]),
    );
    expect(typeof result).toBe("string");
    expect(result).toContain("invalid agent_package");
  });

  it("collapses consecutive read-only execute steps into one pass", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          sub_message: "List all teams.",
          expects: "navigation",
        },
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          sub_message: "For each team, list spaces.",
          expects: "navigation",
        },
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          sub_message: "List tasks filtered to in progress.",
          expects: "task_list",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          sub_message: "Markdown table.",
          expects: "none",
        },
      ]),
      "read",
    );
    expect(Array.isArray(result)).toBe(true);
    const steps = result as { sub_message: string; expects: string }[];
    expect(steps).toHaveLength(2);
    // Evidence does not carry across executor loops, so the merged step keeps the strongest need.
    expect(steps[0]!.expects).toBe("task_list");
    expect(steps[0]!.sub_message).toContain("List all teams.");
    expect(steps[0]!.sub_message).toContain("list spaces");
  });

  it("keeps write steps separate from reads", () => {
    const result = validateClickUpPlanForExecution(
      plan([
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          sub_message: "Create the task.",
          expects: "write_confirmation",
        },
        {
          agent_package: PKG_CLICKUP_EXECUTE,
          sub_message: "Verify it exists.",
          expects: "navigation",
        },
        {
          agent_package: PKG_CLICKUP_FORMAT,
          sub_message: "Confirm.",
          expects: "none",
        },
      ]),
      "write",
    );
    expect(Array.isArray(result)).toBe(true);
    expect(result as unknown[]).toHaveLength(3);
  });

  it("fills expects from the instruction when the plan omits it", () => {
    const steps = collapseReadOnlyExecuteSteps(
      validateClickUpPlanForExecution(
        plan([
          {
            agent_package: PKG_CLICKUP_EXECUTE,
            sub_message: "List all tasks in the list.",
          },
          { agent_package: PKG_CLICKUP_FORMAT, sub_message: "Answer." },
        ]),
        "read",
      ) as { expects: string }[],
    );
    expect(steps[0]!.expects).toBe("task_list");
  });

  it("routes typed task reads to the deterministic workflow", () => {
    const base = {
      kind: "intent" as const,
      intent: "list in-progress tasks",
      operation_kind: "read" as const,
      scope_hint: null,
      status_filter: null,
    };
    expect(isDeterministicTaskRead({ ...base, action: "list_tasks" })).toBe(true);
    expect(isDeterministicTaskRead({ ...base, action: "count_tasks" })).toBe(true);
    expect(isDeterministicTaskRead({ ...base, action: "navigate" })).toBe(false);
    expect(
      isDeterministicTaskRead({ ...base, action: "list_tasks", operation_kind: "write" }),
    ).toBe(false);
    // Constraints the deterministic traversal cannot express must fall back to the planner.
    expect(
      isDeterministicTaskRead({
        ...base,
        action: "list_tasks",
        intent: "list tasks assigned to me",
      }),
    ).toBe(false);
    expect(
      isDeterministicTaskRead({
        ...base,
        action: "list_tasks",
        intent: "list tasks due this week",
      }),
    ).toBe(false);
  });
});
