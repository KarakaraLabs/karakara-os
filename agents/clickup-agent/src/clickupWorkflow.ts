/// <reference path="./baml-runtime.d.ts" />
/**
 * ClickUp plan execution — intent/plan provenance + step-executor loop.
 * Sequenced like slack-agent `runSlackStructuredPlan` / how-to-write-agents §3.6.
 */
import type {
  ClickUpInput,
  ClickUpIntent,
  ClickUpItem,
  ClickUpOutput,
  ClickUpPlanStep,
  ClickUpPriorityIntent,
  ClickUpStructuredPlan,
  ClickUpTaskSummary,
  FinalResponse,
  JsonObject,
  JsonValue,
  ReplyPart,
  SessionResult,
  StructuredReply,
  SystemOneOutput,
} from "./baml-runtime";

export const MAX_REACT_STEPS = 32;
export const PKG_CLICKUP_EXECUTE = "clickup-execute";
export const PKG_CLICKUP_FORMAT = "clickup-format";

const PRIOR_RESULTS_MAX_CHARS = 6000;

function isJsonObject(v: unknown): v is JsonObject {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
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

function stringifyUnknown(value: unknown, max: number): string {
  try {
    const s = JSON.stringify(value, null, 2);
    return s.length > max ? `${s.slice(0, max)}\n…` : s;
  } catch {
    const s = String(value);
    return s.length > max ? `${s.slice(0, max)}…` : s;
  }
}

function extractToolLikePayload(v: unknown): JsonObject | null {
  if (isJsonObject(v) && (Array.isArray(v.tasks) || Array.isArray(v.items)))
    return v;
  if (
    isJsonObject(v) &&
    isJsonObject(v.output) &&
    (Array.isArray(v.output.tasks) || Array.isArray(v.output.items))
  ) {
    return v.output;
  }
  return null;
}

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isJsonObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !(
    "tasks" in v ||
    "items" in v ||
    "steps" in v ||
    "action" in v ||
    "intent" in v
  );
}

function isStructuredReply(v: unknown): v is StructuredReply {
  return (
    isJsonObject(v) && Array.isArray(v.parts) && Array.isArray(v.citations)
  );
}

function isReadOnlyFinish(
  v: unknown,
): v is { op: "ReadOnlyFinish"; reply: StructuredReply } {
  return (
    isJsonObject(v) && v.op === "ReadOnlyFinish" && isStructuredReply(v.reply)
  );
}

// ── Step evidence ────────────────────────────────────────────────────────────
//
// `run.outcome === "completed"` only means the executor loop terminated cleanly. It says nothing
// about whether the step produced the data the plan needed. Evidence is derived from what the
// executor actually saw: Send receipts, archive-read headers, and inline result payloads.

export type ExpectedEvidence = ClickUpPlanStep["expects"];

export type StepEvidence = {
  /// A ClickUp task list was retrieved (Send or a read of its archive).
  listedTasks: boolean;
  /// A single task record was retrieved.
  taskDetail: boolean;
  /// Teams, spaces, or lists were retrieved.
  navigated: boolean;
  /// A create/update/delete completed.
  wrote: boolean;
  /// Largest task array observed in one result.
  taskCount: number;
  /// Action identities observed, for diagnostics.
  actions: string[];
};

function emptyEvidence(): StepEvidence {
  return {
    listedTasks: false,
    taskDetail: false,
    navigated: false,
    wrote: false,
    taskCount: 0,
    actions: [],
  };
}

/**
 * Fold one executor step into the running evidence record.
 *
 * The action identity appears as `Send(list_tasks list_id=…)` on the receipt and at the head of an
 * archive-read output (`@4 · support/clickup:Send(list_tasks …)`), so scanning the rendered step
 * text catches both a live Send and a later read of that Send's archive.
 */
function accumulateEvidence(evidence: StepEvidence, step: unknown): void {
  if (!isJsonObject(step)) return;
  const receipt = isJsonObject(step.receipt) ? step.receipt : null;
  const identity =
    receipt !== null && typeof receipt.action_identity === "string"
      ? receipt.action_identity
      : null;
  if (identity !== null) evidence.actions.push(identity);

  const rendered = stringifyUnknown(step, 20000);
  const actionMatches = rendered.match(
    /Send\(([a-z_]+)(?:\s[^\)]*)?\)/g,
  );
  const ops = new Set<string>();
  for (const raw of [...(actionMatches ?? []), ...(identity !== null ? [identity] : [])]) {
    const op = /Send\(([a-z_]+)/.exec(raw)?.[1];
    // `confirm_delete=false` only asks ClickUp to identify the task; nothing was deleted.
    if (op && !(op === "delete_task" && /\bconfirm_delete=false\b/.test(raw))) ops.add(op);
  }
  if (ops.has("list_tasks")) evidence.listedTasks = true;
  if (ops.has("get_task")) evidence.taskDetail = true;
  if (
    ops.has("list_teams") ||
    ops.has("list_spaces") ||
    ops.has("list_lists")
  )
    evidence.navigated = true;
  if (
    ops.has("create_task") ||
    ops.has("update_task") ||
    ops.has("delete_task")
  )
    evidence.wrote = true;

  // Step-executor hops wrap the payload as `{ status, output, receipt, result }`, so look at both
  // the step itself (plain tool result) and its `result` field (executor envelope).
  const payload = extractToolLikePayload(step) ?? extractToolLikePayload(step.result);
  const tasks = payload?.tasks;
  if (Array.isArray(tasks)) {
    evidence.listedTasks = true;
    evidence.taskCount = Math.max(evidence.taskCount, tasks.length);
  }
  if (isJsonObject(step.result) && isJsonObject(step.result.task))
    evidence.taskDetail = true;
  const items = payload?.items;
  if (Array.isArray(items) && tasks === undefined) evidence.navigated = true;
}

export function collectStepEvidence(steps: unknown[]): StepEvidence {
  const evidence = emptyEvidence();
  for (const step of steps) accumulateEvidence(evidence, step);
  return evidence;
}

function evidenceSatisfies(
  expects: ExpectedEvidence,
  evidence: StepEvidence,
): boolean {
  switch (expects) {
    case "task_list":
      return evidence.listedTasks;
    case "task_detail":
      return evidence.taskDetail;
    case "navigation":
      return evidence.navigated;
    case "write_confirmation":
      return evidence.wrote;
    case "none":
      return true;
  }
}

/** Prose that announces the next navigation hop instead of answering the request. */
function looksLikePartialNavigation(text: string): boolean {
  return /\bnext (would|step|is to)\b/i.test(text) ||
    /\bwould (then )?(list|fetch|query|filter)\b/i.test(text) ||
    /\bnext: ?list/i.test(text);
}

/**
 * Whether a navigation-continuation phrase disqualifies a terminal reply.
 *
 * A hierarchy answer is allowed to mention what would come next ("1 team, 1 space. Next would list
 * lists.") because the hierarchy IS the answer. Task-level and write plans must not end that way:
 * the phrase means the executor stopped before retrieving the data it was asked for.
 */
function requiresTaskLevelEvidence(expects: ExpectedEvidence): boolean {
  return (
    expects === "task_list" ||
    expects === "task_detail" ||
    expects === "write_confirmation"
  );
}

/**
 * Candidate task ids Jev scored, minus the synthetic "none" option.
 *
 * The formatter receives the decision JSON, not the ranking, so a message that names a different
 * candidate is a re-ranking the prompt forbade. The ids come from Jev's own probability map so this
 * check cannot drift from the evaluated candidate set.
 */
function scoredCandidateIds(decision: PriorityDecision): string[] {
  const answer = decision.jev?.answers?.recommended_task;
  if (answer === undefined || answer.type !== "choice") return [];
  return Object.keys(answer.probabilities).filter((id) => id !== "none");
}

/**
 * Deterministic rendering of a validated recommendation. Used as the fallback whenever the
 * formatter output cannot be trusted to preserve the validated task.
 */
function renderValidatedRecommendation(decision: PriorityDecision): string {
  const task = decision.task;
  if (task === undefined) {
    return `I could not recommend a task. ${decision.reason ?? ""}`.trim();
  }
  const lines = [
    `**Recommended task to start immediately: ${task.name} (ID: ${task.id})**`,
  ];
  if (task.status) lines.push("", `- Status: ${task.status}`);
  if (task.priority) lines.push(`- Priority: ${task.priority}`);
  if (task.description)
    lines.push(
      `- Description: ${truncate(task.description, DESCRIPTION_MAX_CHARS)}`,
    );
  if (task.url) lines.push(`- URL: ${task.url}`);
  if (typeof decision.urgency === "number") {
    lines.push(
      "",
      `Urgency: ${decision.urgency} (evidence sufficiency: ${decision.evidence_sufficient ?? "unknown"}).`,
    );
  }
  return lines.join("\n");
}

/**
 * Keep the formatter from changing the validated recommendation.
 *
 * The formatter's return type already forbids tool operations, but its `message` is free text, so
 * "do not select a new task" was only a prompt instruction. Replace the message when it names a
 * scored candidate other than the validated one, or when it omits the validated task entirely.
 */
function enforceValidatedRecommendation(
  message: string,
  decision: PriorityDecision,
): string {
  const chosen = decision.task?.id;
  if (decision.outcome !== "recommend" || chosen === undefined) return message;
  const mentions = (id: string) =>
    new RegExp(`(^|[^0-9a-z])${id}([^0-9a-z]|$)`, "i").test(message);
  const otherCandidates = scoredCandidateIds(decision).filter(
    (id) => id !== chosen && mentions(id),
  );
  if (otherCandidates.length > 0 || !mentions(chosen)) {
    return renderValidatedRecommendation(decision);
  }
  return message;
}

function finalResponseToStructured(fr: FinalResponse): StructuredReply {
  const msg = fr.message.trim() || "Done.";
  const parts: StructuredReply["parts"] = [{ type: "text", text: msg }];
  const sj =
    typeof fr.structured_json === "string" ? fr.structured_json.trim() : "";
  if (sj) {
    parts.push({ type: "data", data: sj, media_type: "application/json" });
  }
  const citations = Array.isArray(fr.citations)
    ? fr.citations.filter((c): c is string => typeof c === "string")
    : [];
  return { parts, citations };
}

function formatLineFromClickUpItem(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const kind = typeof entry.kind === "string" ? entry.kind : "";
  const name = typeof entry.name === "string" ? entry.name : "";
  const id = typeof entry.id === "string" ? entry.id : "";
  if (!name && !id) return "";
  return `• [${kind}] ${name} (id: ${id})`;
}

