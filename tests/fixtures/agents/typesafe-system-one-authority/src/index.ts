/// <reference path="./baml-runtime.d.ts" />
import type {
  SystemOneInput,
  SystemOneOutput,
  ToolStep,
} from "./baml-runtime";

export async function exerciseSystemOneContract(
  input: SystemOneInput,
): Promise<SystemOneOutput | null> {
  const toolSession = await openToolSession("typesafe/system_one");
  await toolSession.send(input);
  const step: ToolStep<SystemOneOutput> = await toolSession.continue();

  switch (step.status) {
    case "streaming":
      return step.output;
    case "suspended":
      return step.output;
    case "done":
      return step.output;
    case "error": {
      const {
        kind,
        message,
        retryable,
        disposition,
        code,
        hint,
        retry_after_ms,
      } = step.error;
      void [
        kind,
        message,
        retryable,
        disposition,
        code,
        hint,
        retry_after_ms,
      ];
      return null;
    }
  }
}
