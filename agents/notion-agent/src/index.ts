/// <reference path="./baml-runtime.d.ts" />
import type {
  NotionInput,
  NotionOutput,
  ReplyPart,
  RunContext,
  SessionResult,
  StructuredReply,
  ToolFailure,
} from "./baml-runtime";
import {
  NO_PAGES_MESSAGE,
  NotionFindings,
  NotionPageRef,
  addToolOutput,
  answerInput,
  emptyFindings,
} from "./notionFindings";

const MAX_SEARCH_QUERIES = 3;
const MAX_SEARCH_PAGES_PER_QUERY = 5;
const SEARCH_PAGE_SIZE = 100;
const MAX_CONTENT_PAGES = 5;
const MAX_RESULT_PAGES = 50;

type NeedClarification = { kind: "clarify"; question: string };
type NotRelevant = { kind: "not_relevant"; reason: string };
type NotionRetrieval = {
  kind: "retrieve";
  goal: string;
  search_queries: string[];
  page_urls: string[];
  read_mode: "search_results" | "metadata" | "content";
  selection: "best_matches" | "all_matches";
  max_pages: number;
};

type Candidate = {
  page: NotionPageRef;
  score: number;
  order: number;
};

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

function isNeedClarification(value: unknown): value is NeedClarification {
  return (
    isObject(value) &&
    value.kind === "clarify" &&
    typeof value.question === "string" &&
    value.question.trim().length > 0
  );
}

function isNotRelevant(value: unknown): value is NotRelevant {
  return (
    isObject(value) &&
    value.kind === "not_relevant" &&
    typeof value.reason === "string" &&
    value.reason.trim().length > 0
  );
}

function isNotionRetrieval(value: unknown): value is NotionRetrieval {
  return (
    isObject(value) &&
    value.kind === "retrieve" &&
    typeof value.goal === "string" &&
    value.goal.trim().length > 0 &&
    Array.isArray(value.search_queries) &&
    value.search_queries.every((query) => typeof query === "string") &&
    Array.isArray(value.page_urls) &&
    value.page_urls.every((url) => typeof url === "string") &&
    (value.read_mode === "search_results" ||
      value.read_mode === "metadata" ||
      value.read_mode === "content") &&
    (value.selection === "best_matches" || value.selection === "all_matches") &&
    typeof value.max_pages === "number" &&
    Number.isFinite(value.max_pages)
  );
}

function normalizeUserMessage(text: string): string {
  const trimmed = text.trim();
  const notionDirective = trimmed.match(/^use\s+notion\s*[:,-]?\s*/i);
  if (!notionDirective) return trimmed;
  const withoutDirective = trimmed.slice(notionDirective[0].length).trim();
  return withoutDirective.length > 0 ? withoutDirective : trimmed;
}

function slugGoal(goal: string): string {
  return (
    goal
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 48) || "goal"
  );
}

function normalizedPageId(value: string): string | null {
  const compact = value.replace(/-/g, "");
  if (!/^[0-9a-f]{32}$/i.test(compact)) return null;
  const id = compact.toLowerCase();
  return `${id.slice(0, 8)}-${id.slice(8, 12)}-${id.slice(12, 16)}-${id.slice(16, 20)}-${id.slice(20)}`;
}

type NotionUrlTarget = { id: string; titleHint: string | null };