function formatLineFromClickUpTaskSummary(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const name = typeof entry.name === "string" ? entry.name : "Unnamed task";
  const status = typeof entry.status === "string" ? entry.status : "unknown";
  const url = typeof entry.url === "string" ? entry.url : "";
  return `• ${name} [${status}]${url ? ` — ${url}` : ""}`;
}

function formatListLikeToolPayload(output: JsonObject): string {
  const msg = output.message;
  let response = typeof msg === "string" ? msg : "Done.";
  const items = output.items;
  if (Array.isArray(items) && items.length > 0) {
    response +=
      "\n\n" +
      items
        .map(formatLineFromClickUpItem)
        .filter((s) => s.length > 0)
        .join("\n");
  }
  const tasks = output.tasks;
  if (Array.isArray(tasks) && tasks.length > 0) {
    response +=
      "\n\n" +
      tasks
        .map(formatLineFromClickUpTaskSummary)
        .filter((s) => s.length > 0)
        .join("\n");
  }
  return response;
}

export type FinalMessageOutcome =
  | { kind: "message"; reply: StructuredReply }
  | { kind: "incomplete"; reason: string };

/**
 * Pick the user-visible answer, but only when the plan's required evidence actually arrived.
 *
 * Two failure modes are rejected here:
 * 1. the required evidence (e.g. a task list) was never retrieved, and
 * 2. the terminal reply is navigation bookkeeping prose ("Next would list …") rather than an answer.
 */
export function extractFinalMessage(
  steps: unknown[],
  required: ExpectedEvidence = "none",
): FinalMessageOutcome {
  const evidence = collectStepEvidence(steps);
  if (!evidenceSatisfies(required, evidence)) {
    return {
      kind: "incomplete",
      reason:
        `the plan required ${describeEvidence(required)} but the executor produced ` +
        `${describeObserved(evidence)}`,
    };
  }

  for (const step of [...steps].reverse()) {
    if (isReadOnlyFinish(step)) {
      const text = replyText(step.reply);
      if (requiresTaskLevelEvidence(required) && looksLikePartialNavigation(text)) {
        return {
          kind: "incomplete",
          reason: `the terminal reply only described the next navigation hop: "${text.slice(0, 160)}"`,
        };
      }
      return { kind: "message", reply: step.reply };
    }
    if (isFinalResponse(step)) {
      if (requiresTaskLevelEvidence(required) && looksLikePartialNavigation(step.message)) {
        return {
          kind: "incomplete",
          reason: `the final response only described the next navigation hop: "${step.message.slice(0, 160)}"`,
        };
      }
      return { kind: "message", reply: finalResponseToStructured(step) };
    }
    const toolLike = extractToolLikePayload(step);
    if (toolLike)
      return { kind: "message", reply: textReply(formatListLikeToolPayload(toolLike)) };
    if (
      isJsonObject(step) &&
      typeof step.message === "string" &&
      step.message.trim()
    ) {
      const text = step.message.trim();
      if (requiresTaskLevelEvidence(required) && looksLikePartialNavigation(text)) {
        return {
          kind: "incomplete",
          reason: `the terminal message only described the next navigation hop: "${text.slice(0, 160)}"`,
        };
      }
      return { kind: "message", reply: textReply(text) };
    }
  }
  return {
    kind: "incomplete",
    reason: `the executor produced ${describeObserved(evidence)} but no user-visible answer`,
  };
}

function replyText(reply: StructuredReply): string {
  return reply.parts
    .map((p) => (p.type === "text" && typeof p.text === "string" ? p.text : ""))
    .join("\n")
    .trim();
}

function describeEvidence(expects: ExpectedEvidence): string {
  switch (expects) {
    case "task_list":
      return "a ClickUp task list result";
    case "task_detail":
      return "a single task record";
    case "navigation":
      return "a workspace navigation result";
    case "write_confirmation":
      return "a completed create/update/delete";
    case "none":
      return "no specific evidence";
  }
}

function describeObserved(evidence: StepEvidence): string {
  const seen: string[] = [];
  if (evidence.listedTasks)
    seen.push(`a task list (${evidence.taskCount} task(s) inline)`);
  if (evidence.taskDetail) seen.push("a task record");
  if (evidence.navigated) seen.push("navigation rows");
  if (evidence.wrote) seen.push("a write result");
  const actions = evidence.actions.length
    ? ` actions=[${evidence.actions.slice(0, 6).join(", ")}]`
    : "";
  return seen.length ? `${seen.join(", ")}${actions}` : `no ClickUp data${actions}`;
}

function executorStepToPriorContextText(step: unknown): string {
  if (step == null) return "";
  if (isFinalResponse(step)) {
    return `[final_response]\n${step.message}`.trim();
  }
  const toolLike = extractToolLikePayload(step);
  if (toolLike) {
    return `[tool_result]\n${formatListLikeToolPayload(toolLike)}`.trim();
  }
  if (
    isJsonObject(step) &&
    typeof step.message === "string" &&
    step.message.trim()
  ) {
    return `[message]\n${step.message.trim()}`;
  }
  return `[raw]\n${stringifyUnknown(step, 3500)}`;
}

function collectStepResultsForPriorContext(steps: unknown[]): string {
  const parts: string[] = [];
  for (const step of steps) {
    const block = executorStepToPriorContextText(step);
    if (block) parts.push(block);
  }
  const joined = parts.join("\n\n---\n\n");
  if (joined.trim()) return joined.slice(0, PRIOR_RESULTS_MAX_CHARS);
  return stringifyUnknown(steps.slice(-5), PRIOR_RESULTS_MAX_CHARS);
}

function filterPlanCitations(plan: ClickUpStructuredPlan): string[] {
  const raw = plan.citations;
  if (!Array.isArray(raw)) return [];
  return raw.filter(
    (c): c is string => typeof c === "string" && c.trim().length > 0,
  );
}

const MAX_DISCOVERY_CANDIDATES = 64;
const MAX_ALL_TEAMS = 8;
const MAX_ALL_SPACES = 24;
const MAX_ALL_LISTS = 48;
const MAX_ALL_TASKS = 64;
/// Listing is a read: the fan-out caps still apply, but the task cap is what a table can render
/// without silently dropping rows, not the prioritization cap.
const MAX_READ_TASKS = 500;
const DESCRIPTION_MAX_CHARS = 1_000;
const CHOICE_PROBABILITY_THRESHOLD = 0.6;
const CHOICE_CONFIDENCE_THRESHOLD = 0.4;
const EVIDENCE_THRESHOLD = 0.6;

type PriorityDecision = {
  outcome: "recommend" | "abstain";
  reason?: string;
  task?: ClickUpTaskSummary;
  urgency?: number;
  evidence_sufficient?: number;
  /** Records the judge was given, so an abstain can say what it looked at. */
  considered?: number;
  jev?: SystemOneOutput;
};

function truncate(
  value: string | null | undefined,
  max: number,
): string | null {
  if (typeof value !== "string") return null;
  return value.length <= max ? value : `${value.slice(0, max)}…`;
}

function taskState(task: ClickUpTaskSummary): JsonObject {
  return {
    id: task.id,
    name: task.name,
    status: task.status,
    description: truncate(task.description, DESCRIPTION_MAX_CHARS),
    url: task.url,
    assignees: task.assignees ?? [],
    priority: task.priority ?? null,
    due_date: task.due_date ?? null,
  };
}

function discoveryItemState(item: ClickUpItem): JsonObject {
  return { id: item.id, name: item.name, kind: item.kind };
}

function itemCriteria(items: ClickUpItem[]): Record<string, JsonValue> {
  return Object.fromEntries(
    items.map((item) => [item.id, discoveryItemState(item)]),
  );
}

function selectedChoice(output: SystemOneOutput, id: string): string | null {
  const answer = output.answers[id];
  if (answer === undefined || answer.type !== "choice") return null;
  const probability = answer.probabilities[answer.choice] ?? 0;
  return probability >= CHOICE_PROBABILITY_THRESHOLD &&
    answer.confidence >= CHOICE_CONFIDENCE_THRESHOLD
    ? answer.choice
    : null;
}

function noul(output: SystemOneOutput, id: string): number {
  const answer = output.answers[id];
  return answer !== undefined && answer.type === "noul" ? answer.noul : 0;
}

/**
 * Status comparison key: case, spaces, and punctuation are insignificant.
 *
 * ClickUp statuses are workspace-defined and written freely: the same status appears as
 * `to do`, `To Do`, `todo`, `in progress`, `In-Progress`, `Blocked / External`. Comparing
 * normalized prose alone makes `todo` miss `to do`, which is a wrong answer, not a slow one.
 */
function statusKey(value: string): string {
  return value.toLocaleLowerCase().replace(/[^a-z0-9]+/g, "");
}

async function callClickUp(input: ClickUpInput): Promise<ClickUpOutput> {
  const session = await openToolSession("support/clickup");
  try {
    await session.send(input);
    for (let reads = 0; reads < 8; reads++) {
      const step = await session.continue();
      if (step.status === "done") {
        await session.finish();
        if (step.output !== null) return step.output;
        throw new Error("ClickUp returned no output.");
      }
      if (step.status === "error") {
        await session.abort(step.error.code);
        throw new Error(step.error.message || `ClickUp failed: ${step.error.code}`);
      }
    }
    await session.abort("ClickUp did not complete after eight reads.");
    throw new Error("ClickUp did not complete after eight reads.");
  } catch (error) {
    try {
      await session.abort(
        error instanceof Error ? error.message : String(error),
      );
    } catch {
      // The original failure is more useful than a best-effort abort failure.
    }
    throw error;
  }
}

function exactItem(
  items: ClickUpItem[],
  hint: string | null | undefined,
): ClickUpItem | null {
  const normalizedHint = hint?.trim().toLocaleLowerCase();
  if (!normalizedHint) return null;
  return (
    items.find(
      (item) =>
        item.id === hint ||
        item.name.trim().toLocaleLowerCase() === normalizedHint,
    ) ?? null
  );
}

