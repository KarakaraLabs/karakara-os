// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clickUpCreateSpecFromIntent,
  matchingLists,
  runClickUpCreateWorkflow,
  type ClickUpCreateSpec,
} from "../../../agents/clickup-agent/src/clickupWorkflow.ts";

type Input = Record<string, unknown>;

const platform = { id: "list-1", name: "Agent platform", kind: "list" };
const backend = { id: "list-2", name: "Backend", kind: "list" };

const spec: ClickUpCreateSpec = {
  intent: "Create a task named Test2Clickup",
  name: "Test2Clickup",
  description: null,
  priority: null,
  scope_hint: null,
};

/** Fake the ClickUp hierarchy, recording every input the workflow sends. */
function stubClickUp(lists: unknown[], createFails = false): Input[] {
  const sent: Input[] = [];
  vi.stubGlobal("openToolSession", async () => {
    let last: Input = {};
    return {
      send: async (input: Input) => {
        last = input;
        sent.push(input);
      },
      continue: async () => {
        if ("name" in last) {
          return createFails
            ? { status: "error", error: { code: "tool_execution", message: "List is archived" } }
            : {
                status: "done",
                output: {
                  message: "Created",
                  tasks: [{ id: "task-9", name: last.name, status: "to do", url: "https://clickup.test/task-9" }],
                },
              };
        }
        if ("space_id" in last) return { status: "done", output: { message: "", items: lists } };
        if ("team_id" in last) {
          return { status: "done", output: { message: "", items: [{ id: "space-1", name: "Team", kind: "space" }] } };
        }
        return { status: "done", output: { message: "", items: [{ id: "team-1", name: "Work", kind: "team" }] } };
      },
      finish: async () => undefined,
      abort: async () => undefined,
    };
  });
  return sent;
}

const created = (sent: Input[]) => sent.filter((input) => "name" in input);
const answer = (result: unknown) => JSON.stringify(result);

beforeEach(() => {
  vi.stubGlobal("openA2aExecutionSession", undefined);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("clickup create workflow", () => {
  it("discovers the hierarchy and creates in the only list without asking", async () => {
    const sent = stubClickUp([platform]);
    const awaitReply = vi.fn();

    const result = await runClickUpCreateWorkflow(spec, awaitReply);

    expect(sent).toEqual([
      {},
      { team_id: "team-1" },
      { space_id: "space-1" },
      { list_id: "list-1", name: "Test2Clickup" },
    ]);
    expect(awaitReply).not.toHaveBeenCalled();
    expect(answer(result)).toContain('Created task \\"Test2Clickup\\" (id: task-9)');
    expect(answer(result)).toContain("Agent platform");
  });

  it("uses the list the user named", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn();

    await runClickUpCreateWorkflow({ ...spec, scope_hint: "backend" }, awaitReply);

    expect(awaitReply).not.toHaveBeenCalled();
    expect(created(sent)[0]?.list_id).toBe("list-2");
  });

  it("asks which list when there are several, and uses the answer", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "Backend");

    await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply.mock.calls[0]?.[0]).toContain("Agent platform, Backend");
    expect(created(sent)[0]?.list_id).toBe("list-2");
  });

  it("asks when the named list does not exist", async () => {
    const sent = stubClickUp([platform]);
    const awaitReply = vi.fn(async () => "agent platform");

    await runClickUpCreateWorkflow({ ...spec, scope_hint: "marketing" }, awaitReply);

    expect(awaitReply.mock.calls[0]?.[0]).toContain('matching "marketing"');
    expect(created(sent)[0]?.list_id).toBe("list-1");
  });

  it("creates nothing when the answer names no list", async () => {
    const sent = stubClickUp([platform, backend]);

    const result = await runClickUpCreateWorkflow(spec, vi.fn(async () => "never mind"));

    expect(created(sent)).toEqual([]);
    expect(answer(result)).toContain("was not created");
  });

  it("creates nothing when no lists exist", async () => {
    const sent = stubClickUp([]);

    const result = await runClickUpCreateWorkflow(spec, vi.fn());

    expect(created(sent)).toEqual([]);
    expect(answer(result)).toContain("did not create");
  });

  it("sends the description and numeric priority", async () => {
    const sent = stubClickUp([platform]);

    await runClickUpCreateWorkflow({ ...spec, description: "Ship it", priority: "high" }, vi.fn());

    expect(created(sent)[0]).toEqual({
      list_id: "list-1",
      name: "Test2Clickup",
      description: "Ship it",
      priority: 2,
    });
  });

  it("returns the real API error", async () => {
    stubClickUp([platform], true);

    const result = await runClickUpCreateWorkflow(spec, vi.fn());

    expect(result.error).toContain("List is archived");
  });
});

describe("clickup create spec", () => {
  const intent = {
    kind: "intent",
    intent: "Create a task",
    operation_kind: "write",
    action: "create_task",
    task_name: "  Test2Clickup ",
  } as never;

  it("builds a spec from a single create", () => {
    expect(clickUpCreateSpecFromIntent(intent)?.name).toBe("Test2Clickup");
  });

  it.each([
    ["an update", { action: "other" }],
    ["a missing name", { task_name: null }],
    ["a blank name", { task_name: "  " }],
    ["a field a new task cannot carry", { unsupported: ["assignee Dana"] }],
  ])("leaves %s to the planner", (_label, change) => {
    expect(clickUpCreateSpecFromIntent({ ...(intent as object), ...change } as never)).toBeNull();
  });
});

describe("matching lists", () => {
  const lists = [platform, backend, { id: "list-3", name: "Backend ops", kind: "list" }];

  it("prefers an exact name over a partial one", () => {
    expect(matchingLists(lists, "backend")).toEqual([backend]);
  });

  it("matches by id and by part of a name", () => {
    expect(matchingLists(lists, "list-1")).toEqual([platform]);
    expect(matchingLists(lists, "platform")).toEqual([platform]);
  });

  it("matches nothing for an empty reply", () => {
    expect(matchingLists(lists, "   ")).toEqual([]);
  });
});
