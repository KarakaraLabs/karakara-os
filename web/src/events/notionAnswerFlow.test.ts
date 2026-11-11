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
type Retrieval = {
  kind: "retrieve";
  goal: string;
  search_queries: string[];
  page_urls: string[];
  read_mode: "search_results" | "metadata" | "content";
  selection: "best_matches" | "all_matches";
  max_pages: number;
};

const PAGE_ID = "11111111-1111-1111-1111-111111111111";
const SECOND_PAGE_ID = "22222222-2222-2222-2222-222222222222";
const VIEW_ID = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FILE_HASH = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const page = {
  id: PAGE_ID,
  title: "Prompt to response Flow",
  url: `https://notion.so/Prompt-to-response-Flow-${PAGE_ID.replaceAll("-", "")}`,
};
const secondPage = {
  id: SECOND_PAGE_ID,
  title: "Beta proposal",
  url: `https://notion.so/Beta-proposal-${SECOND_PAGE_ID.replaceAll("-", "")}`,
};
const searchResult = {
  pages: [page],
  has_more: false,
  message: "Found 1 page(s)",
};
const pageResult = {
  pages: [page],
  has_more: false,
  blocks: [
    { block_type: "heading_2", text: "Flow" },
    { block_type: "paragraph", text: "The prompt becomes a response." },
    { block_type: "missing_hint", text: "row 2 has empty cells" },
  ],
};
const answerText = {
  parts: [{ type: "text", text: "A real answer." }],
  citations: [],
};
const context = {
  text: "Summarize the flow page",
  emit: { awaitInput: vi.fn() },
};

let run: Run;
let retrieval: Retrieval;
let answer: ReturnType<typeof vi.fn>;
let invoke: ReturnType<typeof vi.fn>;

function done(result: unknown) {
  return Promise.resolve({ status: "done", result });
}

function failed(message: string, code = "not_found", kind = "InvalidInput") {
  return Promise.resolve({
    status: "error",
    error: { code, kind, message },
  });
}

function executionSession() {
  return {
    submitIntent: async () => ({
      submitPlan: async ({ steps }: { steps: Array<{ stepId: string; dependsOn: string[] }> }) => {
        const completed = new Set<string>();
        const started = new Set<string>();
        const dependencies = new Map(steps.map((step) => [step.stepId, step.dependsOn]));
        return {
          startStep: async (stepId: string) => {
            for (const dependency of dependencies.get(stepId) ?? []) {
              if (!completed.has(dependency)) {
                throw new Error(`cannot start ${stepId} before dependency: ${dependency}`);
              }
            }
            started.add(stepId);
          },
          completeStep: async (stepId: string) => {
            if (!started.has(stepId)) throw new Error(`step not started: ${stepId}`);
            completed.add(stepId);
          },
          finish: async () => undefined,
          abort: async () => undefined,
        };
      },
    }),
  };
}

beforeAll(async () => {
  vi.stubGlobal("__chat_register", (agent: { run: Run }) => {
    run = agent.run;
  });
  vi.stubGlobal("PlanNotionRetrieval", async () => retrieval);
  vi.stubGlobal("AnswerFromNotion", (args: unknown) => answer(args));
  vi.stubGlobal("invokeTool", (toolName: string, input: unknown) => invoke(toolName, input));
  await import("../../../agents/notion-agent/src/index.ts");
});

beforeEach(() => {
  retrieval = {
    kind: "retrieve",
    goal: "Summarize the flow page",
    search_queries: ["flow"],
    page_urls: [],
    read_mode: "content",
    selection: "best_matches",
    max_pages: 1,
  };
  answer = vi.fn(async () => answerText);
  invoke = vi.fn(async (_toolName: string, input: Record<string, unknown>) => {
    if ("block_id" in input) return done(pageResult);
    if ("page_id" in input) return done({ ...searchResult, blocks: [] });
    return done(searchResult);
  });
  vi.stubGlobal("openA2aExecutionSession", async () => executionSession());
});

