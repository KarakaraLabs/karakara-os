/// <reference path="./baml-runtime.d.ts" />
import type {
  SystemOneInput,
  SystemOneOutput,
  SystemOneQuestion,
  ToolOutcome,
} from "./baml-runtime";

export type AlertRoutingState = {
  alert: {
    asset: string;
    power_w?: number;
    pressure_pa?: number;
  };
  candidates: Record<string, string>;
};

export type RoutingDecision =
  | { kind: "delegate"; agent: string; evidence: SystemOneOutput }
  | { kind: "abstain"; reason: string };

const SELECTION_PROBABILITY_THRESHOLD = 0.5;
const SELECTION_CONFIDENCE_THRESHOLD = 0.4;

function buildRoutingInput(state: AlertRoutingState): SystemOneInput {
  const questions: Record<string, SystemOneQuestion> = {
    next_agent: {
      type: "choice",
      instructions: "Which candidate in `candidates` should first investigate `alert`?",
      criteria: {
        ...state.candidates,
        none: "No candidate is clearly suitable",
      },
    },
    has_comparison_window: {
      type: "noul",
      instructions:
        "Does `alert` provide a time window or baseline to compare its readings against?",
    },
  };
  return { state, questions };
}

function selectedCandidate(output: SystemOneOutput): string | null {
  const answer = output.answers.next_agent;
  if (answer === undefined || answer.type !== "choice") {
    return null;
  }
  const probability = answer.probabilities[answer.choice] ?? 0;
  const confident =
    probability >= SELECTION_PROBABILITY_THRESHOLD &&
    answer.confidence >= SELECTION_CONFIDENCE_THRESHOLD;
  return confident && answer.choice !== "none" ? answer.choice : null;
}

/**
 * Route an alert to one candidate agent, or abstain.
 *
 * The criteria are an agent-defined snapshot of permitted candidates; the
 * tool never makes the A2A call. A dependent follow-up question would be a
 * second `invokeTool` call with the earlier answer or new evidence in `state`.
 */
export async function routeAlert(
  state: AlertRoutingState,
  allowedAgents: ReadonlySet<string>,
): Promise<RoutingDecision> {
  const outcome: ToolOutcome<SystemOneOutput> = await invokeTool(
    "typesafe/system_one",
    buildRoutingInput(state),
  );

  if (outcome.status === "error") {
    return { kind: "abstain", reason: `provider error: ${outcome.error.code}` };
  }

  const candidate = selectedCandidate(outcome.result);
  if (candidate === null) {
    return { kind: "abstain", reason: "uncertain or no candidate" };
  }
  if (!allowedAgents.has(candidate)) {
    return { kind: "abstain", reason: `candidate ${candidate} is not allowed` };
  }

  return { kind: "delegate", agent: candidate, evidence: outcome.result };
}
