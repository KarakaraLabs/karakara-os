// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clickUpCreateSpecFromIntent,
  clickUpMultiCreateSpecFromIntent,
  exactMatchingLists,
  listChosenInReply,
  matchingLists,
  runClickUpCreateWorkflow,
  runClickUpMultiCreateWorkflow,
  type ClickUpCreateSpec,
  type ClickUpMultiCreateSpec,
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
                  tasks: [
                    {
                      id: "task-9",
                      name: last.name,
                      status: "to do",
                      url: "https://clickup.test/task-9",
                    },
                  ],
                },
              };
        }
        if ("space_id" in last) return { status: "done", output: { message: "", items: lists } };
        if ("team_id" in last) {
          return {
            status: "done",
            output: { message: "", items: [{ id: "space-1", name: "Team", kind: "space" }] },
          };
        }
        return {
          status: "done",
          output: { message: "", items: [{ id: "team-1", name: "Work", kind: "team" }] },
        };
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

  it("applies a named-list hint before the workspace list limit", async () => {
    const otherLists = Array.from({ length: 49 }, (_, index) => ({
      id: `other-${index}`,
      name: `Other ${index}`,
      kind: "list",
    }));
    const sent = stubClickUp([...otherLists, backend]);

    const result = await runClickUpCreateWorkflow(
      { ...spec, scope_hint: "Backend" },
      vi.fn(),
    );

    expect(created(sent)[0]?.list_id).toBe("list-2");
    expect(result.error).toBeUndefined();
  });

  it("asks which list when there are several, and uses the answer", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "Backend");

    await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply.mock.calls[0]?.[0]).toContain("Agent platform (id: list-1)");
    expect(awaitReply.mock.calls[0]?.[0]).toContain("Backend (id: list-2)");
    expect(created(sent)[0]?.list_id).toBe("list-2");
  });

  it("asks when the named list does not exist", async () => {
    const sent = stubClickUp([platform]);
    const awaitReply = vi.fn(async () => "agent platform");

    await runClickUpCreateWorkflow({ ...spec, scope_hint: "marketing" }, awaitReply);

    expect(awaitReply.mock.calls[0]?.[0]).toContain('matching "marketing"');
    expect(created(sent)[0]?.list_id).toBe("list-1");
  });

  it.each(["never mind", "no"])(
    "creates nothing when the answer %j names no list",
    async (reply) => {
      const notes = { id: "list-notes", name: "Notes", kind: "list" };
      const sent = stubClickUp([notes, backend]);

      const result = await runClickUpCreateWorkflow(
        spec,
        vi.fn(async () => reply),
      );

      expect(created(sent)).toEqual([]);
      expect(answer(result)).toContain("was not created");
    },
  );

  it("accepts a wordy answer that names one list unambiguously", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "put it in Backend please");

    await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply).toHaveBeenCalledOnce();
    expect(created(sent)[0]?.list_id).toBe("list-2");
  });

  it("asks again once when the first answer is unclear, then uses the second", async () => {
    const sent = stubClickUp([platform, backend]);
    const replies = ["um not sure", "Backend"];
    const awaitReply = vi.fn(async () => replies.shift() ?? "");

    await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply).toHaveBeenCalledTimes(2);
    expect(awaitReply.mock.calls[1]?.[0]).toContain("couldn't tell which list");
    expect(created(sent)[0]?.list_id).toBe("list-2");
  });

  it("cancels after a second unclear answer", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "neither of those");

    const result = await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply).toHaveBeenCalledTimes(2);
    expect(created(sent)).toEqual([]);
    expect(answer(result)).toContain("was not created");
  });

  it("shows ids for duplicate names and accepts an id", async () => {
    const duplicate = { id: "list-3", name: "Backend", kind: "list" };
    const sent = stubClickUp([backend, duplicate]);
    const awaitReply = vi.fn(async () => "list-3");

    await runClickUpCreateWorkflow(spec, awaitReply);

    expect(awaitReply.mock.calls[0]?.[0]).toContain("Backend (id: list-2)");
    expect(awaitReply.mock.calls[0]?.[0]).toContain("Backend (id: list-3)");
    expect(created(sent)[0]?.list_id).toBe("list-3");
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

const multiSpec: ClickUpMultiCreateSpec = {
  intent: "Create tasks from titles",
  names: ["Alpha Export", "Beta Flow"],
  description: null,
  priority: null,
  scope_hint: null,
};

describe("clickup multi-create workflow", () => {
  it("creates every named task in the only list without asking", async () => {
    const sent = stubClickUp([platform]);
    const awaitReply = vi.fn();

    const result = await runClickUpMultiCreateWorkflow(multiSpec, awaitReply);

    expect(awaitReply).not.toHaveBeenCalled();
    expect(created(sent).map((input) => input.name)).toEqual(["Alpha Export", "Beta Flow"]);
    expect(created(sent).every((input) => input.list_id === "list-1")).toBe(true);
    expect(answer(result)).toContain("Created 2 of 2");
  });

  it("asks for the list once, then creates all tasks there", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "Backend");

    await runClickUpMultiCreateWorkflow(multiSpec, awaitReply);

    expect(awaitReply).toHaveBeenCalledOnce();
    expect(created(sent).map((input) => input.list_id)).toEqual(["list-2", "list-2"]);
  });

  it("creates nothing when the list answer stays unclear", async () => {
    const sent = stubClickUp([platform, backend]);
    const awaitReply = vi.fn(async () => "neither");

    const result = await runClickUpMultiCreateWorkflow(multiSpec, awaitReply);

    expect(awaitReply).toHaveBeenCalledTimes(2);
    expect(created(sent)).toEqual([]);
    expect(answer(result)).toContain("no tasks were created");
  });

  it("reports failures without dropping the batch", async () => {
    const sent = stubClickUp([platform], true);

    const result = await runClickUpMultiCreateWorkflow(multiSpec, vi.fn());

    // Both creates were attempted even though the API rejected them.
    expect(sent.filter((input) => "name" in input)).toHaveLength(2);
    expect(answer(result)).toContain("Created 0 of 2");
    expect(answer(result)).toContain("Failed to create 2");
  });
});