async function chooseDiscoveryItem(
  stage: "team" | "space" | "list",
  intent: ClickUpPriorityIntent,
  candidates: ClickUpItem[],
): Promise<ClickUpItem | null> {
  if (candidates.length === 0) return null;
  if (candidates.length > MAX_DISCOVERY_CANDIDATES) {
    throw new Error(
      `There are too many ${stage} candidates to choose safely. Narrow the request.`,
    );
  }
  const exact = exactItem(candidates, intent.scope_hint);
  if (exact !== null || candidates.length === 1) return exact ?? candidates[0]!;

  const criteria = {
    ...itemCriteria(candidates),
    none: "No candidate clearly matches the user goal",
  };
  const outcome = await invokeTool("typesafe/system_one", {
    state: {
      user_goal: intent.intent,
      scope_hint: intent.scope_hint ?? null,
      stage,
      candidates: candidates.map(discoveryItemState),
    },
    questions: {
      next_candidate: {
        type: "choice",
        instructions:
          "Which candidate best matches user_goal and scope_hint for this discovery stage? Choose none when the match is unclear.",
        criteria,
      },
      sufficient_context: {
        type: "noul",
        instructions:
          "Does user_goal and scope_hint provide enough evidence to select exactly one candidate at this discovery stage?",
      },
    },
  });
  if (outcome.status === "error") {
    throw new Error(`Jev could not select a ${stage}: ${outcome.error.code}`);
  }
  if (noul(outcome.result, "sufficient_context") < EVIDENCE_THRESHOLD)
    return null;
  const choice = selectedChoice(outcome.result, "next_candidate");
  return choice !== null && choice !== "none"
    ? (candidates.find((candidate) => candidate.id === choice) ?? null)
    : null;
}

type TraversalLimits = { maxTasks: number; purpose: string };

const PRIORITY_TRAVERSAL: TraversalLimits = {
  maxTasks: MAX_ALL_TASKS,
  purpose: "prioritize",
};
const READ_TRAVERSAL: TraversalLimits = {
  maxTasks: MAX_READ_TASKS,
  purpose: "list",
};

/**
 * Run `fn` over `items` with bounded concurrency, preserving input order.
 *
 * Used for ClickUp fan-out: each list is an independent API call, and doing them serially makes the
 * traversal scale linearly with the number of lists.
 */
async function mapWithConcurrency<T, R>(
  items: T[],
  limit: number,
  fn: (item: T, index: number) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (true) {
      const index = next++;
      if (index >= items.length) return;
      results[index] = await fn(items[index]!, index);
    }
  });
  await Promise.all(workers);
  return results;
}

const CLICKUP_FANOUT_CONCURRENCY = 4;

/** Every list reachable through teams and spaces. A hint is applied before the list safety gate. */
async function collectAllLists(scopeHint: string | null = null): Promise<ClickUpItem[]> {
  const teams = (await callClickUp({})).items ?? [];
  if (teams.length > MAX_ALL_TEAMS && !scopeHint) {
    throw new Error(
      "There are too many teams to search safely. Name a workspace, space, or list.",
    );
  }

  const spaces = (
    await mapWithConcurrency(teams, CLICKUP_FANOUT_CONCURRENCY, async (team) =>
      (await callClickUp({ team_id: team.id })).items ?? [],
    )
  ).flat();
  if (spaces.length > MAX_ALL_SPACES && !scopeHint) {
    throw new Error(
      "There are too many spaces to search safely. Name a workspace, space, or list.",
    );
  }

  const lists = (
    await mapWithConcurrency(spaces, CLICKUP_FANOUT_CONCURRENCY, async (space) =>
      (await callClickUp({ space_id: space.id })).items ?? [],
    )
  ).flat();
  const hintMatches = scopeHint ? matchingLists(lists, scopeHint) : [];
  const hintedLists = hintMatches.length > 0 ? hintMatches : lists;
  if (hintedLists.length > MAX_ALL_LISTS) {
    throw new Error(
      "There are too many lists to search safely. Name a workspace, space, or list.",
    );
  }
  return hintedLists;
}

async function collectAllTasks(
  limits: TraversalLimits = PRIORITY_TRAVERSAL,
): Promise<ClickUpTaskSummary[]> {
  const lists = await collectAllLists();
  const taskPages = await mapWithConcurrency(
    lists,
    CLICKUP_FANOUT_CONCURRENCY,
    async (list) => (await callClickUp({ list_id: list.id })).tasks ?? [],
  );
  const tasks = taskPages.flat();
  if (tasks.length > limits.maxTasks) {
    throw new Error(
      `There are too many tasks to ${limits.purpose} safely. Name a workspace, space, or list.`,
    );
  }
  return tasks;
}

async function collectScopedTasks(
  intent: ClickUpPriorityIntent,
  limits: TraversalLimits = PRIORITY_TRAVERSAL,
): Promise<ClickUpTaskSummary[]> {
  if (intent.scope === "all_accessible") return collectAllTasks(limits);

  const team = await chooseDiscoveryItem(
    "team",
    intent,
    (await callClickUp({})).items ?? [],
  );
  if (team === null)
    throw new Error(
      "I could not identify one workspace. Name the team, space, or list.",
    );
  const space = await chooseDiscoveryItem(
    "space",
    intent,
    (await callClickUp({ team_id: team.id })).items ?? [],
  );
  if (space === null)
    throw new Error("I could not identify one space. Name the space or list.");
  const list = await chooseDiscoveryItem(
    "list",
    intent,
    (await callClickUp({ space_id: space.id })).items ?? [],
  );
  if (list === null)
    throw new Error("I could not identify one list. Name the list.");
  return (await callClickUp({ list_id: list.id })).tasks ?? [];
}

async function prioritizeTasks(
  intent: ClickUpPriorityIntent,
  tasks: ClickUpTaskSummary[],
  scopeNote: string | null = null,
): Promise<PriorityDecision> {
  const requestedStatuses = intentStatuses(intent);
  const wanted = requestedStatuses
    .map(statusKey)
    .filter((key) => key.length > 0);
  const candidates =
    wanted.length > 0
      ? tasks.filter((task) => {
          const status = statusKey(task.status);
          return wanted.some(
            (value) => status === value || status.includes(value) || value.includes(status),
          );
        })
      : tasks;
  if (candidates.length === 0)
    return {
      outcome: "abstain",
      reason:
        requestedStatuses.length > 0
          ? `No tasks with status ${requestedStatuses.join(" or ")} were returned from the selected scope.`
          : "No tasks were returned from the selected scope.",
      considered: 0,
    };
  if (candidates.length > MAX_ALL_TASKS) {
    return {
      outcome: "abstain",
      reason: `Too many tasks were returned to prioritize safely (limit ${MAX_ALL_TASKS}).`,
      considered: candidates.length,
    };
  }

  const criteria: Record<string, JsonValue> = Object.fromEntries(
    candidates.map((task) => [task.id, taskState(task)]),
  );
  criteria.none = "The task records do not support a recommendation.";
  const outcome = await invokeTool("typesafe/system_one", {
    state: {
      // The scope note tells Jev which set the user is looking at, so it can prefer a task outside
      // that set when the records justify it instead of hedging.
      user_goal: scopeNote ? `${intent.intent} (${scopeNote})` : intent.intent,
      status_filter: requestedStatuses.join(", ") || null,
      tasks: candidates.map(taskState),
    },
    questions: {
      recommended_task: {
        type: "choice",
        instructions:
          "Which task should the user work on first for user_goal? Choose none when the records do not support a recommendation.",
        criteria,
      },
      urgency: {
        type: "score",
        instructions:
          "How urgent is work on the most important task for user_goal?",
        criteria: [
          "No action needed",
          "Plan soon",
          "Work on it next",
          "Urgent, interrupt normal work",
        ],
      },
      sufficient_evidence: {
        type: "noul",
        instructions:
          "Do the available task status, priority, due date, assignees, and description provide enough evidence for a recommendation?",
      },
    },
  });
  if (outcome.status === "error") {
    return {
      outcome: "abstain",
      reason: `The prioritization judge was unavailable: ${outcome.error.code}.`,
      considered: candidates.length,
    };
  }

  const selected = selectedChoice(outcome.result, "recommended_task");
  const evidence = noul(outcome.result, "sufficient_evidence");
  const urgencyAnswer = outcome.result.answers.urgency;
  const urgency =
    urgencyAnswer !== undefined && urgencyAnswer.type === "score"
      ? urgencyAnswer.score
      : undefined;
  if (
    evidence < EVIDENCE_THRESHOLD ||
    selected === null ||
    selected === "none"
  ) {
    return {
      outcome: "abstain",
      reason: selected === "none"
        ? "The judge found no task that should be started first."
        : "The task records do not support a confident recommendation.",
      urgency,
      evidence_sufficient: evidence,
      considered: candidates.length,
      jev: outcome.result,
    };
  }
  const task = candidates.find((candidate) => candidate.id === selected);
  if (task === undefined) {
    return {
      outcome: "abstain",
      reason: "The judge returned a task that was not among the retrieved candidates.",
      considered: candidates.length,
    };
  }
  return {
    outcome: "recommend",
    task,
    urgency,
    evidence_sufficient: evidence,
    jev: outcome.result,
  };
}

/**
 * User-facing explanation for an abstain.
 *
 * An abstain is a real answer, so it must name the reason, the evidence, how many records were
 * judged, and the scope that produced them. Silence or a bare "none" is a bug.
 */
export function describePriorityAbstain(
  decision: PriorityDecision,
  requestedStatuses: string[],
): string {
  const scope =
    requestedStatuses.length > 0
      ? ` with status "${requestedStatuses.join('" or "')}"`
      : "";
  const considered = decision.considered ?? 0;
  const evidence =
    typeof decision.evidence_sufficient === "number"
      ? ` Evidence sufficiency was ${decision.evidence_sufficient.toFixed(2)} (needs ${EVIDENCE_THRESHOLD.toFixed(2)}).`
      : "";
  const reason = decision.reason ?? "no reason was reported.";
  return (
    `I can't recommend a task from these records: ${reason}${evidence} ` +
    `I looked at ${considered} task(s)${scope}. ` +
    `Tell me which status, area, or priority matters most and I'll narrow it down.`
  );
}

/**
 * Cheaper read-only path for task prioritization. The LLM interprets the request
 * and formats the response; TypeScript owns ClickUp traversal and Jev policy.
 */
