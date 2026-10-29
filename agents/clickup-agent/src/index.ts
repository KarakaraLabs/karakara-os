/// <reference path="./baml-runtime.d.ts" />
import type { ClickUpIntent, RunContext, SessionResult } from "./baml-runtime";
import {
  clickUpIntentFromDeclaredRoute,
  clickUpPriorityIntentFromDeclaredRoute,
  declaredRouteFromMessage,
  isClickUpIntent,
  isClickUpPriorityIntent,
  isDeterministicTaskRead,
  isNeedClarification,
  isNotRelevant,
  onClickupSourceDispatch,
  parseClickUpStructuredPlanFromPlanning,
  runClickUpPriorityWorkflow,
  runClickUpStructuredPlan,
  runClickUpTaskQueryWorkflow,
  textReply,
  validateClickUpPlanForExecution,
} from "./clickupExecution";


/**
 * Plan → validate → execute for a read/write intent that is not a deterministic task query.
 *
 * Shared by the declared-route path and the classify path so both enforce the same contract.
 */
async function runPlannedIntent(
  intent: ClickUpIntent,
): Promise<SessionResult> {
  const planResult = await PlanClickUpWork({
    intent: intent.intent,
    operation_kind: intent.operation_kind,
  });
  const structured = parseClickUpStructuredPlanFromPlanning(planResult);
  if (!structured) {
    return {
      error:
        "Planning failed: PlanClickUpWork did not return a valid ClickUpStructuredPlan shape. Try rephrasing your request.",
    };
  }
  const stepsOrErr = validateClickUpPlanForExecution(
    structured,
    intent.operation_kind,
  );
  if (typeof stepsOrErr === "string") {
    return {
      error: `Planning output did not satisfy the ClickUp execution contract: ${stepsOrErr}`,
    };
  }
  return runClickUpStructuredPlan(
    `clickup-chat-${Date.now()}`,
    structured,
    intent.intent,
    intent.operation_kind,
    stepsOrErr,
  );
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const originalText =
      typeof ctx.text === "string" && ctx.text.length > 0
        ? ctx.text
        : "unknown";
    let text = originalText;

    // A calling coordinator can declare the route as a data part. When it validates, the declared
    // goal and filters are authoritative: no classifier call and no second paraphrase. Ingress,
    // human messages, and older callers send text only and keep the classify path below.
    const declaredRoute = declaredRouteFromMessage(ctx.message);
    if (declaredRoute) {
      const declaredPriority = clickUpPriorityIntentFromDeclaredRoute(declaredRoute);
      if (declaredPriority) {
        return runClickUpPriorityWorkflow(declaredPriority, declaredRoute.scope_note);
      }
      const declaredIntent = clickUpIntentFromDeclaredRoute(declaredRoute);
      if (declaredIntent) {
        if (isDeterministicTaskRead(declaredIntent)) {
          return runClickUpTaskQueryWorkflow(declaredIntent);
        }
        return runPlannedIntent(declaredIntent);
      }
    }

    let resolvedIntent: ClickUpIntent | null = null;
    while (true) {
      const intentResult = await InferClickUpIntent({});

      if (isClickUpPriorityIntent(intentResult)) {
        return runClickUpPriorityWorkflow(intentResult);
      }
      if (isClickUpIntent(intentResult)) {
        resolvedIntent = intentResult;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(
            `This doesn't look like a ClickUp request — ${intentResult.reason}`,
          ),
        };
      }
      if (isNeedClarification(intentResult)) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
        continue;
      }
      return {
        error:
          "InferClickUpIntent returned an unexpected shape — expected ClickUpIntent, NeedClarification, or NotRelevant.",
      };
    }
    if (!resolvedIntent)
      return { error: "Could not determine a valid ClickUp intent." };

    // Task listing and counting run on a deterministic traversal: the model classifies the request
    // and TypeScript owns teams → spaces → lists → tasks → status filter. Nothing about whether the
    // traversal is complete is left to a step-scoped executor decision.
    if (isDeterministicTaskRead(resolvedIntent)) {
      return runClickUpTaskQueryWorkflow(resolvedIntent);
    }

    return runPlannedIntent(resolvedIntent);
  },

  onDispatch: onClickupSourceDispatch,
});
