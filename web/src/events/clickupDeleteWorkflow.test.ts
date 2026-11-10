// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clickUpDeleteSpecFromIntent,
  clickUpMultiDeleteSpecFromIntent,
  collectStepEvidence,
  isDeleteConfirmation,
  runClickUpDeleteWorkflow,
  runClickUpMultiDeleteWorkflow,
  type ClickUpDeleteSpec,
} from "../../../agents/clickup-agent/src/clickupWorkflow.ts";
import {
  delegatedPromptFromOutput,
  planStepMessage,
  replyText,
} from "../../../agents/coordinator-agent/src/routing.ts";

const test1 = { id: "86afcwxrj", name: "Test1", status: "in progress" };

const noFilters = {
  statuses: [],
  assignee: null,
  priority: null,
  due: null,
  text: null,
};

const byId: ClickUpDeleteSpec = {
  intent: "Delete Test1",
  task_id: test1.id,
  filters: noFilters,
  scope_hint: null,
};

type Input = Record<string, unknown>;
type ToolStep =
  | { status: "done"; output: unknown }
  | { status: "error"; error: { code: string; message: string } };

/** Fake the ClickUp tool session, recording every input the workflow sends. */
function stubClickUp(respond: (input: Input) => ToolStep): Input[] {
  const sent: Input[] = [];
  vi.stubGlobal("openToolSession", async () => {
    let last: Input = {};
    return {
      send: async (input: Input) => {
        last = input;
        sent.push(input);
      },
      continue: async () => respond(last),
      finish: async () => undefined,
      abort: async () => undefined,
    };
  });
  return sent;
}

function foundTask(input: Input): ToolStep {
  return input.confirm_delete === true
    ? { status: "done", output: { message: "Deleted", tasks: [], items: [] } }
    : { status: "done", output: { message: "", tasks: [test1], items: [] } };
}

function text(result: unknown): string {
  return JSON.stringify(result);
}

beforeEach(() => {
  vi.stubGlobal("openA2aExecutionSession", undefined);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ClickUp delete workflow", () => {
  it("deletes only after an explicit yes and names the task in the question", async () => {
    const sent = stubClickUp(foundTask);
    const awaitReply = vi.fn(async () => "yes");

    const result = await runClickUpDeleteWorkflow(byId, awaitReply);

    expect(awaitReply.mock.calls[0]).toEqual([expect.stringContaining('"Test1"')]);
    expect(sent).toContainEqual({ task_id: test1.id, confirm_delete: true });
    expect(text(result)).toContain("Deleted task 86afcwxrj (Test1)");
  });

  it.each(["no", "wait, which one?", "yes but later", ""])(
    "does not delete on the reply %j",
    async (reply) => {
      const sent = stubClickUp(foundTask);

      const result = await runClickUpDeleteWorkflow(byId, async () => reply);

      expect(sent.some((input) => "confirm_delete" in input)).toBe(false);
      expect(text(result)).toContain("was not deleted");
    },
  );

  it("does not ask or delete when no task is named", async () => {
    const sent = stubClickUp(foundTask);
    const awaitReply = vi.fn(async () => "yes");

    const result = await runClickUpDeleteWorkflow({ ...byId, task_id: null }, awaitReply);

    expect(awaitReply).not.toHaveBeenCalled();
    expect(sent).toEqual([]);
    expect(text(result)).toContain("did not delete anything");
  });

  it("reports the ClickUp API error text when the delete fails", async () => {
    stubClickUp((input) =>
      input.confirm_delete === true
        ? {
            status: "error",
            error: {
              code: "tool_execution",
              message: 'ClickUp API returned 400: {"err":"Task not found"}',
            },
          }
        : foundTask(input),
    );

    const result = await runClickUpDeleteWorkflow(byId, async () => "yes");

    expect(result).toEqual({
      error: 'ClickUp agent error: ClickUp API returned 400: {"err":"Task not found"}',
    });
  });

  it("accepts confirmation wording regardless of case and punctuation", () => {
    expect(isDeleteConfirmation("Yes!")).toBe(true);
    expect(isDeleteConfirmation("  go ahead. ")).toBe(true);
    expect(isDeleteConfirmation("yesterday")).toBe(false);
  });
});

/** Fake a full teams → spaces → lists → tasks hierarchy, with named tasks in the one list. */
function stubHierarchy(tasks: { id: string; name: string; status: string }[]): Input[] {
  return stubClickUp((input) => {
    if (input.confirm_delete === true) return { status: "done", output: { message: "Deleted", tasks: [], items: [] } };
    if ("list_id" in input) return { status: "done", output: { message: "", tasks, items: [] } };
    if ("space_id" in input) return { status: "done", output: { message: "", items: [{ id: "list-1", name: "L", kind: "list" }] } };
    if ("team_id" in input) return { status: "done", output: { message: "", items: [{ id: "space-1", name: "S", kind: "space" }] } };
    return { status: "done", output: { message: "", items: [{ id: "team-1", name: "T", kind: "team" }] } };
  });
}

const alpha = { id: "t-alpha", name: "Alpha Export", status: "to do" };
const beta = { id: "t-beta", name: "Beta Flow", status: "to do" };