describe("notion answers", () => {
  it("answers from content fetched directly by code", async () => {
    const result = await run(context);

    expect(result.message).toEqual(answerText);
    expect(answer).toHaveBeenCalledWith({
      goal: "Summarize the flow page",
      user_message: "Summarize the flow page",
      read_mode: "content",
      matched_page_count: 1,
      found_pages: [
        {
          id: page.id,
          title: page.title,
          url: page.url,
          last_edited_time: null,
        },
      ],
      page_contents: [
        {
          id: page.id,
          title: page.title,
          url: page.url,
          text: "Flow\nThe prompt becomes a response.",
        },
      ],
      read_failures: [],
      content_truncated: false,
      result_list_truncated: false,
      search_truncated: false,
    });
  });

  it("completes the empty content path without calling the answer model", async () => {
    invoke = vi.fn(() => done({ pages: [], has_more: false, message: "Found 0 page(s)" }));

    const result = await run({ ...context, text: "Summarize a missing page" });

    expect(result.error).toBeUndefined();
    expect(answer).not.toHaveBeenCalled();
    expect(result.message).toEqual({
      parts: [{ type: "text", text: NO_PAGES_MESSAGE }],
      citations: [],
    });
  });

  it("does not list or read arbitrary pages when content retrieval has no locator", async () => {
    retrieval = {
      ...retrieval,
      search_queries: [],
      page_urls: [],
    };
    invoke = vi.fn();

    const result = await run({
      ...context,
      text: "Read page 238cff78-8181-80c8-8273-cc5fbdd8c7da",
    });

    expect(invoke).not.toHaveBeenCalled();
    expect(answer).not.toHaveBeenCalled();
    expect(result.message).toEqual({
      parts: [{ type: "text", text: NO_PAGES_MESSAGE }],
      citations: [],
    });
  });

  it("uses an unfiltered search only for an explicit page listing", async () => {
    retrieval = {
      ...retrieval,
      search_queries: [],
      page_urls: [],
      read_mode: "search_results",
      selection: "all_matches",
    };

    const result = await run({ ...context, text: "List all Notion pages" });

    expect(result.error).toBeUndefined();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toEqual({
      query: null,
      start_cursor: null,
      page_size: 100,
    });
  });

  it("extracts only the page id in a Notion URL path", async () => {
    const compactPageId = PAGE_ID.replaceAll("-", "");
    retrieval = {
      ...retrieval,
      search_queries: ["Prompt to response Flow"],
      page_urls: [
        `https://www.notion.so/workspace/Prompt-to-response-Flow-${compactPageId}?v=${VIEW_ID}`,
      ],
      read_mode: "metadata",
    };
    const directResult = { pages: [page], has_more: false, message: "Found page" };
    invoke = vi.fn((_toolName: string, input: Record<string, unknown>) => {
      if (input.page_id === PAGE_ID) return done(directResult);
      return failed(`unexpected input: ${JSON.stringify(input)}`);
    });

    const result = await run({
      ...context,
      text: `${retrieval.page_urls[0]} file hash ${FILE_HASH}`,
    });

    expect(result.error).toBeUndefined();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toEqual({ page_id: PAGE_ID });
  });

  it("falls back to title search when a direct page fetch misses", async () => {
    const compactPageId = PAGE_ID.replaceAll("-", "");
    retrieval = {
      ...retrieval,
      search_queries: ["Prompt to response Flow"],
      page_urls: [`https://notion.so/Prompt-to-response-Flow-${compactPageId}?v=${VIEW_ID}`],
      read_mode: "metadata",
    };
    invoke = vi.fn((_toolName: string, input: Record<string, unknown>) => {
      if ("page_id" in input) return failed("stale page id");
      return done(searchResult);
    });

    const result = await run(context);

    expect(result.error).toBeUndefined();
    expect(invoke.mock.calls.map((call) => call[1])).toEqual([
      { page_id: PAGE_ID },
      { query: "Prompt to response Flow", start_cursor: null, page_size: 100 },
    ]);
  });

  it.each([
    {
      label: "authorization",
      message: "Notion API authentication failed (401)",
      code: "notion_unauthorized",
      kind: "NotAuthorized",
    },
    {
      label: "rate limit",
      message: "Notion rate limit exceeded (429)",
      code: "notion_rate_limited",
      kind: "RateLimited",
    },
  ])("does not hide $label errors from direct page fetches", async ({ message, code, kind }) => {
    const compactPageId = PAGE_ID.replaceAll("-", "");
    retrieval = {
      ...retrieval,
      search_queries: [],
      page_urls: [`https://notion.so/${compactPageId}`],
      read_mode: "metadata",
    };
    invoke = vi.fn(() => failed(message, code, kind));

    const result = await run(context);

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(result.error).toContain(message);
    expect(result.message).toBeUndefined();
  });

  it("answers from readable pages and reports individual read failures", async () => {
    retrieval = {
      ...retrieval,
      goal: "Compare Alpha and Beta",
      search_queries: ["Alpha", "Beta"],
      selection: "all_matches",
      max_pages: 2,
    };
    const alpha = { ...page, title: "Alpha proposal" };
    invoke = vi.fn((_toolName: string, input: Record<string, unknown>) => {
      if (input.block_id === PAGE_ID) return failed("Alpha is not shared");
      if (input.block_id === SECOND_PAGE_ID) {
        return done({
          blocks: [{ block_type: "paragraph", text: "Beta text" }],
          has_more: false,
          message: "Retrieved page blocks",
        });
      }
      return done({
        pages: [alpha, secondPage],
        has_more: false,
        message: "Found 2 pages",
      });
    });

    const result = await run({ ...context, text: "Compare Alpha and Beta" });

    expect(result.error).toBeUndefined();
    expect(answer.mock.calls[0][0].page_contents).toEqual([{ ...secondPage, text: "Beta text" }]);
    expect(answer.mock.calls[0][0].read_failures).toEqual([
      { title: "Alpha proposal", url: alpha.url, reason: "Alpha is not shared" },
    ]);
  });

  it("fails only when every selected page read fails", async () => {
    invoke = vi.fn((_toolName: string, input: Record<string, unknown>) => {
      if ("block_id" in input) return failed("page is not shared");
      return done(searchResult);
    });

    const result = await run(context);

    expect(result.error).toContain("Could not read any selected Notion page");
    expect(answer).not.toHaveBeenCalled();
  });

  it("drops zero-score candidates when a stronger title match exists", async () => {
    retrieval = { ...retrieval, read_mode: "metadata" };
    const unrelated = { ...secondPage, title: "Completely unrelated" };
    invoke = vi.fn(() =>
      done({
        pages: [unrelated, page],
        has_more: false,
        message: "Found 2 pages",
      }),
    );

    await run(context);

    expect(answer.mock.calls[0][0].matched_page_count).toBe(1);
    expect(answer.mock.calls[0][0].found_pages).toHaveLength(1);
    expect(answer.mock.calls[0][0].found_pages[0].id).toBe(PAGE_ID);
  });

  it("paginates all matches and does not mark an intentional best-match stop incomplete", async () => {
    retrieval = {
      ...retrieval,
      read_mode: "search_results",
      selection: "all_matches",
      max_pages: 10,
    };
    invoke = vi
      .fn()
      .mockImplementationOnce(() =>
        done({
          pages: [page],
          has_more: true,
          next_cursor: "next",
          message: "page 1",
        }),
      )
      .mockImplementationOnce(() =>
        done({ pages: [secondPage], has_more: false, message: "page 2" }),
      );

    await run(context);

    expect(invoke).toHaveBeenCalledTimes(2);
    expect(answer.mock.calls[0][0].search_truncated).toBe(false);

    retrieval = { ...retrieval, selection: "best_matches", max_pages: 1 };
    answer.mockClear();
    invoke = vi.fn(() =>
      done({
        pages: [page],
        has_more: true,
        next_cursor: "ignored",
        message: "page 1",
      }),
    );

    await run(context);

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(answer.mock.calls[0][0].search_truncated).toBe(false);
  });

  it("returns an answer-model failure as an error", async () => {
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
    addToolOutput(findings, {
      pages: [page],
      blocks: [{ text: "0123456789" }],
    });

    const input = answerInput(findings, {
      selectedPages: [page],
      maxChars: 4,
    });

    expect(input.page_contents[0].text).toBe("0123");
    expect(input.content_truncated).toBe(true);
  });

  it("records block text against the requested page when metadata lookup fails", () => {
    const findings = emptyFindings();
    addToolOutput(findings, { blocks: [{ text: "still readable" }], has_more: false }, page);

    const input = answerInput(findings, { selectedPages: [page] });

    expect(input.page_contents).toEqual([{ ...page, text: "still readable" }]);
  });

  it("shares the content budget across pages", () => {
    const findings = emptyFindings();
    addToolOutput(findings, {
      pages: [page],
      blocks: [{ text: "AAAAAAAAAA" }],
    });
    addToolOutput(findings, {
      pages: [secondPage],
      blocks: [{ text: "BB" }],
    });

    const input = answerInput(findings, {
      selectedPages: [page, secondPage],
      maxChars: 6,
    });

    expect(input.page_contents.map((content) => content.text)).toEqual(["AAAA", "BB"]);
    expect(input.content_truncated).toBe(true);
  });

  it("marks a page the tool could not finish", () => {
    const findings = emptyFindings();
    addToolOutput(findings, {
      pages: [page],
      blocks: [{ text: "start" }],
      has_more: true,
    });

    expect(answerInput(findings, { selectedPages: [page] }).content_truncated).toBe(true);
  });

  it("does not treat more search results as an unfinished page", () => {
    const findings = emptyFindings();
    addToolOutput(findings, { pages: [page], has_more: true });

    expect(answerInput(findings, { selectedPages: [page] }).content_truncated).toBe(false);
  });

  it("does not repeat a page fetched twice", () => {
    const findings = emptyFindings();
    addToolOutput(findings, pageResult);
    addToolOutput(findings, pageResult);

    expect(findings.contents).toHaveLength(1);
    expect(findings.contents[0].text).toBe("Flow\nThe prompt becomes a response.");
  });
});
