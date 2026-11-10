// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ClickUpIntent } from "../../../agents/clickup-agent/src/baml-runtime";
import {
  clickUpUpdateSpecFromIntent,
  runClickUpWriteWorkflow,
  type ClickUpUpdateSpec,
} from "../../../agents/clickup-agent/src/clickupWorkflow.ts";

const noFilters = {
  statuses: [] as string[],
  assignee: null,
  priority: null,
  due: null,
  text: null,
};

function updateIntent(overrides: Partial<ClickUpIntent> = {}): ClickUpIntent {
  return {
    kind: "intent",
    intent: "Mark Fix login done",
    operation_kind: "write",
    action: "update_task",
    status_filter: null,
    statuses: null,
    assignee_filter: null,
    priority_filter: null,
    due_filter: null,
    text_filter: "Fix login",
    unsupported: null,
    scope_hint: null,
    task_id: null,
    task_name: null,
    task_description: null,
    task_priority: null,
    target_status: "done",
    target_priority: null,
    ...overrides,
  };
}

function task(id: string, name: string, status = "in progress") {
  return { id, name, status, url: `https://clickup.test/${id}` };
}

function stubClickUp(
  tasks: ReturnType<typeof task>[],
  failedUpdates = new Map<string, string>(),
): Record<string, unknown>[] {
  const sent: Record<string, unknown>[] = [];
  vi.stubGlobal("openToolSession", async () => {
    let last: Record<string, unknown> = {};
    return {
      send: async (input: Record<string, unknown>) => {
        last = input;
        sent.push(input);
      },
      continue: async () => {
        if (typeof last.task_id === "string" && ("status" in last || "priority" in last)) {
          const failure = failedUpdates.get(last.task_id);
          if (failure) {
            return { status: "error", error: { code: "tool_execution", message: failure } };
          }
          const found = tasks.find((candidate) => candidate.id === last.task_id)!;
          return {
            status: "done",
            output: {
              tasks: [
                {
                  ...found,
                  status: typeof last.status === "string" ? last.status : found.status,
                },
              ],
              items: [],
            },
          };
        }
        if ("list_id" in last) return { status: "done", output: { tasks, items: [] } };
        if ("space_id" in last) {
          return {
            status: "done",
            output: { items: [{ id: "list-1", name: "Backend", kind: "list" }] },
          };
        }
        if ("team_id" in last) {
          return {
            status: "done",
            output: { items: [{ id: "space-1", name: "Engineering", kind: "space" }] },
          };
        }
        return {
          status: "done",
          output: { items: [{ id: "team-1", name: "Work", kind: "team" }] },
        };
      },
      finish: async () => undefined,
      abort: async () => undefined,
    };
  });
  return sent;
}

function updates(sent: Record<string, unknown>[]): Record<string, unknown>[] {
  return sent.filter((input) => "task_id" in input && ("status" in input || "priority" in input));
}

beforeEach(() => {
  vi.stubGlobal("openA2aExecutionSession", undefined);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ClickUp update intent", () => {
  it("does not run an update with no task selector", async () => {
    const openToolSession = vi.fn();
    vi.stubGlobal("openToolSession", openToolSession);
    const spec = clickUpUpdateSpecFromIntent(
      updateIntent({ text_filter: null, status_filter: null, statuses: null }),
    );
    if (!spec) throw new Error("update intent did not produce a spec");

    const result = await runClickUpWriteWorkflow(spec);

    expect(openToolSession).not.toHaveBeenCalled();
    expect(JSON.stringify(result)).toContain("which ClickUp task or task set");
  });

  it("drops stale statuses when a task name selects the target", () => {
    const spec = clickUpUpdateSpecFromIntent(
      updateIntent({ statuses: ["in progress"], text_filter: "Fix login" }),
    );

    expect(spec?.filters.statuses).toEqual([]);
    expect(spec?.filters.text).toBe("Fix login");
  });

  it("keeps a current-status selector for an explicit bulk update", () => {
    const spec = clickUpUpdateSpecFromIntent(
      updateIntent({
        intent: "Move every in-progress task to done",
        text_filter: null,
        status_filter: "in progress",
        statuses: null,
      }),
    );

    expect(spec?.filters.statuses).toEqual(["in progress"]);
  });
});

describe("ClickUp update safety", () => {
  it("lists several partial name matches and changes nothing", async () => {
    const sent = stubClickUp([
      task("task-1", "Fix login timeout"),
      task("task-2", "Investigate login redirect"),
    ]);
    const spec = clickUpUpdateSpecFromIntent(updateIntent({ text_filter: "login" }))!;

    const result = await runClickUpWriteWorkflow(spec);
    const text = JSON.stringify(result);

    expect(updates(sent)).toEqual([]);
    expect(text).toContain("task-1");
    expect(text).toContain("task-2");
    expect(text).toContain("exact task name or id");
  });

  it("asks instead of updating a single partial name match", async () => {
    const sent = stubClickUp([task("task-1", "Fix login timeout")]);
    const spec = clickUpUpdateSpecFromIntent(updateIntent())!;

    const result = await runClickUpWriteWorkflow(spec);

    expect(updates(sent)).toEqual([]);
    expect(JSON.stringify(result)).toContain("exact task name or id");
  });

  it("updates every exact duplicate and ignores partial matches", async () => {
    const sent = stubClickUp([
      task("task-1", "Fix login"),
      task("task-2", "Fix login"),
      task("task-3", "Fix login timeout"),
    ]);
    const spec = clickUpUpdateSpecFromIntent(updateIntent())!;

    await runClickUpWriteWorkflow(spec);

    expect(updates(sent)).toEqual([
      { task_id: "task-1", status: "done" },
      { task_id: "task-2", status: "done" },
    ]);
  });

  it("keeps status-only bulk updates working", async () => {
    const sent = stubClickUp([
      task("task-1", "First"),
      task("task-2", "Second"),
      task("task-3", "Already done", "done"),
    ]);
    const spec = clickUpUpdateSpecFromIntent(
      updateIntent({ text_filter: null, status_filter: "in progress" }),
    )!;

    await runClickUpWriteWorkflow(spec);

    expect(updates(sent)).toEqual([
      { task_id: "task-1", status: "done" },
      { task_id: "task-2", status: "done" },
    ]);
  });

  it("refuses a bulk update above the safety limit", async () => {
    const sent = stubClickUp(
      Array.from({ length: 11 }, (_, index) => task(`task-${index}`, `Task ${index}`)),
    );
    const spec = clickUpUpdateSpecFromIntent(
      updateIntent({ text_filter: null, status_filter: "in progress" }),
    )!;

    const result = await runClickUpWriteWorkflow(spec);

    expect(updates(sent)).toEqual([]);
    expect(JSON.stringify(result)).toContain("too many to update at once");
  });

  it("reports a failed task without hiding successful updates", async () => {
    const tasks = [task("task-1", "First"), task("task-2", "Second")];
    const sent = stubClickUp(tasks, new Map([["task-2", "Task is archived"]]));
    const spec: ClickUpUpdateSpec = {
      intent: "Mark in-progress tasks done",
      task_id: null,
      target_status: "done",
      target_priority: null,
      filters: { ...noFilters, statuses: ["in progress"] },
      scope_hint: null,
    };

    const result = await runClickUpWriteWorkflow(spec);
    const text = JSON.stringify(result);

    expect(updates(sent)).toEqual([
      { task_id: "task-1", status: "done" },
      { task_id: "task-2", status: "done" },
    ]);
    expect(text).toContain("1 changed, 1 failed");
    expect(text).toContain("Task is archived");
  });
});