export async function runClickUpPriorityWorkflow(
  intent: ClickUpPriorityIntent,
  scopeNote: string | null = null,
): Promise<SessionResult> {
  const prioritySlug = slugGoal(intent.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-priority-${prioritySlug}`)
      : null;
  const intentId = `intent-clickup-priority-${prioritySlug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: intent.intent,
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-priority-${prioritySlug}`,
        steps: [
          {
            stepId: "discover-clickup-scope",
            description:
              "Traverse the requested ClickUp scope with validated IDs.",
            order: 0,
            dependsOn: [],
          },
          {
            stepId: "evaluate-priority",
            description:
              "Use Jev to prioritize the retrieved ClickUp tasks or abstain.",
            order: 1,
            dependsOn: ["discover-clickup-scope"],
          },
          {
            stepId: "format-priority-result",
            description:
              "Format the validated task-priority result for the user.",
            order: 2,
            dependsOn: ["evaluate-priority"],
          },
        ],
      })
    : null;

  try {
    await executable?.startStep?.("discover-clickup-scope");
    const tasks = await collectScopedTasks(intent);
    await executable?.completeStep?.("discover-clickup-scope");

    await executable?.startStep?.("evaluate-priority");
    const decision = await prioritizeTasks(intent, tasks, scopeNote);
    await executable?.completeStep?.("evaluate-priority");

    if (decision.outcome === "abstain") {
      // Nothing to format: the records did not support a recommendation, so a model round trip
      // would only hedge about a decision that was never made. The step still has to run through
      // the FSM (`pending -> running -> completed`), which costs no model call.
      await executable?.startStep?.("format-priority-result");
      await executable?.completeStep?.("format-priority-result");
      await executable?.finish?.();
      return {
        message: textReply(
          describePriorityAbstain(decision, intentStatuses(intent)),
        ),
      };
    }

    await executable?.startStep?.("format-priority-result");
    const formatted = await FormatClickUpPrioritization({
      intent: intent.intent,
      decision_json: JSON.stringify(decision),
    });
    await executable?.completeStep?.("format-priority-result");
    await executable?.finish?.();
    return {
      message: finalResponseToStructured({
        ...formatted,
        message: enforceValidatedRecommendation(formatted.message, decision),
      }),
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return {
      message: textReply(`I could not prioritize those tasks. ${reason}`),
    };
  }
}

/**
 * Deterministic read path for task listing and counting.
 *
 * The model classifies the request (`action`, `status_filter`, `scope_hint`) and never decides
 * whether traversal is complete: TypeScript walks teams → spaces → lists → tasks, filters by
 * status, and renders the answer from the validated records. This removes the failure mode where
 * a step-scoped executor stops after navigation and reports "no tasks".
 */
export async function runClickUpTaskQueryWorkflow(
  intent: ClickUpIntent,
): Promise<SessionResult> {
  const action = intent.action === "count_tasks" ? "count_tasks" : "list_tasks";
  const filters = taskFiltersFromIntent(intent);
  const querySlug = slugGoal(intent.intent);

  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-query-${querySlug}`)
      : null;
  const intentId = `intent-clickup-query-${querySlug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: intent.intent,
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-query-${querySlug}`,
        steps: [
          {
            stepId: "discover-clickup-scope",
            description:
              "Traverse the requested ClickUp scope with validated IDs.",
            order: 0,
            dependsOn: [],
          },
          {
            stepId: "filter-tasks",
            description: hasTaskFilters(filters)
              ? `Keep tasks${describeTaskFilters(filters)}.`
              : "Keep every retrieved task.",
            order: 1,
            dependsOn: ["discover-clickup-scope"],
          },
          {
            stepId: "format-task-list",
            description:
              action === "count_tasks"
                ? "Render the validated task count."
                : "Render the validated task list.",
            order: 2,
            dependsOn: ["filter-tasks"],
          },
        ],
      })
    : null;

  let tasks: ClickUpTaskSummary[];
  try {
    await executable?.startStep?.("discover-clickup-scope");
    tasks = await collectScopedTasksForRead(intent, filters);
    await executable?.completeStep?.("discover-clickup-scope");
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original failure is more useful than a best-effort abort failure.
    }
    return { message: textReply(`I could not read those tasks. ${reason}`) };
  }

  await executable?.startStep?.("filter-tasks");
  const { matching, statusMiss } = applyTaskFilters(tasks, filters);
  await executable?.completeStep?.("filter-tasks");

  await executable?.startStep?.("format-task-list");
  const message = renderTaskQueryAnswer(
    action,
    matching,
    filters,
    tasks,
    statusMiss,
  );
  await executable?.completeStep?.("format-task-list");
  await executable?.finish?.();
  return { message: textReply(message) };
}

/**
 * Largest bulk update applied without an explicit id before the host refuses and asks to narrow.
 *
 * A filter-selected update is powerful: "move the in-progress tasks to to do" could rewrite dozens
 * of tasks. A known id is exact and bypasses the cap; a filter match beyond this is treated as too
 * broad to apply blindly, so the host asks for a narrower scope instead of mass-mutating.
 */
const MAX_BULK_UPDATE = 10;

/** One task's outcome in a deterministic update pass. */
type UpdateOutcome =
  | { kind: "updated"; task: ClickUpTaskSummary }
  | { kind: "unchanged"; task: ClickUpTaskSummary }
  | { kind: "failed"; task: ClickUpTaskSummary; reason: string };

/**
 * Deterministic status/priority update path.
 *
 * TypeScript owns resolution and execution: a known id goes straight to `update_task`; otherwise the
 * trusted traversal retrieves the scope, typed filters select the tasks (exact status first), and
 * every match is updated. No intent classification, no `PlanClickUpWork`, and no `ChooseClickUpAction`
 * ReAct loop — the model is not on this path at all.
 */
export async function runClickUpWriteWorkflow(
  spec: ClickUpUpdateSpec,
): Promise<SessionResult> {
  const slug = slugGoal(spec.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-update-${slug}`)
      : null;
  const intentId = `intent-clickup-update-${slug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({ intentId, description: spec.intent })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-update-${slug}`,
        steps: [
          {
            stepId: "resolve-update-targets",
            description: spec.task_id
              ? "Validate the named task id."
              : "Traverse the requested ClickUp scope and select the tasks to update.",
            order: 0,
            dependsOn: [],
          },
          {
            stepId: "apply-updates",
            description: "Apply the status/priority change to each selected task.",
            order: 1,
            dependsOn: ["resolve-update-targets"],
          },
          {
            stepId: "format-update-result",
            description: "Confirm the applied changes.",
            order: 2,
            dependsOn: ["apply-updates"],
          },
        ],
      })
    : null;

  try {
    await executable?.startStep?.("resolve-update-targets");
    const resolution = await resolveWriteTargets(spec, "update");
    await executable?.completeStep?.("resolve-update-targets");

    // Nothing matched, or the filter was too broad to apply without an id: answer deterministically
    // without touching ClickUp. These still cycle the remaining steps through the FSM (no model call).
    if (resolution.kind !== "targets") {
      await executable?.startStep?.("apply-updates");
      await executable?.completeStep?.("apply-updates");
      await executable?.startStep?.("format-update-result");
      await executable?.completeStep?.("format-update-result");
      await executable?.finish?.();
      return { message: textReply(resolution.message) };
    }

    await executable?.startStep?.("apply-updates");
    const outcomes: UpdateOutcome[] = [];
    for (const task of resolution.targets) {
      try {
        outcomes.push(await applyTaskUpdate(task, spec));
      } catch (error) {
        outcomes.push({
          kind: "failed",
          task,
          reason: error instanceof Error ? error.message : String(error),
        });
      }
    }
    await executable?.completeStep?.("apply-updates");

    await executable?.startStep?.("format-update-result");
    const message = renderUpdateConfirmation(outcomes, spec);
    await executable?.completeStep?.("format-update-result");
    await executable?.finish?.();
    return { message: textReply(message) };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return { error: `ClickUp agent error: ${reason}` };
  }
}

type UpdateResolution =
  | { kind: "targets"; targets: ClickUpTaskSummary[] }
  | { kind: "message"; message: string };

/** Resolve the tasks a write applies to: exact id, or typed-filter selection over the scope. */
async function resolveWriteTargets(
  spec: Pick<ClickUpUpdateSpec, "intent" | "task_id" | "filters" | "scope_hint">,
  verb: "update" | "delete",
): Promise<UpdateResolution> {
  if (!spec.task_id && !hasTaskFilters(spec.filters)) {
    return {
      kind: "message",
      message: `Tell me which ClickUp task or task set to ${verb}; I did not change anything.`,
    };
  }

  if (spec.task_id) {
    const out = await callClickUp({ task_id: spec.task_id });
    const task = (out.tasks ?? [])[0];
    if (!task) {
      return {
        kind: "message",
        message: `No ClickUp task was found with id ${spec.task_id}.`,
      };
    }
    return { kind: "targets", targets: [task] };
  }

  const scopeIntent: ClickUpPriorityIntent = {
    kind: "prioritize",
    intent: spec.intent,
    scope: spec.scope_hint?.trim() ? "one_best_match" : "all_accessible",
    scope_hint: spec.scope_hint ?? null,
    status_filter: null,
    statuses: null,
  };
  const scopeTasks = await collectScopedTasks(scopeIntent, READ_TRAVERSAL);
  const { matching, statusMiss } = applyWriteTaskFilters(scopeTasks, spec.filters);

  if (matching.length === 0) {
    const label = describeTaskFilters(spec.filters);
    if (spec.filters.statuses.length > 0 && statusMiss && scopeTasks.length > 0) {
      const statuses = [...new Set(scopeTasks.map((t) => t.status))].sort();
      return {
        kind: "message",
        message:
          `No ClickUp tasks${label} were found to ${verb}. ` +
          `The ${scopeTasks.length} task(s) retrieved have these statuses: ${statuses.join(", ")}.`,
      };
    }
    return {
      kind: "message",
      message: `No ClickUp tasks${label} were found to ${verb}.`,
    };
  }

  // Exact-name gate: substring matching only discovers candidates. A write by name requires at
  // least one exact (case-insensitive) name, even when the substring produced just one candidate.
  // Apply to all exact duplicates, but ask rather than guess on a partial match. A status-only
  // selection is a deliberate bulk and remains unchanged.
  let targets = matching;
  if (spec.filters.text !== null) {
    const wanted = spec.filters.text.trim().toLocaleLowerCase();
    const exact = matching.filter(
      (task) => task.name.trim().toLocaleLowerCase() === wanted,
    );
    if (exact.length === 0) {
      return { kind: "message", message: describeAmbiguousMatches(matching, spec.filters, verb) };
    }
    targets = exact;
  }

  if (targets.length > MAX_BULK_UPDATE) {
    const label = describeTaskFilters(spec.filters);
    return {
      kind: "message",
      message:
        `${targets.length} ClickUp tasks${label} match — too many to ${verb} at once without naming them. ` +
        `Narrow the request (a specific task, name, or tighter filter) and I'll apply the change.`,
    };
  }

  return { kind: "targets", targets };
}

/**
 * Ask which task the user meant when a name matched several but none exactly.
 *
 * Lists the candidates with ids so the next turn can name the exact task or pass its id, which takes
 * the no-traversal fast path. Deterministic — no model call.
 */