describe("clickup multi-create spec", () => {
  const base = {
    kind: "intent",
    intent: "Create tasks",
    operation_kind: "write",
    action: "create_tasks",
    task_names: ["Alpha", "Beta", "alpha", "  "],
  } as never;

  it("collects, trims and de-duplicates the names", () => {
    expect(clickUpMultiCreateSpecFromIntent(base)?.names).toEqual(["Alpha", "Beta"]);
  });

  it.each([
    ["a single create", { action: "create_task" }],
    ["no names", { task_names: [] }],
    ["a field a new task cannot carry", { unsupported: ["assignee Dana"] }],
  ])("leaves %s to another path", (_label, change) => {
    expect(clickUpMultiCreateSpecFromIntent({ ...(base as object), ...change } as never)).toBeNull();
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

  it("requires an exact name or id for a prompted choice", () => {
    expect(exactMatchingLists(lists, "platform")).toEqual([]);
    expect(exactMatchingLists(lists, "Agent platform")).toEqual([platform]);
    expect(exactMatchingLists(lists, "list-2")).toEqual([backend]);
  });

  describe("listChosenInReply", () => {
    it("takes an exact name or id outright", () => {
      expect(listChosenInReply(lists, "Agent platform")).toEqual([platform]);
      expect(listChosenInReply(lists, "list-2")).toEqual([backend]);
    });

    it("resolves a wordy reply that mentions one option by name or id", () => {
      expect(listChosenInReply(lists, "put it in Agent platform please")).toEqual([platform]);
      expect(listChosenInReply(lists, "use list-1 thanks")).toEqual([platform]);
    });

    it("stays ambiguous when the reply names two unrelated options", () => {
      expect(listChosenInReply(lists, "either Agent platform or Backend")).toHaveLength(2);
    });

    it("prefers the most specific when names nest", () => {
      // "Backend" is contained in "Backend ops"; a reply naming the longer one resolves to it.
      expect(listChosenInReply(lists, "the Backend ops list")).toEqual([
        { id: "list-3", name: "Backend ops", kind: "list" },
      ]);
    });

    it("returns nothing when the reply names no option", () => {
      expect(listChosenInReply(lists, "none of these")).toEqual([]);
      expect(listChosenInReply(lists, "   ")).toEqual([]);
    });
  });
});
