/// <reference path="./baml-runtime.d.ts" />
import type {
  ReplyPart,
  RunContext,
  SessionResult,
  StructuredReply,
} from "./baml-runtime";
import {
  NO_PAGES_MESSAGE,
  addStepResults,
  addToolOutput,
  answerInput,
  emptyFindings,
} from "./notionFindings";

const MAX_REACT_STEPS = 8;
const MAX_TOOL_READS = 8;

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

type NeedClarification = { question: string };
type NotRelevant = { reason: string };
type NotionIntent = { intent: string };
type NotionPlanStep = { id: string; description: string; kind: "discover" | "read" | "synthesize" };
type NotionPlan = { goal: string; steps: NotionPlanStep[] };

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function executionMessageId(message: unknown): string {
  if (isObject(message)) {
    if (typeof message.messageId === "string" && message.messageId.trim().length > 0) return message.messageId;
    if (typeof message.id === "string" && message.id.trim().length > 0) return message.id;
  }
  return "msg-notion-fallback";
}

function isNeedClarification(v: unknown): v is NeedClarification {
  return isObject(v) && typeof v.question === "string" && v.question.trim().length > 0
    && !("goal" in v) && !("steps" in v) && !("message" in v) && !("intent" in v) && !("reason" in v);
}

function isNotRelevant(v: unknown): v is NotRelevant {
  return isObject(v) && typeof v.reason === "string" && !("question" in v) && !("goal" in v) && !("intent" in v);
}

function isNotionIntent(v: unknown): v is NotionIntent {
  return isObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0
    && !("question" in v) && !("reason" in v) && !("steps" in v);
}

function normalizeUserMessage(text: string): string {
  const trimmed = text.trim();
  const notionDirective = trimmed.match(/^use\s+notion\s*[:,-]?\s*/i);
  if (!notionDirective) return trimmed;
  const withoutDirective = trimmed.slice(notionDirective[0].length).trim();
  return withoutDirective.length > 0 ? withoutDirective : trimmed;
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}

/** Read a page directly, for when the plan called for a read and the model did not make one. */
async function fetchPageBlocks(pageId: string): Promise<unknown> {
  const session = await openToolSession("support/notion");
  let finished = false;
  try {
    await session.send({ block_id: pageId } as never);
    for (let reads = 0; reads < MAX_TOOL_READS; reads++) {
      const step = await session.continue();
      if (step.status === "error") {
        throw new Error(step.error.message || `Notion failed: ${step.error.code}`);
      }
      if (step.status === "done") {
        await session.finish();
        finished = true;
        return step.output;
      }
    }
    throw new Error(`Notion did not return the page after ${MAX_TOOL_READS} reads.`);
  } finally {
    if (!finished) {
      try {
        await session.abort("page read did not complete");
      } catch {
        // Best effort. Preserve the original read failure.
      }
    }
  }
}

/** Phase 3+4: Execute the plan — open execution session, run per-step executors, synthesize. */
async function runNotionPlan(
  ctx: RunContext,
  userText: string,
  plan: NotionPlan,
): Promise<SessionResult> {
  const { goal, steps } = plan;

  // ── Open execution session with the goal-derived plan ──────────────────
  const executionSession = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("notion-" + Date.now().toString())
    : null;
  const messageId = executionMessageId(ctx.message);
  const intentId = "intent-notion-" + slugGoal(goal);
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: goal,
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-notion-" + slugGoal(goal),
        steps: steps.map((s, i) => ({
          stepId: s.id,
          description: s.description,
          order: i,
          dependsOn: i > 0 ? [steps[i - 1]!.id] : [],
        })),
      })
    : null;

  // ── Execute each tool step independently (persona pattern) ──────────────
  // Each step drives its own support/notion session. What the tool returned is collected here, so
  // the answer does not depend on the model having read the archive.
  const findings = emptyFindings();
  const toolSteps = steps.filter((s) => s.kind !== "synthesize");
  const synthesizeStep = steps.find((s) => s.kind === "synthesize");

  try {
    for (const toolStep of toolSteps) {
      if (executable) {
        await executable.startStep?.(toolStep.id);
      }

      const notionRun = await runGeneratedStepExecutor(
        "ChooseNotionAction",
        {
          goal,
          step_description: toolStep.description,
        },
        { max_steps: MAX_REACT_STEPS },
      );
      if (notionRun.outcome !== "completed") {
        throw new Error(
          notionRun.outcome === "fatal"
            ? notionRun.message
            : `[${notionRun.recovery.code}] ${notionRun.recovery.mistake}`,
        );
      }
      addStepResults(findings, notionRun.steps);

      if (executable) {
        await executable.completeStep?.(toolStep.id);
      }
    }

    // The model can end a read step without fetching anything. Notion ranks results, so read the top one.
    const topPage = findings.pages[0];
    if (steps.some((s) => s.kind === "read") && findings.contents.length === 0 && topPage) {
      addToolOutput(findings, await fetchPageBlocks(topPage.id));
    }

    if (synthesizeStep && executable) {
      await executable.startStep?.(synthesizeStep.id);
    }

    const finalMessage: StructuredReply = findings.pages.length === 0
      ? textReply(NO_PAGES_MESSAGE)
      : await AnswerFromNotion({ goal, user_message: userText, ...answerInput(findings) }) as StructuredReply;

    if (synthesizeStep && executable) {
      await executable.completeStep?.(synthesizeStep.id);
    }
    if (executable) await executable.finish?.();

    return { message: finalMessage };
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    try { if (executable) await executable.abort?.(errMsg); } catch (_) { /* best-effort */ }
    return { error: `Notion agent error: ${errMsg}` };
  }
}

__chat_register({
  run: async (ctx) => {
    const originalText = ctx.text || "unknown";
    let text = normalizeUserMessage(originalText);

    // ── Phase 1: Intent inference ────────────────────────────────────────────
    // InferNotionIntent classifies whether the message is a valid Notion query,
    // distills it into a clean intent, or asks for clarification / says not relevant.
    // Conversation history is passed via ctx.tags in the BAML prompt so the model
    // always has full context — no need to thread originalText manually.
    let validatedIntent: string | null = null;
    while (true) {
      const intentResult = await InferNotionIntent({ user_message: text });

      if (isNotionIntent(intentResult)) {
        validatedIntent = intentResult.intent;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(`This doesn't look like a Notion question — ${intentResult.reason}`),
        };
      }
      if (isNeedClarification(intentResult)) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = normalizeUserMessage(clarifiedText);
        continue;
      }
      return {
        error:
          "InferNotionIntent returned an unexpected shape — expected NotionIntent, NeedClarification, or NotRelevant.",
      };
    }
    if (!validatedIntent) return { error: "Could not determine a valid Notion intent." };

    // ── Phase 2: Planning ────────────────────────────────────────────────────
    // PlanNotionWork takes the validated intent and produces an explicit step plan.
    // No clarification needed here — intent is already resolved above.
    const resolvedPlan = await PlanNotionWork({ intent: validatedIntent });
    return runNotionPlan(ctx, text, resolvedPlan);
  },
});