function describeAmbiguousMatches(
  candidates: ClickUpTaskSummary[],
  filters: ClickUpTaskFilters,
  verb: "update" | "delete",
): string {
  const label = describeTaskFilters(filters);
  const shown = candidates.slice(0, MAX_BULK_UPDATE);
  const lines = shown.map(
    (task) => `- ${task.name} [${task.status}] (id: ${task.id})`,
  );
  const more =
    candidates.length > shown.length
      ? `\n…and ${candidates.length - shown.length} more.`
      : "";
  return (
    `${candidates.length} ClickUp task(s)${label} matched only part of that name, so I did not change anything. ` +
    `Reply with an exact task name or id and I'll ${verb} it:\n\n` +
    lines.join("\n") +
    more
  );
}

/** Replies that authorize a delete. Anything else cancels, so an unclear answer never deletes. */
const DELETE_CONFIRMATIONS = new Set([
  "y",
  "yes",
  "yes please",
  "yes delete",
  "yes delete it",
  "delete",
  "delete it",
  "confirm",
  "confirmed",
  "go ahead",
  "proceed",
  "do it",
]);

/** True only when the user's whole reply is an explicit confirmation. */
export function isDeleteConfirmation(reply: string): boolean {
  const normalized = reply
    .toLocaleLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
  return DELETE_CONFIRMATIONS.has(normalized);
}

type DeleteResolution =
  | { kind: "target"; task: ClickUpTaskSummary }
  | { kind: "message"; message: string };

/** Resolve a delete to exactly one task. Unlike an update, a delete never applies to a bulk match. */
async function resolveDeleteTarget(
  spec: ClickUpDeleteSpec,
): Promise<DeleteResolution> {
  if (!spec.task_id && !spec.filters.text) {
    return {
      kind: "message",
      message: "Tell me the name or id of the single ClickUp task to delete; I did not delete anything.",
    };
  }
  const resolution = await resolveWriteTargets(spec, "delete");
  if (resolution.kind === "message") return resolution;
  if (resolution.targets.length > 1) {
    return {
      kind: "message",
      message: describeAmbiguousMatches(resolution.targets, spec.filters, "delete"),
    };
  }
  return { kind: "target", task: resolution.targets[0]! };
}

/**
 * Deterministic delete path: find one task, ask the user, delete only on an explicit yes.
 *
 * The confirmation lives in this suspended turn, so the task the user approved is the task that gets
 * deleted. No model call decides the target or interprets the answer. `awaitReply` suspends the turn
 * until the user responds and returns their text.
 */