describe("ClickUp multi-delete workflow", () => {
  it("deletes a named task with no confirmation step", async () => {
    const sent = stubHierarchy([alpha, beta]);

    const result = await runClickUpMultiDeleteWorkflow({
      intent: "Delete tasks",
      task_ids: [],
      names: ["Alpha Export", "Beta Flow"],
      scope_hint: null,
    });

    expect(sent).toContainEqual({ task_id: "t-alpha", confirm_delete: true });
    expect(sent).toContainEqual({ task_id: "t-beta", confirm_delete: true });
    expect(text(result)).toContain("Deleted 2");
  });

  it("deletes by id without confirmation", async () => {
    const sent = stubClickUp(foundTask);

    const result = await runClickUpMultiDeleteWorkflow({
      intent: "Delete Test1",
      task_ids: [test1.id],
      names: [],
      scope_hint: null,
    });

    expect(sent).toContainEqual({ task_id: test1.id, confirm_delete: true });
    expect(text(result)).toContain("Deleted 1");
  });

  it("reports names that match zero or many tasks instead of guessing", async () => {
    const dup1 = { id: "t-dup1", name: "Dup", status: "to do" };
    const dup2 = { id: "t-dup2", name: "Dup", status: "to do" };
    const sent = stubHierarchy([alpha, dup1, dup2]);

    const result = await runClickUpMultiDeleteWorkflow({
      intent: "Delete tasks",
      task_ids: [],
      names: ["Alpha Export", "Ghost", "Dup"],
      scope_hint: null,
    });

    // Only the unambiguous match is deleted; the missing and the duplicate are left alone.
    expect(sent.filter((input) => input.confirm_delete === true)).toEqual([
      { task_id: "t-alpha", confirm_delete: true },
    ]);
    expect(text(result)).toContain("Not found: Ghost");
    expect(text(result)).toContain("Dup");
  });
});

describe("classified multi-delete intent", () => {
  const base = {
    kind: "intent",
    intent: "Delete tasks",
    operation_kind: "delete",
    action: "delete_tasks",
    task_names: ["Alpha", "Beta", "alpha"],
  } as never;

  it("collects and de-duplicates the names", () => {
    expect(clickUpMultiDeleteSpecFromIntent(base)?.names).toEqual(["Alpha", "Beta"]);
  });

  it.each([
    ["a non delete_tasks action", { action: "other" }],
    ["no names or id", { task_names: [] }],
  ])("returns null for %s", (_label, change) => {
    expect(clickUpMultiDeleteSpecFromIntent({ ...(base as object), ...change } as never)).toBeNull();
  });
});

describe("classified delete intent", () => {
  it("uses an exact id classified by the ClickUp specialist", () => {
    const spec = clickUpDeleteSpecFromIntent({
      kind: "intent",
      intent: "Delete task 86afcwxrj by id",
      operation_kind: "delete",
      action: "other",
      task_id: "86afcwxrj",
      text_filter: null,
      status_filter: null,
      statuses: null,
      scope_hint: null,
    } as never);
    expect(spec.task_id).toBe("86afcwxrj");
  });
});

describe("delete evidence", () => {
  const step = (action: string) => ({
    status: "done",
    receipt: { action_identity: action },
  });

  it("does not count a dry run as a write", () => {
    const evidence = collectStepEvidence([
      step('Send(delete_task task_id="abc" confirm_delete=false)'),
    ]);
    expect(evidence.wrote).toBe(false);
  });

  it("counts a confirmed delete as a write", () => {
    const evidence = collectStepEvidence([
      step('Send(delete_task task_id="abc" confirm_delete=true)'),
    ]);
    expect(evidence.wrote).toBe(true);
  });
});

describe("delegated prompts", () => {
  it("reads the awaitInput question from the status update string", () => {
    const statusUpdate = JSON.stringify({
      status: {
        state: "TASK_STATE_INPUT_REQUIRED",
        message: { parts: [{ text: "Delete Test1?" }] },
      },
    });
    expect(delegatedPromptFromOutput({ chunks: [{ statusUpdate }] })).toBe("Delete Test1?");
  });

  it("still reads a plain message chunk", () => {
    expect(
      delegatedPromptFromOutput({
        chunks: [{ message: { parts: [{ text: "Which list?" }] } }],
      }),
    ).toBe("Which list?");
  });

  it("returns null when no chunk carries a question", () => {
    expect(delegatedPromptFromOutput({ chunks: [{ statusUpdate: "not json" }] })).toBeNull();
  });
});

describe("coordinator plan steps", () => {
  const earlier = [
    { agent_package: "notion-agent", goal: "Find the doc", answer: "Found page: Roadmap" },
  ];

  it("sends the first step (no earlier answers) exactly as planned", () => {
    expect(planStepMessage("Find the doc", [])).toBe("Find the doc");
  });

  it("carries earlier answers into every downstream step", () => {
    const message = planStepMessage("Create a task for each page", earlier);
    expect(message.startsWith("Create a task for each page")).toBe(true);
    expect(message).toContain("[notion-agent]");
    expect(message).toContain("Found page: Roadmap");
  });

  it("flattens a reply's text and data parts in order", () => {
    expect(
      replyText({
        parts: [
          { type: "text", text: "Deleted." },
          { type: "data", data: '{"id":"abc"}', media_type: "application/json" },
        ],
        citations: [],
      }),
    ).toBe('Deleted.\n{"id":"abc"}');
  });
});
