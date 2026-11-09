// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import {
  NO_PAGES_MESSAGE,
  addToolOutput,
  answerInput,
  emptyFindings,
} from "../../../agents/notion-agent/src/notionFindings.ts";

type Run = (ctx: unknown) => Promise<{ message?: unknown; error?: string }>;

const page = { id: "page-1", title: "Prompt to response Flow", url: "https://notion.test/page-1" };
const searchResult = { pages: [page], has_more: false, message: "Found 1 page(s)" };
const pageResult = {
  pages: [page],
  has_more: false,
  blocks: [
    { block_type: "heading_2", text: "Flow" },
    { block_type: "paragraph", text: "The prompt becomes a response." },
    { block_type: "missing_hint", text: "row 2 has empty cells" },
  ],
};
const toolStep = (result: unknown) => ({ status: "done", archive_ref: "@1", result });
const finishedStep = { op: "ReadOnlyFinish", reply: { parts: [], citations: [] } };

let run: Run;
let plan: { goal: string; steps: { id: string; description: string; kind: string }[] };
let runs: unknown[][];
let answer: ReturnType<typeof vi.fn>;
let toolSends: unknown[];
let toolOutput: unknown;

const answerText = { parts: [{ type: "text", text: "A real summary." }], citations: [] };
const planOf = (...kinds: string[]) => ({
  goal: "Summarize the flow page",
  steps: kinds.map((kind) => ({ id: `step-${kind}`, description: kind, kind })),
});
const context = { text: "Summarize the flow page", emit: { awaitInput: vi.fn() } };

beforeAll(async () => {
  vi.stubGlobal("__chat_register", (agent: { run: Run }) => {
    run = agent.run;
  });
  vi.stubGlobal("InferNotionIntent", async () => ({ kind: "intent", intent: "Summarize the flow page" }));
  vi.stubGlobal("PlanNotionWork", async () => plan);
  vi.stubGlobal("runGeneratedStepExecutor", async () => ({
    outcome: "completed",
    last: finishedStep,
    steps: runs.shift() ?? [],
  }));
  vi.stubGlobal("AnswerFromNotion", (args: unknown) => answer(args));
  vi.stubGlobal("openToolSession", async () => ({
    send: async (input: unknown) => {
      toolSends.push(input);
    },
    continue: async () => ({ status: "done", output: toolOutput }),
    finish: async () => undefined,
    abort: async () => undefined,
  }));
  await import("../../../agents/notion-agent/src/index.ts");
});

beforeEach(() => {
  plan = planOf("discover", "read", "synthesize");
  answer = vi.fn(async () => answerText);
  toolSends = [];
  toolOutput = pageResult;
});

describe("notion answers", () => {
  it("answers from fetched page text even when the model never read the archive", async () => {
    runs = [
      [toolStep(searchResult), finishedStep],
      [toolStep(pageResult), finishedStep],
    ];

    const result = await run(context);

    expect(result.message).toEqual(answerText);
    expect(toolSends).toEqual([]);
    expect(answer).toHaveBeenCalledWith({
      goal: "Summarize the flow page",
      user_message: "Summarize the flow page",
      found_pages: [{ title: page.title, url: page.url }],
      page_contents: [
        { title: page.title, url: page.url, text: "Flow\nThe prompt becomes a response." },
      ],
      content_truncated: false,
    });
  });

  it("reads the top page itself when the model skips a planned read", async () => {
    runs = [[toolStep(searchResult), finishedStep], [finishedStep]];

    await run(context);

    expect(toolSends).toEqual([{ block_id: "page-1" }]);
    expect(answer.mock.calls[0][0].page_contents).toHaveLength(1);
  });

  it("does not read pages for a title-only plan", async () => {
    plan = planOf("discover", "synthesize");
    runs = [[toolStep(searchResult), finishedStep]];

    await run(context);

    expect(toolSends).toEqual([]);
    expect(answer.mock.calls[0][0].page_contents).toEqual([]);
  });

  it("explains an empty search without a model call", async () => {
    runs = [[toolStep({ has_more: false, message: "Found 0 page(s)" }), finishedStep], [finishedStep]];

    const result = await run(context);

    expect(answer).not.toHaveBeenCalled();
    expect(toolSends).toEqual([]);
    expect(result.message).toEqual({
      parts: [{ type: "text", text: NO_PAGES_MESSAGE }],
      citations: [],
    });
  });

  it("reports a failed answer as an error, not as empty content", async () => {
    runs = [[toolStep(pageResult)]];
    answer = vi.fn(async () => {
      throw new Error("model unavailable");
    });

    const result = await run(context);

    expect(result.error).toContain("model unavailable");
    expect(result.message).toBeUndefined();
  });
});

describe("notion findings", () => {
  it("marks content cut at the size limit", () => {
    const findings = emptyFindings();
    addToolOutput(findings, { pages: [page], blocks: [{ text: "0123456789" }] });

    const input = answerInput(findings, 4);

    expect(input.page_contents[0].text).toBe("0123");
    expect(input.content_truncated).toBe(true);
  });

  it("marks a page the tool could not finish", () => {
    const findings = emptyFindings();
    addToolOutput(findings, { pages: [page], blocks: [{ text: "start" }], has_more: true });

    expect(answerInput(findings).content_truncated).toBe(true);
  });

  it("does not treat more search results as an unfinished page", () => {
    const findings = emptyFindings();
    addToolOutput(findings, { pages: [page], has_more: true });

    expect(answerInput(findings).content_truncated).toBe(false);
  });

  it("does not repeat a page fetched twice", () => {
    const findings = emptyFindings();
    addToolOutput(findings, pageResult);
    addToolOutput(findings, pageResult);

    expect(findings.contents).toHaveLength(1);
    expect(findings.contents[0].text).toBe("Flow\nThe prompt becomes a response.");
  });
});