function notionTargetsFromUrls(values: string[]): NotionUrlTarget[] {
  const targets: NotionUrlTarget[] = [];
  const notionUrlPattern =
    /https?:\/\/(?:[a-z0-9-]+\.)*notion\.(?:so|site)\/[^\s<>"']+/gi;
  const pageIdPattern =
    /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32}/gi;

  for (const value of values) {
    for (const rawUrl of value.match(notionUrlPattern) ?? []) {
      const url = rawUrl.replace(/[),.;]+$/, "");
      const pathStart = url.indexOf("/", url.indexOf("://") + 3);
      const path = url.slice(pathStart + 1).split(/[?#]/, 1)[0] ?? "";
      const matches = path.match(pageIdPattern) ?? [];
      const rawId = matches[matches.length - 1];
      const id = rawId ? normalizedPageId(rawId) : null;
      if (!id) continue;

      const segment = path.split("/").filter(Boolean).pop() ?? "";
      const idOffset = segment.toLowerCase().lastIndexOf(rawId.toLowerCase());
      let titleHint = idOffset > 0 ? segment.slice(0, idOffset) : "";
      try {
        titleHint = decodeURIComponent(titleHint);
      } catch {
        // Keep the encoded path text as a fallback query.
      }
      titleHint = titleHint
        .replace(/[-_]+$/g, "")
        .replace(/[-_]+/g, " ")
        .trim();

      if (!targets.some((target) => target.id === id)) {
        targets.push({ id, titleHint: titleHint || null });
      }
    }
  }
  return targets;
}

function cleanQueries(queries: string[]): string[] {
  const cleaned: string[] = [];
  for (const query of queries) {
    const value = query.trim();
    if (
      value &&
      !cleaned.some((known) => known.toLowerCase() === value.toLowerCase())
    ) {
      cleaned.push(value);
    }
    if (cleaned.length === MAX_SEARCH_QUERIES) break;
  }
  return cleaned;
}

function normalizedText(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
}

function titleScore(title: string, queries: string[]): number {
  const normalizedTitle = normalizedText(title);
  let best = 0;
  for (let index = 0; index < queries.length; index++) {
    const normalizedQuery = normalizedText(queries[index]!);
    if (!normalizedQuery) continue;
    let score = 0;
    if (normalizedTitle === normalizedQuery) score = 1_000;
    else if (normalizedTitle.startsWith(normalizedQuery)) score = 700;
    else if (normalizedTitle.includes(normalizedQuery)) score = 500;
    else {
      const tokens = normalizedQuery
        .split(" ")
        .filter((token) => token.length > 1);
      score =
        tokens.filter((token) => normalizedTitle.includes(token)).length * 50;
    }
    best = Math.max(best, score - index);
  }
  return best;
}

class NotionToolCallError extends Error {
  readonly failure: ToolFailure;

  constructor(failure: ToolFailure) {
    super(failure.message || `Notion failed: ${failure.code}`);
    this.name = "NotionToolCallError";
    this.failure = failure;
  }
}

function isNotionNotFound(error: unknown): boolean {
  if (!(error instanceof NotionToolCallError)) return false;
  const code = error.failure.code.toLowerCase();
  const message = error.message.toLowerCase();
  return (
    code === "not_found" ||
    code === "notion_not_found" ||
    message.includes("object_not_found") ||
    message.includes("resource not found (404)") ||
    message.includes("could not find")
  );
}

async function invokeNotion(input: NotionInput): Promise<NotionOutput> {
  const outcome = await invokeTool("support/notion", input);
  if (outcome.status === "error") {
    throw new NotionToolCallError(outcome.error);
  }
  return outcome.result;
}

function addCandidates(
  candidates: Map<string, Candidate>,
  findings: NotionFindings,
  output: NotionOutput,
  queries: string[],
  directIds: Set<string>,
): void {
  addToolOutput(findings, output);
  for (const page of output.pages ?? []) {
    const direct = directIds.has(
      normalizedPageId(page.id) ?? page.id.toLowerCase(),
    );
    const score = direct ? 10_000 : titleScore(page.title, queries);
    const candidateId = normalizedPageId(page.id) ?? page.id.toLowerCase();
    const known = candidates.get(candidateId);
    if (!known) {
      candidates.set(candidateId, { page, score, order: candidates.size });
    } else {
      known.score = Math.max(known.score, score);
      known.page = page;
    }
  }
}

async function retrieveCandidates(
  retrieval: NotionRetrieval,
  userText: string,
  findings: NotionFindings,
): Promise<{ candidates: Candidate[]; searchIncomplete: boolean }> {
  const directTargets = notionTargetsFromUrls([
    ...retrieval.page_urls,
    userText,
  ]);
  const directIds = new Set(directTargets.map((target) => target.id));
  const queries = cleanQueries([
    ...directTargets.flatMap((target) =>
      target.titleHint ? [target.titleHint] : [],
    ),
    ...retrieval.search_queries,
  ]);
  const candidates = new Map<string, Candidate>();
  let directFetchMiss = false;
  let searchIncomplete = false;

  for (const target of directTargets) {
    try {
      const output = await invokeNotion({ page_id: target.id });
      addCandidates(candidates, findings, output, queries, directIds);
    } catch (error) {
      if (!isNotionNotFound(error)) throw error;
      directFetchMiss = true;
      // A stale or missing URL is a miss. Search can still resolve its title hint.
    }
  }

  const listAll =
    retrieval.read_mode === "search_results" &&
    retrieval.selection === "all_matches";
  const shouldSearch =
    queries.length > 0
      ? directTargets.length === 0 ||
        directFetchMiss ||
        candidates.size === 0 ||
        retrieval.selection === "all_matches"
      : directTargets.length === 0 && listAll;
  const searches: Array<string | null> = queries.length > 0 ? queries : [null];
  if (shouldSearch) {
    for (const query of searches) {
      let cursor: string | null = null;
      const apiPageLimit =
        retrieval.selection === "all_matches" ? MAX_SEARCH_PAGES_PER_QUERY : 1;
      for (let apiPage = 0; apiPage < apiPageLimit; apiPage++) {
        const output = await invokeNotion({
          query,
          start_cursor: cursor,
          page_size: SEARCH_PAGE_SIZE,
        });
        addCandidates(candidates, findings, output, queries, directIds);
        cursor = output.next_cursor ?? null;
        if (!output.has_more) break;
        if (retrieval.selection === "all_matches" && !cursor) {
          searchIncomplete = true;
          break;
        }
        if (
          retrieval.selection === "all_matches" &&
          apiPage === apiPageLimit - 1
        ) {
          searchIncomplete = true;
        }
      }
    }
  }

  let ranked = [...candidates.values()].sort(
    (left, right) => right.score - left.score || left.order - right.order,
  );
  if (queries.length > 0 && ranked.some((candidate) => candidate.score > 0)) {
    ranked = ranked.filter((candidate) => candidate.score > 0);
  }

  return { candidates: ranked, searchIncomplete };
}

function selectedPageLimit(retrieval: NotionRetrieval): number {
  const requested = Math.max(1, Math.floor(retrieval.max_pages));
  const maximum =
    retrieval.read_mode === "content" ? MAX_CONTENT_PAGES : MAX_RESULT_PAGES;
  return Math.min(requested, maximum);
}

async function runNotionRetrieval(
  userText: string,
  retrieval: NotionRetrieval,
): Promise<SessionResult> {
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`notion-${Date.now()}`)
      : null;
  const intentId = `intent-notion-${slugGoal(retrieval.goal)}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: retrieval.goal,
      })
    : null;
  const planSteps = [
    {
      stepId: "retrieve-pages",
      description: "Find matching Notion pages and metadata.",
    },
    ...(retrieval.read_mode === "content"
      ? [
          {
            stepId: "read-content",
            description: "Read the selected page content.",
          },
        ]
      : []),
    {
      stepId: "answer",
      description: "Answer the user's request from the retrieved evidence.",
    },
  ];
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-notion-${slugGoal(retrieval.goal)}`,
        steps: planSteps.map((step, order) => ({
          ...step,
          order,
          dependsOn: order > 0 ? [planSteps[order - 1]!.stepId] : [],
        })),
      })
    : null;

  const findings = emptyFindings();
  try {
    await executable?.startStep?.("retrieve-pages");
    const retrieved = await retrieveCandidates(retrieval, userText, findings);
    const limit = selectedPageLimit(retrieval);
    const selected = retrieved.candidates
      .slice(0, limit)
      .map((candidate) => candidate.page);
    const resultListTruncated = retrieved.candidates.length > selected.length;
    await executable?.completeStep?.("retrieve-pages");

    if (selected.length === 0) {
      if (retrieval.read_mode === "content") {
        await executable?.startStep?.("read-content");
        await executable?.completeStep?.("read-content");
      }
      await executable?.startStep?.("answer");
      await executable?.completeStep?.("answer");
      await executable?.finish?.();
      return { message: textReply(NO_PAGES_MESSAGE) };
    }

    const readFailures: Array<{
      title: string;
      url: string;
      reason: string;
    }> = [];
    if (retrieval.read_mode === "content") {
      await executable?.startStep?.("read-content");
      let successfulReads = 0;
      for (const page of selected) {
        try {
          const output = await invokeNotion({ block_id: page.id });
          addToolOutput(findings, output, page);
          successfulReads++;
        } catch (error) {
          readFailures.push({
            title: page.title,
            url: page.url,
            reason: error instanceof Error ? error.message : String(error),
          });
        }
      }
      if (successfulReads === 0 && readFailures.length > 0) {
        throw new Error(
          `Could not read any selected Notion page: ${readFailures.map((failure) => failure.title).join(", ")}`,
        );
      }
      await executable?.completeStep?.("read-content");
    }

    await executable?.startStep?.("answer");
    const finalMessage = (await AnswerFromNotion({
      goal: retrieval.goal,
      user_message: userText,
      read_mode: retrieval.read_mode,
      read_failures: readFailures,
      ...answerInput(findings, {
        selectedPages: selected,
        matchedPageCount: retrieved.candidates.length,
        resultListTruncated,
        searchTruncated: retrieved.searchIncomplete,
      }),
    })) as StructuredReply;
    await executable?.completeStep?.("answer");
    await executable?.finish?.();
    return { message: finalMessage };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(message);
    } catch {
      // Preserve the retrieval error.
    }
    return { error: `Notion agent error: ${message}` };
  }
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    let text = normalizeUserMessage(ctx.text || "unknown");

    while (true) {
      const decision = await PlanNotionRetrieval({ user_message: text });
      if (isNotionRetrieval(decision)) {
        return runNotionRetrieval(text, decision);
      }
      if (isNotRelevant(decision)) {
        return {
          message: textReply(
            `This doesn't look like a Notion question. ${decision.reason}`,
          ),
        };
      }
      if (isNeedClarification(decision)) {
        const reply = await ctx.emit.awaitInput(decision.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = normalizeUserMessage(clarifiedText);
        continue;
      }
      return {
        error:
          "PlanNotionRetrieval returned an unexpected shape. Expected NotionRetrieval, NeedClarification, or NotRelevant.",
      };
    }
  },
});
