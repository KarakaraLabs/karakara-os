// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clickUpDeleteSpecFromDeclaredRoute,
  collectStepEvidence,
  declaredRouteFromMessage,
  isDeleteConfirmation,
  runClickUpDeleteWorkflow,
  type ClickUpDeleteSpec,
  type DeclaredRoute,
} from "../../../agents/clickup-agent/src/clickupWorkflow.ts";
import {
  delegatedPromptFromOutput,
  parseRouteOperation,
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

    const result = await runClickUpDeleteWorkflow(
      { ...byId, task_id: null },
      awaitReply,
    );

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
      error:
        'ClickUp agent error: ClickUp API returned 400: {"err":"Task not found"}',
    });
  });

  it("accepts confirmation wording regardless of case and punctuation", () => {
    expect(isDeleteConfirmation("Yes!")).toBe(true);
    expect(isDeleteConfirmation("  go ahead. ")).toBe(true);
    expect(isDeleteConfirmation("yesterday")).toBe(false);
  });
});

describe("declared delete routes", () => {
  function declared(operation: unknown, capability = "clickup:delete-task"): DeclaredRoute {
    const route = declaredRouteFromMessage({
      parts: [
        {
          data: {
            schema: "agent-route/v1",
            capability,
            goal: "Delete Test1",
            filters: { statuses: ["in progress"], text: "Test1" },
            operation,
            unsupported: [],
          },
        },
      ],
    });
    if (!route) throw new Error("route did not parse");
    return route;
  }

  it("keeps the id and name but drops statuses inherited from the conversation", () => {
    const spec = clickUpDeleteSpecFromDeclaredRoute(
      declared({ name: "delete_task", task_id: test1.id }),
    );
    expect(spec?.task_id).toBe(test1.id);
    expect(spec?.filters.text).toBe("Test1");
    expect(spec?.filters.statuses).toEqual([]);
  });

  it("ignores other capabilities", () => {
    expect(
      clickUpDeleteSpecFromDeclaredRoute(declared(null, "clickup:update-task")),
    ).toBeNull();
  });

  it("parses delete_task on the coordinator side without a destination value", () => {
    expect(parseRouteOperation({ name: "delete_task", task_id: "abc" })).toEqual({
      name: "delete_task",
      task_id: "abc",
      target_status: null,
      target_priority: null,
    });
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
    expect(delegatedPromptFromOutput({ chunks: [{ statusUpdate }] })).toBe(
      "Delete Test1?",
    );
  });

  it("still reads a plain message chunk", () => {
    expect(
      delegatedPromptFromOutput({
        chunks: [{ message: { parts: [{ text: "Which list?" }] } }],
      }),
    ).toBe("Which list?");
  });

  it("returns null when no chunk carries a question", () => {
    expect(
      delegatedPromptFromOutput({ chunks: [{ statusUpdate: "not json" }] }),
    ).toBeNull();
  });
});