export async function runClickUpDeleteWorkflow(
  spec: ClickUpDeleteSpec,
  awaitReply: (prompt: string) => Promise<string>,
): Promise<SessionResult> {
  const slug = slugGoal(spec.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-delete-${slug}`)
      : null;
  const intentId = `intent-clickup-delete-${slug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({ intentId, description: spec.intent })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-delete-${slug}`,
        steps: [
          {
            stepId: "resolve-delete-target",
            description: "Find the single task to delete.",
            order: 0,
            dependsOn: [],
          },
          {
            stepId: "confirm-delete",
            description: "Ask the user to confirm the deletion.",
            order: 1,
            dependsOn: ["resolve-delete-target"],
          },
          {
            stepId: "apply-delete",
            description: "Delete the confirmed task.",
            order: 2,
            dependsOn: ["confirm-delete"],
          },
        ],
      })
    : null;

  /** Cycle the steps that have nothing to do, then answer without touching ClickUp. */
  const finishWithoutDeleting = async (
    remainingSteps: string[],
    message: string,
  ): Promise<SessionResult> => {
    for (const stepId of remainingSteps) {
      await executable?.startStep?.(stepId);
      await executable?.completeStep?.(stepId);
    }
    await executable?.finish?.();
    return { message: textReply(message) };
  };

  try {
    await executable?.startStep?.("resolve-delete-target");
    const resolution = await resolveDeleteTarget(spec);
    await executable?.completeStep?.("resolve-delete-target");
    if (resolution.kind === "message") {
      return await finishWithoutDeleting(
        ["confirm-delete", "apply-delete"],
        resolution.message,
      );
    }
    const { task } = resolution;

    await executable?.startStep?.("confirm-delete");
    const reply = await awaitReply(
      `Permanently delete ClickUp task "${task.name}" [${task.status}] (id: ${task.id})? ` +
        `Reply "yes" to delete it; anything else cancels.`,
    );
    await executable?.completeStep?.("confirm-delete");
    if (!isDeleteConfirmation(reply)) {
      return await finishWithoutDeleting(
        ["apply-delete"],
        `Cancelled: task ${task.id} (${task.name}) was not deleted.`,
      );
    }

    await executable?.startStep?.("apply-delete");
    await callClickUp({ task_id: task.id, confirm_delete: true });
    await executable?.completeStep?.("apply-delete");
    await executable?.finish?.();
    return { message: textReply(`Deleted task ${task.id} (${task.name}).`) };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return { error: `ClickUp agent error: ${reason}` };
  }
}

/** A single new task the host can create without a plan or a ReAct loop. */
export interface ClickUpCreateSpec {
  intent: string;
  name: string;
  description: string | null;
  priority: "urgent" | "high" | "normal" | "low" | null;
  scope_hint: string | null;
}

/**
 * Build a create spec from a classified intent.
 *
 * Returns null when the intent is not a single create, names no task, or asks for something a new
 * task cannot carry. The caller then plans, which is the slow but correct path.
 */
export function clickUpCreateSpecFromIntent(
  intent: ClickUpIntent,
): ClickUpCreateSpec | null {
  if (intent.operation_kind !== "write" || intent.action !== "create_task") return null;
  if ((intent.unsupported ?? []).length > 0) return null;
  const name = optionalText(intent.task_name);
  if (name === null) return null;
  return {
    intent: intent.intent,
    name,
    description: optionalText(intent.task_description),
    priority: normalizedChoice(intent.task_priority, PRIORITY_FILTERS),
    scope_hint: optionalText(intent.scope_hint),
  };
}

/** Lists a person's words refer to: an exact id or name when there is one, otherwise a name match. */
export function matchingLists(lists: ClickUpItem[], text: string): ClickUpItem[] {
  const wanted = text.trim().toLocaleLowerCase();
  if (wanted.length === 0) return [];
  const exact = exactMatchingLists(lists, text);
  if (exact.length > 0) return exact;
  return lists.filter((list) => list.name.toLocaleLowerCase().includes(wanted));
}

/** Exact list choices accepted after the workflow asks the user to choose. */
export function exactMatchingLists(lists: ClickUpItem[], text: string): ClickUpItem[] {
  const raw = text.trim();
  const wanted = raw.toLocaleLowerCase();
  if (wanted.length === 0) return [];
  return lists.filter(
    (list) => list.id === raw || list.name.trim().toLocaleLowerCase() === wanted,
  );
}

/**
 * The list a free-form answer picks after the workflow asks the user to choose.
 *
 * An exact id or full name wins outright. Otherwise the answer must *mention* exactly one option —
 * its id appears verbatim, or its full name appears as a substring — so a natural reply like
 * "Project 2 in clickup" resolves to "Project 2", while a reply that fits two options (or none)
 * stays ambiguous and the caller asks again rather than guessing. Still fully deterministic: no
 * model interprets the answer.
 */
export function listChosenInReply(options: ClickUpItem[], text: string): ClickUpItem[] {
  const exact = exactMatchingLists(options, text);
  if (exact.length > 0) return exact;
  const raw = text.trim();
  const wanted = raw.toLocaleLowerCase();
  if (wanted.length === 0) return [];
  const mentioned = options.filter(
    (list) => raw.includes(list.id) || wanted.includes(list.name.trim().toLocaleLowerCase()),
  );
  if (mentioned.length <= 1) return mentioned;
  // Nested names (e.g. "Backend" inside "Backend ops") both match a reply naming the longer one.
  // Prefer the most specific: drop any match whose name is wholly contained in another match's name.
  const names = mentioned.map((list) => list.name.trim().toLocaleLowerCase());
  const specific = mentioned.filter(
    (_list, i) =>
      !names.some((other, j) => j !== i && other.length > names[i]!.length && other.includes(names[i]!)),
  );
  return specific.length > 0 ? specific : mentioned;
}

/**
 * Resolve the one list a create should write to, asking the user when it is not already unambiguous.
 *
 * `label` names what is being placed (a quoted task name, or "the N new tasks") and only shapes the
 * question. The hint decides it outright when it names exactly one list; otherwise the user is asked
 * and given one more try on an unclear answer, matching the tolerant reply rules. Returns null when
 * the user never names a single list, so the caller can cancel without creating anything.
 */
async function resolveTargetList(
  lists: ClickUpItem[],
  scopeHint: string | null,
  label: string,
  awaitReply: (prompt: string) => Promise<string>,
): Promise<ClickUpItem | null> {
  const matches = scopeHint ? matchingLists(lists, scopeHint) : lists;
  if (matches.length === 1) return matches[0]!;
  const options = matches.length > 1 ? matches : lists;
  const optionList = options.map((list) => `${list.name} (id: ${list.id})`).join(", ");
  let chosen: ClickUpItem[] = [];
  for (let attempt = 0; attempt < 2 && chosen.length !== 1; attempt++) {
    const lead = attempt > 0
      ? `I couldn't tell which list you meant. `
      : scopeHint && matches.length === 0
        ? `I could not find a ClickUp list matching "${scopeHint}". `
        : "";
    const reply = await awaitReply(
      `${lead}Which ClickUp list should ${label} go in? ` +
        `Options: ${optionList}. Reply with a list name or id.`,
    );
    chosen = listChosenInReply(options, reply);
  }
  return chosen.length === 1 ? chosen[0]! : null;
}

/** Trim, drop empties, and de-duplicate (case-insensitively) a model-supplied list of task names. */
export function cleanNameList(values: unknown): string[] {
  if (!Array.isArray(values)) return [];
  const seen = new Set<string>();
  const out: string[] = [];
  for (const value of values) {
    const name = optionalText(value);
    if (name === null) continue;
    const key = name.toLocaleLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(name);
  }
  return out;
}

/**
 * Deterministic create path: find the list, then create the task. No model call picks the list.
 *
 * With one list and no hint the list is obvious. Otherwise a hint that names exactly one list
 * decides it, and anything still unclear is asked once, because a task created in the wrong list
 * is worse than a question.
 */
export async function runClickUpCreateWorkflow(
  spec: ClickUpCreateSpec,
  awaitReply: (prompt: string) => Promise<string>,
): Promise<SessionResult> {
  const slug = slugGoal(spec.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-create-${slug}`)
      : null;
  const intentId = `intent-clickup-create-${slug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({ intentId, description: spec.intent })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-create-${slug}`,
        steps: [
          { stepId: "resolve-list", description: "Find the list for the new task.", order: 0, dependsOn: [] },
          { stepId: "create-task", description: "Create the task.", order: 1, dependsOn: ["resolve-list"] },
        ],
      })
    : null;

  /** Close the unused create step and answer without touching ClickUp. */
  const finishWithoutCreating = async (message: string): Promise<SessionResult> => {
    await executable?.completeStep?.("resolve-list");
    await executable?.startStep?.("create-task");
    await executable?.completeStep?.("create-task");
    await executable?.finish?.();
    return { message: textReply(message) };
  };

  try {
    await executable?.startStep?.("resolve-list");
    const lists = await collectAllLists(spec.scope_hint);
    if (lists.length === 0) {
      return await finishWithoutCreating(
        `I found no ClickUp lists to put "${spec.name}" in, so I did not create it.`,
      );
    }

    const list = await resolveTargetList(lists, spec.scope_hint, `"${spec.name}"`, awaitReply);
    if (!list) {
      return await finishWithoutCreating(`Cancelled: "${spec.name}" was not created.`);
    }
    await executable?.completeStep?.("resolve-list");

    await executable?.startStep?.("create-task");
    const out = await callClickUp({
      list_id: list.id,
      name: spec.name,
      ...(spec.description !== null ? { description: spec.description } : {}),
      ...(spec.priority !== null ? { priority: PRIORITY_TO_NUMBER[spec.priority] } : {}),
    });
    await executable?.completeStep?.("create-task");
    await executable?.finish?.();

    const task = (out.tasks ?? [])[0];
    const reference = task ? ` (id: ${task.id})${task.url ? ` ${task.url}` : ""}` : "";
    return { message: textReply(`Created task "${spec.name}"${reference} in list ${list.name}.`) };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return { error: `ClickUp agent error: ${reason}` };
  }
}

/** Several new tasks to create in one list — the deterministic fan-out of a "create one per X" ask. */
export interface ClickUpMultiCreateSpec {
  intent: string;
  names: string[];
  description: string | null;
  priority: "urgent" | "high" | "normal" | "low" | null;
  scope_hint: string | null;
}

/** Build a multi-create spec from the specialist's classification; null when it is not a batch create. */
export function clickUpMultiCreateSpecFromIntent(
  intent: ClickUpIntent,
): ClickUpMultiCreateSpec | null {
  if (intent.operation_kind !== "write" || intent.action !== "create_tasks") return null;
  if ((intent.unsupported ?? []).length > 0) return null;
  const names = cleanNameList(intent.task_names);
  if (names.length === 0) return null;
  return {
    intent: intent.intent,
    names,
    description: optionalText(intent.task_description),
    priority: normalizedChoice(intent.task_priority, PRIORITY_FILTERS),
    scope_hint: optionalText(intent.scope_hint),
  };
}

/**
 * Deterministic batch create: resolve the list once, then create every named task in a loop.
 *
 * This is the fan-out the model-driven walk could not do reliably — its single write-confirmation
 * gate stopped after the first task. Here the loop is owned by code, so N names always yield N
 * attempts, each reported, and a shared description/priority applies to all.
 */
export async function runClickUpMultiCreateWorkflow(
  spec: ClickUpMultiCreateSpec,
  awaitReply: (prompt: string) => Promise<string>,
): Promise<SessionResult> {
  const slug = slugGoal(spec.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-create-many-${slug}`)
      : null;
  const intentId = `intent-clickup-create-many-${slug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({ intentId, description: spec.intent })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-create-many-${slug}`,
        steps: [
          { stepId: "resolve-list", description: "Find the list for the new tasks.", order: 0, dependsOn: [] },
          { stepId: "create-tasks", description: "Create each task.", order: 1, dependsOn: ["resolve-list"] },
        ],
      })
    : null;

  const finishWithoutCreating = async (message: string): Promise<SessionResult> => {
    await executable?.completeStep?.("resolve-list");
    await executable?.startStep?.("create-tasks");
    await executable?.completeStep?.("create-tasks");
    await executable?.finish?.();
    return { message: textReply(message) };
  };

  try {
    await executable?.startStep?.("resolve-list");
    const lists = await collectAllLists(spec.scope_hint);
    if (lists.length === 0) {
      return await finishWithoutCreating(
        `I found no ClickUp lists to create ${spec.names.length} task(s) in, so I created nothing.`,
      );
    }
    const label = spec.names.length === 1 ? `"${spec.names[0]}"` : `the ${spec.names.length} new tasks`;
    const list = await resolveTargetList(lists, spec.scope_hint, label, awaitReply);
    if (!list) {
      return await finishWithoutCreating("Cancelled: no tasks were created.");
    }
    await executable?.completeStep?.("resolve-list");

    await executable?.startStep?.("create-tasks");
    const created: string[] = [];
    const failed: string[] = [];
    for (const name of spec.names) {
      try {
        const out = await callClickUp({
          list_id: list.id,
          name,
          ...(spec.description !== null ? { description: spec.description } : {}),
          ...(spec.priority !== null ? { priority: PRIORITY_TO_NUMBER[spec.priority] } : {}),
        });
        const task = (out.tasks ?? [])[0];
        created.push(task ? `${name} (id: ${task.id})` : name);
      } catch (error) {
        failed.push(`${name}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    await executable?.completeStep?.("create-tasks");
    await executable?.finish?.();

    const lines = [`Created ${created.length} of ${spec.names.length} ClickUp task(s) in list ${list.name}:`];
    for (const entry of created) lines.push(`- ${entry}`);
    if (failed.length > 0) {
      lines.push(`Failed to create ${failed.length}:`);
      for (const entry of failed) lines.push(`- ${entry}`);
    }
    return { message: textReply(lines.join("\n")) };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return { error: `ClickUp agent error: ${reason}` };
  }
}

/** Apply the spec's change to one task, skipping a status write that is already satisfied. */
async function applyTaskUpdate(
  task: ClickUpTaskSummary,
  spec: ClickUpUpdateSpec,
): Promise<UpdateOutcome> {
  const needsStatus =
    spec.target_status !== null &&
    statusKey(task.status) !== statusKey(spec.target_status);
  const needsPriority =
    spec.target_priority !== null &&
    (typeof task.priority !== "string" ||
      task.priority.trim().toLocaleLowerCase() !== spec.target_priority);

  if (!needsStatus && !needsPriority) {
    return { kind: "unchanged", task };
  }

  const input: { task_id: string; status?: string; priority?: number } = {
    task_id: task.id,
  };
  if (needsStatus && spec.target_status !== null) input.status = spec.target_status;
  if (needsPriority && spec.target_priority !== null) {
    input.priority = PRIORITY_TO_NUMBER[spec.target_priority];
  }

  const out = await callClickUp(input);
  const updated = (out.tasks ?? [])[0] ?? task;
  return { kind: "updated", task: updated };
}

/** Deterministic confirmation for an update pass — no model call. */
function renderUpdateConfirmation(
  outcomes: UpdateOutcome[],
  spec: ClickUpUpdateSpec,
): string {
  const changes: string[] = [];
  if (spec.target_status !== null) changes.push(`status to '${spec.target_status}'`);
  if (spec.target_priority !== null) changes.push(`priority to '${spec.target_priority}'`);
  const changeLabel = changes.join(" and ") || "the requested fields";

  const line = (o: UpdateOutcome): string => {
    if (o.kind === "updated") {
      return `Updated task ${o.task.id} (${o.task.name}), set ${changeLabel}.`;
    }
    if (o.kind === "failed") {
      return `Failed to update task ${o.task.id} (${o.task.name}): ${o.reason}`;
    }
    return `Task ${o.task.id} (${o.task.name}) already matched ${changeLabel}; no change needed.`;
  };

  if (outcomes.length === 1) return line(outcomes[0]!);

  const updatedCount = outcomes.filter((o) => o.kind === "updated").length;
  const failedCount = outcomes.filter((o) => o.kind === "failed").length;
  const header =
    failedCount === 0 && updatedCount === outcomes.length
      ? `Updated ${outcomes.length} ClickUp task(s), set ${changeLabel}:`
      : `Processed ${outcomes.length} ClickUp task(s): ${updatedCount} changed, ${failedCount} failed.`;
  return [header, "", ...outcomes.map((o) => `- ${line(o)}`)].join("\n");
}

/**
 * Reuse the deterministic traversal that the priority path already trusts.
 *
 * `scope_hint` is a name, never an ID, so `one_best_match` semantics (Jev disambiguation over the
 * retrieved candidates) apply whenever the user names an area.
 *
 * `status_filter` is deliberately not forwarded: filtering happens locally so a status the scope
 * does not contain can be reported against the statuses that do exist, instead of coming back empty.
 */
async function collectScopedTasksForRead(
  intent: ClickUpIntent,
  _filters: ClickUpTaskFilters,
): Promise<ClickUpTaskSummary[]> {
  const scopeIntent: ClickUpPriorityIntent = {
    kind: "prioritize",
    intent: intent.intent,
    scope: intent.scope_hint?.trim() ? "one_best_match" : "all_accessible",
    scope_hint: intent.scope_hint ?? null,
    status_filter: null,
    statuses: null,
  };
  return collectScopedTasks(scopeIntent, READ_TRAVERSAL);
}

/**
 * Filters the deterministic read path applies to the tasks it fetched.
 *
 * These come from typed intent fields, never from request wording: ClickUp statuses are
 * workspace-defined, so vocabulary matching would encode one customer's configuration.
 */
export interface ClickUpTaskFilters {
  /** Every status the conversation is about, matched case- and space-insensitively. */
  statuses: string[];
  assignee: string | null;
  priority: "urgent" | "high" | "normal" | "low" | null;
  due: "overdue" | "today" | "this_week" | "next_week" | null;
  text: string | null;
}

/** Collect the statuses an intent carries, tolerating the legacy single-status field. */
function intentStatuses(intent: ClickUpIntent | ClickUpPriorityIntent): string[] {
  const raw = intent as { statuses?: unknown };
  const fromList = Array.isArray(raw.statuses)
    ? raw.statuses
        .filter((value): value is string => typeof value === "string")
        .map((value) => value.trim())
        .filter((value) => value.length > 0)
    : [];
  if (fromList.length > 0) return fromList;
  const single = optionalText(intent.status_filter);
  return single ? [single] : [];
}

const PRIORITY_FILTERS = ["urgent", "high", "normal", "low"] as const;
const DUE_FILTERS = ["overdue", "today", "this_week", "next_week"] as const;

function optionalText(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0
    ? value.trim()
    : null;
}

function normalizedChoice<T extends string>(
  value: unknown,
  allowed: readonly T[],
): T | null {
  if (typeof value !== "string") return null;
  const normalized = value
    .trim()
    .toLocaleLowerCase()
    .replace(/[\s-]+/g, "_");
  return allowed.find((option) => option === normalized) ?? null;
}

/** Typed filters carried by the intent, normalised and tolerant of absent fields. */
export function taskFiltersFromIntent(
  intent: ClickUpIntent,
): ClickUpTaskFilters {
  const raw = intent as {
    assignee_filter?: unknown;
    priority_filter?: unknown;
    due_filter?: unknown;
    text_filter?: unknown;
  };
  return {
    statuses: intentStatuses(intent),
    assignee: optionalText(raw.assignee_filter),
    priority: normalizedChoice(raw.priority_filter, PRIORITY_FILTERS),
    due: normalizedChoice(raw.due_filter, DUE_FILTERS),
    text: optionalText(raw.text_filter),
  };
}

/** ClickUp numeric priority codes. */
const PRIORITY_TO_NUMBER: Record<"urgent" | "high" | "normal" | "low", number> = {
  urgent: 1,
  high: 2,
  normal: 3,
  low: 4,
};

/**
 * A validated status/priority update the host can execute without a plan or a ReAct loop.
 *
 * Either `task_id` names the exact task, or `filters` select tasks by their current properties.
 * Destination values stay separate from selection filters.
 */
export interface ClickUpUpdateSpec {
  intent: string;
  task_id: string | null;
  target_status: string | null;
  target_priority: "urgent" | "high" | "normal" | "low" | null;
  filters: ClickUpTaskFilters;
  scope_hint: string | null;
}

/**
 * Build a deterministic update from the ClickUp specialist's own classification.
 *
 * The coordinator only chooses this agent and one of its advertised capabilities. ClickUp owns the
 * provider-specific selector, destination values, ambiguity checks, and write limits.
 */
export function clickUpUpdateSpecFromIntent(
  intent: ClickUpIntent,
): ClickUpUpdateSpec | null {
  if (intent.operation_kind !== "write" || intent.action !== "update_task") return null;
  if ((intent.unsupported ?? []).length > 0) return null;

  const targetStatus = optionalText(intent.target_status);
  const targetPriority = normalizedChoice(intent.target_priority, PRIORITY_FILTERS);
  if (!targetStatus && !targetPriority) return null;

  const taskId = optionalText(intent.task_id);
  const originalFilters = taskFiltersFromIntent(intent);

  // A task id or name is enough to select the target. A status remembered from earlier conversation
  // can otherwise hide the named task after it changes. Keep statuses for explicit bulk selectors,
  // as in "move every urgent in-progress task to done".
  const targetKey = targetStatus ? statusKey(targetStatus) : null;
  const selectionStatuses = taskId || originalFilters.text
    ? []
    : targetKey
      ? originalFilters.statuses.filter((status) => statusKey(status) !== targetKey)
      : originalFilters.statuses;

  return {
    intent: intent.intent,
    task_id: taskId,
    target_status: targetStatus,
    target_priority: targetPriority,
    filters: { ...originalFilters, statuses: selectionStatuses },
    scope_hint: optionalText(intent.scope_hint),
  };
}

/** A validated delete of one named task, executed deterministically after user confirmation. */
export interface ClickUpDeleteSpec {
  intent: string;
  task_id: string | null;
  filters: ClickUpTaskFilters;
  scope_hint: string | null;
}

/** Build a delete spec from the ClickUp specialist's classification. */
export function clickUpDeleteSpecFromIntent(
  intent: ClickUpIntent,
): ClickUpDeleteSpec {
  return {
    intent: intent.intent,
    task_id: optionalText(intent.task_id),
    filters: { ...taskFiltersFromIntent(intent), statuses: [] },
    scope_hint: optionalText(intent.scope_hint),
  };
}

/** One or more explicitly-named tasks to delete in a single pass, no confirmation. */
export interface ClickUpMultiDeleteSpec {
  intent: string;
  task_ids: string[];
  names: string[];
  scope_hint: string | null;
}

/**
 * Build a batch-delete spec from the specialist's classification; null when it is not a `delete_tasks`.
 *
 * `delete_tasks` is the explicit path: the user named the exact tasks, so there is nothing to
 * confirm. A delete expressed only as a selection (a text or status filter, no names) stays on the
 * single confirmed path, where confirmation is the safety net for a fuzzy match.
 */
export function clickUpMultiDeleteSpecFromIntent(
  intent: ClickUpIntent,
): ClickUpMultiDeleteSpec | null {
  if (intent.operation_kind !== "delete" || intent.action !== "delete_tasks") return null;
  const names = cleanNameList(intent.task_names);
  const id = optionalText(intent.task_id);
  const task_ids = id ? [id] : [];
  if (names.length === 0 && task_ids.length === 0) return null;
  return { intent: intent.intent, task_ids, names, scope_hint: optionalText(intent.scope_hint) };
}

/**
 * Deterministic batch delete with no confirmation: each task was named explicitly by the user.
 *
 * Ids are fetched directly; names are resolved against one scoped traversal and must match a task
 * name exactly (a name that matches several is reported, not guessed, so a fuzzy name never deletes
 * the wrong task). Every resolved task is deleted in one pass and the outcome of each is reported.
 */
export async function runClickUpMultiDeleteWorkflow(
  spec: ClickUpMultiDeleteSpec,
): Promise<SessionResult> {
  const slug = slugGoal(spec.intent);
  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(`clickup-delete-many-${slug}`)
      : null;
  const intentId = `intent-clickup-delete-many-${slug}`;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({ intentId, description: spec.intent })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: `plan-clickup-delete-many-${slug}`,
        steps: [
          { stepId: "resolve-targets", description: "Find the tasks to delete.", order: 0, dependsOn: [] },
          { stepId: "apply-deletes", description: "Delete each resolved task.", order: 1, dependsOn: ["resolve-targets"] },
        ],
      })
    : null;

  try {
    await executable?.startStep?.("resolve-targets");
    const targets: ClickUpTaskSummary[] = [];
    const notFound: string[] = [];
    const ambiguous: string[] = [];

    for (const id of spec.task_ids) {
      const out = await callClickUp({ task_id: id });
      const task = (out.tasks ?? [])[0];
      if (task) targets.push(task);
      else notFound.push(`id ${id}`);
    }

    if (spec.names.length > 0) {
      const scopeIntent: ClickUpPriorityIntent = {
        kind: "prioritize",
        intent: spec.intent,
        scope: spec.scope_hint?.trim() ? "one_best_match" : "all_accessible",
        scope_hint: spec.scope_hint ?? null,
        status_filter: null,
        statuses: null,
      };
      const scopeTasks = await collectScopedTasks(scopeIntent, READ_TRAVERSAL);
      for (const name of spec.names) {
        const wanted = name.trim().toLocaleLowerCase();
        const exact = scopeTasks.filter((task) => task.name.trim().toLocaleLowerCase() === wanted);
        if (exact.length === 0) notFound.push(name);
        else if (exact.length > 1) ambiguous.push(`${name} (${exact.length} matches)`);
        else targets.push(exact[0]!);
      }
    }
    await executable?.completeStep?.("resolve-targets");

    // A task named by both id and name, or matched twice, is deleted once.
    const unique = [...new Map(targets.map((task) => [task.id, task])).values()];

    await executable?.startStep?.("apply-deletes");
    const deleted: string[] = [];
    const failed: string[] = [];
    for (const task of unique) {
      try {
        await callClickUp({ task_id: task.id, confirm_delete: true });
        deleted.push(`${task.name} (id: ${task.id})`);
      } catch (error) {
        failed.push(`${task.name} (id: ${task.id}): ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    await executable?.completeStep?.("apply-deletes");
    await executable?.finish?.();

    const lines = [
      deleted.length > 0 ? `Deleted ${deleted.length} ClickUp task(s):` : "No ClickUp tasks were deleted.",
    ];
    for (const entry of deleted) lines.push(`- ${entry}`);
    if (notFound.length > 0) lines.push(`Not found: ${notFound.join(", ")}.`);
    if (ambiguous.length > 0) lines.push(`Matched more than one, so not deleted: ${ambiguous.join(", ")}.`);
    if (failed.length > 0) {
      lines.push(`Failed to delete ${failed.length}:`);
      for (const entry of failed) lines.push(`- ${entry}`);
    }
    return { message: textReply(lines.join("\n")) };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    try {
      await executable?.abort?.(reason);
    } catch {
      // The original workflow error is more useful than a best-effort abort failure.
    }
    return { error: `ClickUp agent error: ${reason}` };
  }
}

/** True when the intent narrows the task list at all. */
export function hasTaskFilters(filters: ClickUpTaskFilters): boolean {
  return (
    filters.statuses.length > 0 ||
    filters.assignee !== null ||
    filters.priority !== null ||
    filters.due !== null ||
    filters.text !== null
  );
}

function renderTaskQueryAnswer(
  action: "list_tasks" | "count_tasks",
  tasks: ClickUpTaskSummary[],
  filters: ClickUpTaskFilters,
  scopeTasks: ClickUpTaskSummary[] = [],
  statusMiss = false,
): string {
  const filterLabel = describeTaskFilters(filters);
  if (tasks.length === 0) {
    if (filters.statuses.length > 0 && statusMiss && scopeTasks.length > 0) {
      // Statuses are workspace-defined: report the ones that exist rather than a bare "none".
      const statuses = [...new Set(scopeTasks.map((t) => t.status))].sort();
      return (
        `No ClickUp tasks${filterLabel} were found in the requested scope. ` +
        `The ${scopeTasks.length} task(s) retrieved have these statuses: ${statuses.join(", ")}.`
      );
    }
    return `No ClickUp tasks${filterLabel} were found in the requested scope.`;
  }
  if (action === "count_tasks") {
    // A count request wants the number and the distribution, not 49 table rows.
    const header = `${tasks.length} ClickUp task${tasks.length === 1 ? "" : "s"}${filterLabel}.`;
    const byStatus = new Map<string, number>();
    for (const task of tasks) {
      byStatus.set(task.status, (byStatus.get(task.status) ?? 0) + 1);
    }
    const breakdown = [...byStatus.entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .map(([status, count]) => `- ${status}: ${count}`)
      .join("\n");
    return `${header}\n\n${breakdown}`;
  }
  const header = `${tasks.length} ClickUp task${tasks.length === 1 ? "" : "s"}${filterLabel}:`;
  return `${header}\n\n${renderTaskTable(tasks)}`;
}

/** Human-readable summary of the filters that produced the answer. */
function describeTaskFilters(filters: ClickUpTaskFilters): string {
  const parts: string[] = [];
  if (filters.statuses.length > 0)
    parts.push(`with status "${filters.statuses.join('" or "')}"`);
  if (filters.assignee) parts.push(`assigned to "${filters.assignee}"`);
  if (filters.priority) parts.push(`with priority "${filters.priority}"`);
  if (filters.due) parts.push(`due ${filters.due.replace(/_/g, " ")}`);
  if (filters.text) parts.push(`matching "${filters.text}"`);
  return parts.length > 0 ? ` ${parts.join(", ")}` : "";
}

const DAY_MS = 86_400_000;

function startOfToday(): Date {
  const date = new Date();
  date.setHours(0, 0, 0, 0);
  return date;
}

function taskDueDate(task: ClickUpTaskSummary): Date | null {
  const raw = task.due_date;
  if (typeof raw !== "string") return null;
  const text = raw.trim();
  if (!text) return null;
  if (/^\d+$/.test(text)) {
    const numeric = Number(text);
    // ClickUp serialises due dates as epoch milliseconds; accept seconds too.
    const date = new Date(numeric < 1e11 ? numeric * 1000 : numeric);
    return Number.isNaN(date.getTime()) ? null : date;
  }
  const date = new Date(text);
  return Number.isNaN(date.getTime()) ? null : date;
}

function matchesDueFilter(task: ClickUpTaskSummary, due: string): boolean {
  const date = taskDueDate(task);
  if (date === null) return false;
  const today = startOfToday().getTime();
  const at = date.getTime();
  switch (due) {
    case "overdue":
      return at < today;
    case "today":
      return at >= today && at < today + DAY_MS;
    case "this_week":
      return at >= today && at < today + 7 * DAY_MS;
    case "next_week":
      return at >= today + 7 * DAY_MS && at < today + 14 * DAY_MS;
    default:
      return true;
  }
}

/**
 * Apply the intent's typed filters to the fetched task list.
 *
 * Status matching is exact first, then substring in either direction, because workspaces name
 * statuses freely ("In Progress", "In Progress (Dev)", "Scoping", "Blocked / External") and the
 * caller asked in their own words. When nothing matches, `statusMiss` tells the renderer to list
 * the statuses that do exist instead of claiming there are no tasks.
 */
function applyTaskFiltersInternal(
  tasks: ClickUpTaskSummary[],
  filters: ClickUpTaskFilters,
  includeDescriptions: boolean,
): { matching: ClickUpTaskSummary[]; statusMiss: boolean } {
  let statusMiss = false;
  let matching = tasks;

  if (filters.statuses.length > 0) {
    const wanted = filters.statuses.map(statusKey).filter((key) => key.length > 0);
    const exact = tasks.filter((task) =>
      wanted.includes(statusKey(task.status)),
    );
    if (exact.length > 0) {
      matching = exact;
    } else {
      // Workspace-defined statuses: fall back to containment before reporting a miss.
      const observed = tasks.filter((task) => {
        const status = statusKey(task.status);
        return wanted.some(
          (value) => status.includes(value) || value.includes(status),
        );
      });
      matching = observed;
      statusMiss = observed.length === 0;
    }
  }
  if (filters.assignee) {
    const wanted = filters.assignee.toLocaleLowerCase();
    matching = matching.filter((task) =>
      (Array.isArray(task.assignees) ? task.assignees : []).some((assignee) =>
        assignee.toLocaleLowerCase().includes(wanted),
      ),
    );
  }
  if (filters.priority) {
    const wanted = filters.priority;
    matching = matching.filter(
      (task) =>
        typeof task.priority === "string" &&
        task.priority.trim().toLocaleLowerCase() === wanted,
    );
  }
  if (filters.due) {
    matching = matching.filter((task) => matchesDueFilter(task, filters.due!));
  }
  if (filters.text) {
    const wanted = filters.text.toLocaleLowerCase();
    matching = matching.filter((task) => {
      const searchable = includeDescriptions
        ? `${task.name} ${task.description ?? ""}`
        : task.name;
      return searchable.toLocaleLowerCase().includes(wanted);
    });
  }
  return { matching, statusMiss };
}

/** Read filters search task names and descriptions. */
export function applyTaskFilters(
  tasks: ClickUpTaskSummary[],
  filters: ClickUpTaskFilters,
): { matching: ClickUpTaskSummary[]; statusMiss: boolean } {
  return applyTaskFiltersInternal(tasks, filters, true);
}

/** Write selectors search names only; description text can never select a task to mutate. */
function applyWriteTaskFilters(
  tasks: ClickUpTaskSummary[],
  filters: ClickUpTaskFilters,
): { matching: ClickUpTaskSummary[]; statusMiss: boolean } {
  return applyTaskFiltersInternal(tasks, filters, false);
}

function renderTaskTable(tasks: ClickUpTaskSummary[]): string {
  const header = "| Task | Status | Priority | Assignees | Due | ID |";
  const divider = "| --- | --- | --- | --- | --- | --- |";
  const rows = tasks.map((task) => {
    const name = task.url
      ? `[${escapeTableCell(task.name)}](${task.url})`
      : escapeTableCell(task.name);
    const assignees =
      Array.isArray(task.assignees) && task.assignees.length > 0
        ? task.assignees.map(escapeTableCell).join(", ")
        : "—";
    return `| ${name} | ${escapeTableCell(task.status)} | ${escapeTableCell(
      task.priority ?? "—",
    )} | ${assignees} | ${escapeTableCell(
      task.due_date ?? "—",
    )} | ${escapeTableCell(task.id)} |`;
  });
  return [header, divider, ...rows].join("\n");
}

function escapeTableCell(value: string): string {
  return value.replace(/\|/g, "\\|").replace(/\n+/g, " ").trim();
}

/**
 * Commit intent/plan to provenance and run clickup-execute → clickup-format steps.
 * Call from chat (`index.ts`) or dispatch (`withTask` + unit-scoped session label).
 */
export async function runClickUpStructuredPlan(
  sessionLabel: string,
  structured: ClickUpStructuredPlan,
  validatedIntent: string,
  operationKind: string,
  steps: ClickUpPlanStep[],
): Promise<SessionResult> {
  const goal = structured.objective.trim() || validatedIntent;
  const intentSlug = slugGoal(structured.intent_description || goal);

  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(sessionLabel)
      : null;
  const intentId = "intent-clickup-" + intentSlug;
  const citations = filterPlanCitations(structured);
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: structured.intent_description || goal,
        ...(citations.length > 0 ? { citations } : {}),
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-clickup-" + intentSlug,
        steps: steps.map((s, i) => ({
          stepId: "step-" + i,
          description: s.sub_message,
          order: i,
          dependsOn: i > 0 ? ["step-" + (i - 1)] : [],
        })),
      })
    : null;

  const allStepOutputsNested: unknown[][] = [];
  let priorResultsText: string | null = null;
  // Evidence the whole plan must produce before a terminal answer is accepted.
  const requiredEvidence: ExpectedEvidence[] = [];

  try {
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i]!;
      const stepId = "step-" + i;
      const pkg = step.agent_package.trim().toLowerCase();

      if (executable) await executable.startStep?.(stepId);

      if (pkg === PKG_CLICKUP_EXECUTE) {
        const run = await runGeneratedStepExecutor(
          "ChooseClickUpAction",
          {
            goal,
            step_description: step.sub_message,
            operation_kind: operationKind,
            prior_results: priorResultsText,
          },
          { max_steps: MAX_REACT_STEPS },
        );
        if (run.outcome !== "completed") {
          if (run.outcome === "agent_correctable") {
            throw new Error(`[${run.recovery.code}] ${run.recovery.mistake}`);
          }
          throw new Error(run.message);
        }

        // `completed` only means the loop terminated. Check the step produced the evidence its
        // plan row declared before recording progress or letting the answer stand.
        const evidence = collectStepEvidence(run.steps);
        if (!evidenceSatisfies(step.expects, evidence)) {
          throw new Error(
            `incomplete-execution: step "${step.sub_message}" required ` +
              `${describeEvidence(step.expects)} but produced ` +
              `${describeObserved(evidence)}`,
          );
        }

        allStepOutputsNested.push(run.steps);
        // Accumulate instead of replacing: a later step must still see what earlier steps retrieved.
        priorResultsText = appendPriorResults(
          priorResultsText,
          collectStepResultsForPriorContext(run.steps),
        );
        if (step.expects !== "none") requiredEvidence.push(step.expects);
        if (executable) await executable.completeStep?.(stepId);
      } else if (pkg === PKG_CLICKUP_FORMAT) {
        const required = strongestEvidence(requiredEvidence);
        const outcome = extractFinalMessage(
          allStepOutputsNested.flat(),
          required,
        );
        if (outcome.kind === "incomplete") {
          throw new Error(`incomplete-execution: ${outcome.reason}`);
        }
        if (executable) await executable.completeStep?.(stepId);
        if (executable) await executable.finish?.();
        return { message: outcome.reply };
      } else {
        if (executable) await executable.completeStep?.(stepId);
      }
    }

    if (executable) await executable.finish?.();
    return {
      message: textReply("ClickUp plan completed without a format step."),
    };
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    try {
      if (executable) await executable.abort?.(errMsg);
    } catch {
      /* best-effort */
    }
    if (errMsg.startsWith("incomplete-execution:")) {
      return {
        error:
          `ClickUp agent error: ${errMsg}. The request was not completed; no partial answer was returned.`,
      };
    }
    return { error: `ClickUp agent error: ${errMsg}` };
  }
}

/** Keep a bounded, append-only prior-results buffer across executor loops. */
function appendPriorResults(previous: string | null, next: string): string {
  const combined = previous?.trim()
    ? `${previous}\n\n---\n\n${next}`
    : next;
  if (combined.length <= PRIOR_RESULTS_MAX_CHARS) return combined;
  // Drop from the front so the newest evidence survives the cap.
  return `${combined.slice(combined.length - PRIOR_RESULTS_MAX_CHARS)}`;
}

/** Strongest evidence requirement across a plan's execute steps. */
function strongestEvidence(
  required: ExpectedEvidence[],
): ExpectedEvidence {
  if (required.includes("task_list")) return "task_list";
  if (required.includes("write_confirmation")) return "write_confirmation";
  if (required.includes("task_detail")) return "task_detail";
  if (required.includes("navigation")) return "navigation";
  return "none";
}
