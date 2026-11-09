// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

type Step = { status: string; output?: unknown; error?: { message: string } };
type Run = (ctx: unknown) => Promise<{ message?: unknown; error?: string }>;

const textOutput = (text: string) => ({ chunks: [{ message: { parts: [{ text }] } }] });
const agents = ["notion-agent", "clickup-agent"].map((agentPackage) => ({
  agentPackage,
  name: agentPackage,
  capabilities: [],
  tools: [],
}));

/** Scripted specialist replies, in the order the coordinator reads them. */
let scripts: Record<string, Step[]>;
let sent: { agent: string; text: string }[];
let plan: { agent_package: string; sub_message: string }[];
let routeShape: string;
let synthesize: ReturnType<typeof vi.fn>;
let run: Run;

function openToolSession(tool: string, options?: { target?: { agent_package: string } }) {
  if (tool === "system/discover_agents") {
    return Promise.resolve({
      send: async () => undefined,
      continue: async () => ({ status: "done", output: { agents } }),
      finish: async () => undefined,
      abort: async () => undefined,
    });
  }
  const agent = options?.target?.agent_package ?? "";
  return Promise.resolve({
    send: async (input: { parts: { text: string }[] }) => {
      sent.push({ agent, text: input.parts[0].text });
    },
    continue: async () => {
      const step = scripts[agent].shift();
      if (!step) throw new Error(`${agent} was read more often than scripted`);
      return step;
    },
    finish: async () => undefined,
    abort: async () => undefined,
  });
}

beforeAll(async () => {
  vi.stubGlobal("__chat_register", (agent: { run: Run }) => {
    run = agent.run;
  });
  vi.stubGlobal("openToolSession", openToolSession);
  vi.stubGlobal("messageText", (message: { text: string }) => message.text);
  vi.stubGlobal("ClassifyCoordinatorTurn", async () => ({
    kind: "ready",
    inferred_intent: "Summarize a page and delete a task",
    route: {
      shape: routeShape,
      goal: "Summarize a page and delete a task",
      targets: [
        { agent_package: "notion-agent", capability: "notion:search-pages" },
        { agent_package: "clickup-agent", capability: "clickup:delete-task" },
      ],
    },
  }));
  vi.stubGlobal("MakeStructuredPlan", async () => ({
    intent_description: "Summarize and delete",
    objective: "Summarize and delete",
    plan_steps: plan,
    citations: null,
  }));
  vi.stubGlobal("CoordinatorSynthesizeReply", (args: unknown) => synthesize(args));
  await import("../../../agents/coordinator-agent/src/index.ts");
});

beforeEach(() => {
  sent = [];
  routeShape = "sequence";
  plan = [
    { agent_package: "notion-agent", sub_message: "Summarize the page" },
    { agent_package: "clickup-agent", sub_message: "Delete Step2Test" },
  ];
  synthesize = vi.fn(async () => ({ parts: [{ type: "text", text: "All done." }], citations: [] }));
});

function context(replies: string[] = []) {
  const awaitInput = vi.fn(async () => ({ text: replies.shift() ?? "" }));
  return { awaitInput, ctx: { text: "do both", emit: { message: vi.fn(), awaitInput } } };
}

describe("coordinator multi-agent plans", () => {
  it("sends each goal once and synthesizes from the final answers", async () => {
    scripts = {
      "notion-agent": [{ status: "done", output: textOutput("Summary of the page") }],
      "clickup-agent": [{ status: "done", output: textOutput("Deleted Step2Test") }],
    };
    const { ctx, awaitInput } = context();

    const result = await run(ctx);

    expect(sent).toEqual([
      { agent: "notion-agent", text: "Summarize the page" },
      { agent: "clickup-agent", text: "Delete Step2Test" },
    ]);
    expect(awaitInput).not.toHaveBeenCalled();
    expect(synthesize).toHaveBeenCalledWith({
      user_message: "do both",
      plan_objective: "Summarize and delete",
      specialist_answers: [
        { agent_package: "notion-agent", goal: "Summarize the page", answer: "Summary of the page" },
        { agent_package: "clickup-agent", goal: "Delete Step2Test", answer: "Deleted Step2Test" },
      ],
    });
    expect(result.message).toEqual({ parts: [{ type: "text", text: "All done." }], citations: [] });
  });

  it("relays a specialist's question and sends the reply to that specialist", async () => {
    scripts = {
      "notion-agent": [{ status: "done", output: textOutput("Summary of the page") }],
      "clickup-agent": [
        { status: "suspended", output: textOutput('Permanently delete "Step2Test"?') },
        { status: "done", output: textOutput("Deleted Step2Test") },
      ],
    };
    const { ctx, awaitInput } = context(["yes"]);

    await run(ctx);

    expect(awaitInput).toHaveBeenCalledExactlyOnceWith('Permanently delete "Step2Test"?');
    expect(sent.at(-1)).toEqual({ agent: "clickup-agent", text: "yes" });
    expect(sent.filter((call) => call.agent === "notion-agent")).toHaveLength(1);
    expect(synthesize).toHaveBeenCalledOnce();
  });

  it("stops the plan when a specialist fails", async () => {
    scripts = {
      "notion-agent": [{ status: "error", error: { message: "Notion is down" } }],
      "clickup-agent": [],
    };
    const { ctx } = context();

    const result = await run(ctx);

    expect(result.error).toBe("Notion is down");
    expect(sent.map((call) => call.agent)).toEqual(["notion-agent"]);
    expect(synthesize).not.toHaveBeenCalled();
  });

  it("gives a foreach step the earlier answers", async () => {
    routeShape = "foreach";
    scripts = {
      "notion-agent": [{ status: "done", output: textOutput("Found page: Roadmap") }],
      "clickup-agent": [{ status: "done", output: textOutput("Created 1 task") }],
    };
    const { ctx } = context();

    await run(ctx);

    expect(sent[1].text).toContain("Delete Step2Test");
    expect(sent[1].text).toContain("Found page: Roadmap");
  });
});
