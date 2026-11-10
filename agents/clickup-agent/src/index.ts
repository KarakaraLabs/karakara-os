/// <reference path="./baml-runtime.d.ts" />
import type { ClickUpIntent, RunContext, SessionResult } from "./baml-runtime";
import {
  clickUpCreateSpecFromIntent,
  clickUpDeleteSpecFromIntent,
  clickUpMultiCreateSpecFromIntent,
  clickUpMultiDeleteSpecFromIntent,
  clickUpUpdateSpecFromIntent,
  isClickUpIntent,
  isClickUpPriorityIntent,
  isDeterministicTaskRead,
  isNeedClarification,
  isNotRelevant,
  onClickupSourceDispatch,
  parseClickUpStructuredPlanFromPlanning,
  runClickUpCreateWorkflow,
  runClickUpDeleteWorkflow,
  runClickUpMultiCreateWorkflow,
  runClickUpMultiDeleteWorkflow,
  runClickUpPriorityWorkflow,
  runClickUpStructuredPlan,
  runClickUpTaskQueryWorkflow,
  runClickUpWriteWorkflow,
  textReply,
  validateClickUpPlanForExecution,
} from "./clickupExecution";


/**
 * Plan → validate → execute for a read/write intent that is not a deterministic task query.
 *
 * Used when the specialist's classified intent does not have a deterministic workflow.
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
    const awaitReply = async (prompt: string): Promise<string> =>
      messageText(await ctx.emit.awaitInput(prompt));

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

    // The specialist owns provider-specific update fields and resolves them under the same safety
    // gates whether the request came directly from a user or through the coordinator.
    const updateSpec = clickUpUpdateSpecFromIntent(resolvedIntent);
    if (updateSpec) {
      return runClickUpWriteWorkflow(updateSpec);
    }

    // Deleting explicitly-named tasks (one or many) runs deterministically with no confirmation:
    // the user named the exact tasks, so each is resolved and removed in a single pass.
    const multiDeleteSpec = clickUpMultiDeleteSpecFromIntent(resolvedIntent);
    if (multiDeleteSpec) {
      return runClickUpMultiDeleteWorkflow(multiDeleteSpec);
    }

    // A delete selected by a fuzzy filter needs the user's answer between finding the task and
    // removing it, which a single planned step cannot express.
    if (resolvedIntent.operation_kind === "delete") {
      return runClickUpDeleteWorkflow(clickUpDeleteSpecFromIntent(resolvedIntent), awaitReply);
    }

    // Creating several named tasks loops the deterministic create over every name, resolving the
    // list once — reliable where the model-driven walk stopped after the first task.
    const multiCreateSpec = clickUpMultiCreateSpecFromIntent(resolvedIntent);
    if (multiCreateSpec) {
      return runClickUpMultiCreateWorkflow(multiCreateSpec, awaitReply);
    }

    // Creating a task needs only the list id, so code walks teams → spaces → lists directly.
    const createSpec = clickUpCreateSpecFromIntent(resolvedIntent);
    if (createSpec) {
      return runClickUpCreateWorkflow(createSpec, awaitReply);
    }

    return runPlannedIntent(resolvedIntent);
  },

  onDispatch: onClickupSourceDispatch,
});
