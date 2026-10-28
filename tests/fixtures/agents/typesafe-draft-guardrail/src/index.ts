/// <reference path="./baml-runtime.d.ts" />
import type {
  SystemOneInput,
  SystemOneOutput,
  SystemOneQuestion,
  ToolOutcome,
} from "./baml-runtime";

export type DraftReviewState = {
  user_request: string;
  draft_reply: string;
  policy: string;
};

export type GuardrailDecision =
  | { kind: "send"; evidence: SystemOneOutput }
  | { kind: "regenerate"; reason: string };

const DISCLOSURE_THRESHOLD = 0.3;

function buildGuardrailInput(state: DraftReviewState): SystemOneInput {
  const questions: Record<string, SystemOneQuestion> = {
    discloses_internal_detail: {
      type: "noul",
      instructions: "Does `draft_reply` disclose anything `policy` forbids?",
    },
    answers_request: {
      type: "score",
      instructions: "How completely does `draft_reply` address `user_request`?",
      criteria: [
        "Does not address the request",
        "Partially addresses it; key points missing",
        "Fully addresses the request",
      ],
    },
  };
  return { state, questions };
}

/**
 * Gate an LLM draft before it is sent.
 *
 * The draft is only sent below the tested disclosure threshold; otherwise the
 * agent regenerates or escalates. Thresholds reflect the cost of a wrong
 * action and are validated against labeled examples, not documentation.
 */
export async function reviewDraft(
  state: DraftReviewState,
): Promise<GuardrailDecision> {
  const outcome: ToolOutcome<SystemOneOutput> = await invokeTool(
    "typesafe/system_one",
    buildGuardrailInput(state),
  );

  if (outcome.status === "error") {
    return { kind: "regenerate", reason: `provider error: ${outcome.error.code}` };
  }

  const answer = outcome.result.answers.discloses_internal_detail;
  const risk = answer !== undefined && answer.type === "noul" ? answer.noul : 1;

  if (risk >= DISCLOSURE_THRESHOLD) {
    return { kind: "regenerate", reason: "draft may disclose internal detail" };
  }

  return { kind: "send", evidence: outcome.result };
}
