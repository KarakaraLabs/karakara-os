// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { describe, expect, it } from "vitest";
import {
  PKG_CLICKUP_EXECUTE,
  PKG_CLICKUP_FORMAT,
  applyTaskFilters,
  collapseReadOnlyExecuteSteps,
  describePriorityAbstain,
  isDeterministicTaskRead,
  taskFiltersFromIntent,
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
      statuses: null,
      assignee_filter: null,
      priority_filter: null,
      due_filter: null,
      text_filter: null,
      unsupported: null,
    };
    expect(isDeterministicTaskRead({ ...base, action: "list_tasks" })).toBe(true);
    expect(isDeterministicTaskRead({ ...base, action: "count_tasks" })).toBe(true);
    expect(isDeterministicTaskRead({ ...base, action: "navigate" })).toBe(false);
    expect(
      isDeterministicTaskRead({ ...base, action: "list_tasks", operation_kind: "write" }),
    ).toBe(false);
    // Routing depends on declared typed constraints, never on request vocabulary: workspace
    // status names and customer phrasing vary, so the classifier states what it could not encode.
    expect(
      isDeterministicTaskRead({
        ...base,
        action: "list_tasks",
        unsupported: ["current user"],
      }),
    ).toBe(false);
    expect(
      isDeterministicTaskRead({
        ...base,
        action: "list_tasks",
        assignee_filter: "Dana",
        priority_filter: "urgent",
        due_filter: "this_week",
        unsupported: [],
      }),
    ).toBe(true);
    // Naming display fields is not a constraint: the renderer supplies them as columns, and
    // the planner paraphrases the request by listing exactly those fields.
    expect(
      isDeterministicTaskRead({
        ...base,
        action: "list_tasks",
        intent:
          "list all tasks currently marked in progress, including names, assignees, due dates, and statuses",
      }),
    ).toBe(true);
  });

  it("keeps typed filters independent of workspace status names", () => {
    const filters = taskFiltersFromIntent({
      kind: "intent",
      intent: "list tasks",
      operation_kind: "read",
      action: "list_tasks",
      status_filter: "Blocked / External",
      statuses: ["Blocked / External", "Scoping"],
      assignee_filter: "Dana",
      priority_filter: "high",
      due_filter: "overdue",
      text_filter: "deploy",
      unsupported: null,
      scope_hint: null,
    });
    expect(filters).toEqual({
      statuses: ["Blocked / External", "Scoping"],
      assignee: "Dana",
      priority: "high",
      due: "overdue",
      text: "deploy",
    });
  });

  it("matches custom statuses exactly, then by containment, and reports a miss", () => {
    const tasks = [
      { id: "1", name: "A", status: "in progress", url: "", priority: "urgent" },
      { id: "2", name: "B", status: "In Progress (Dev)", url: "", priority: "high" },
      { id: "3", name: "C", status: "to do", url: "", priority: "low" },
    ];
    const empty = { statuses: [] as string[], assignee: null, priority: null, due: null, text: null };
    expect(
      applyTaskFilters(tasks, { ...empty, statuses: ["in progress"] }).matching.map((t) => t.id),
    ).toEqual(["1"]);
    expect(
      applyTaskFilters(tasks, { ...empty, statuses: ["progress"] }).matching.map((t) => t.id),
    ).toEqual(["1", "2"]);
    expect(
      applyTaskFilters(tasks, { ...empty, statuses: ["in progress", "scoping"] }).matching.map((t) => t.id),
    ).toEqual(["1"]);
    // Statuses are workspace-defined: case, spaces, and punctuation are insignificant.
    expect(
      applyTaskFilters(tasks, { ...empty, statuses: ["todo"] }).matching.map((t) => t.id),
    ).toEqual(["3"]);
    expect(
      applyTaskFilters(tasks, { ...empty, statuses: ["inprogress"] }).matching.map((t) => t.id),
    ).toEqual(["1"]);
    expect(applyTaskFilters(tasks, { ...empty, statuses: ["done"] }).statusMiss).toBe(true);
    expect(
      applyTaskFilters(tasks, { ...empty, priority: "low" }).matching.map((t) => t.id),
    ).toEqual(["3"]);
  });

  it("explains an abstain instead of returning silence", () => {
    const message = describePriorityAbstain(
      {
        outcome: "abstain",
        reason: "The task records do not support a confident recommendation.",
        evidence_sufficient: 0.51,
        considered: 46,
      },
      ["to do"],
    );
    expect(message).toContain("do not support a confident recommendation");
    expect(message).toContain("0.51");
    expect(message).toContain("46 task(s)");
    expect(message).toContain('"to do"');
    // No field may leak through as a literal.
    expect(message).not.toMatch(/undefined|null|NaN/);
  });
});
