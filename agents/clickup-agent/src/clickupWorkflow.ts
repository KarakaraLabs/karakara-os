/// <reference path="./baml-runtime.d.ts" />
/**
 * ClickUp plan execution — intent/plan provenance + step-executor loop.
 * Sequenced like slack-agent `runSlackStructuredPlan` / how-to-write-agents §3.6.
 */
import type {
  ClickUpInput,
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

function extractFinalMessage(steps: unknown[]): StructuredReply {
  for (const step of [...steps].reverse()) {
    if (isReadOnlyFinish(step)) return step.reply;
    if (isFinalResponse(step)) return finalResponseToStructured(step);
    const toolLike = extractToolLikePayload(step);
    if (toolLike) return textReply(formatListLikeToolPayload(toolLike));
    if (
      isJsonObject(step) &&
      typeof step.message === "string" &&
      step.message.trim()
    ) {
      return textReply(step.message.trim());
    }
  }
  if (steps.length > 0) {
    const raw = stringifyUnknown(steps[steps.length - 1], 4000);
    if (raw.trim()) return textReply(`ClickUp session produced:\n${raw}`);
  }
  return textReply("ClickUp returned no usable response for this request.");
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

function normalizedStatus(value: string): string {
  return value.trim().toLocaleLowerCase().replace(/\s+/g, " ");
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
        throw new Error(`ClickUp failed: ${step.error.code}`);
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

async function collectAllTasks(): Promise<ClickUpTaskSummary[]> {
  const teams = (await callClickUp({})).items ?? [];
  if (teams.length > MAX_ALL_TEAMS) {
    throw new Error(
      "There are too many teams to search safely. Name a workspace, space, or list.",
    );
  }

  const spaces: ClickUpItem[] = [];
  for (const team of teams) {
    spaces.push(...((await callClickUp({ team_id: team.id })).items ?? []));
    if (spaces.length > MAX_ALL_SPACES) {
      throw new Error(
        "There are too many spaces to search safely. Name a workspace, space, or list.",
      );
    }
  }

  const lists: ClickUpItem[] = [];
  for (const space of spaces) {
    lists.push(...((await callClickUp({ space_id: space.id })).items ?? []));
    if (lists.length > MAX_ALL_LISTS) {
      throw new Error(
        "There are too many lists to search safely. Name a workspace, space, or list.",
      );
    }
  }

  const tasks: ClickUpTaskSummary[] = [];
  for (const list of lists) {
    tasks.push(...((await callClickUp({ list_id: list.id })).tasks ?? []));
    if (tasks.length > MAX_ALL_TASKS) {
      throw new Error(
        "There are too many tasks to prioritize safely. Name a workspace, space, or list.",
      );
    }
  }
  return tasks;
}

async function collectScopedTasks(
  intent: ClickUpPriorityIntent,
): Promise<ClickUpTaskSummary[]> {
  if (intent.scope === "all_accessible") return collectAllTasks();

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
): Promise<PriorityDecision> {
  const requestedStatus = intent.status_filter?.trim() || null;
  const candidates = requestedStatus
    ? tasks.filter(
        (task) => normalizedStatus(task.status) === normalizedStatus(requestedStatus),
      )
    : tasks;
  if (candidates.length === 0)
    return {
      outcome: "abstain",
      reason: requestedStatus
        ? `No tasks with status ${requestedStatus} were returned from the selected scope.`
        : "No tasks were returned from the selected scope.",
    };
  if (candidates.length > MAX_ALL_TASKS) {
    return {
      outcome: "abstain",
      reason: "Too many tasks were returned to prioritize safely.",
    };
  }

  const criteria: Record<string, JsonValue> = Object.fromEntries(
    candidates.map((task) => [task.id, taskState(task)]),
  );
  criteria.none = "The task records do not support a recommendation.";
  const outcome = await invokeTool("typesafe/system_one", {
    state: {
      user_goal: intent.intent,
      status_filter: requestedStatus,
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
      reason: `Jev was unavailable: ${outcome.error.code}`,
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
      reason:
        "The available task records do not support a confident recommendation.",
      urgency,
      evidence_sufficient: evidence,
      jev: outcome.result,
    };
  }
  const task = candidates.find((candidate) => candidate.id === selected);
  if (task === undefined) {
    return {
      outcome: "abstain",
      reason: "Jev returned a task outside the retrieved candidates.",
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
 * Cheaper read-only path for task prioritization. The LLM interprets the request
 * and formats the response; TypeScript owns ClickUp traversal and Jev policy.
 */
export async function runClickUpPriorityWorkflow(
  intent: ClickUpPriorityIntent,
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
    const decision = await prioritizeTasks(intent, tasks);
    await executable?.completeStep?.("evaluate-priority");

    await executable?.startStep?.("format-priority-result");
    const formatted = await FormatClickUpPrioritization({
      intent: intent.intent,
      decision_json: JSON.stringify(decision),
    });
    await executable?.completeStep?.("format-priority-result");
    await executable?.finish?.();
    return { message: finalResponseToStructured(formatted) };
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
        allStepOutputsNested.push(run.steps);
        priorResultsText = collectStepResultsForPriorContext(run.steps);
        if (executable) await executable.completeStep?.(stepId);
      } else if (pkg === PKG_CLICKUP_FORMAT) {
        const finalMessage = extractFinalMessage(allStepOutputsNested.flat());
        if (executable) await executable.completeStep?.(stepId);
        if (executable) await executable.finish?.();
        return { message: finalMessage };
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
    return { error: `ClickUp agent error: ${errMsg}` };
  }
}
