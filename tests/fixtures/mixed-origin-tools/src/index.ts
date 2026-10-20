/// <reference path="./baml-runtime.d.ts" />
import type { JsonValue } from "./baml-runtime";

async function useStaticTool(): Promise<void> {
  const session = await openToolSession("system/callback");
  await session.send({ op: "cancel", sourceKey: "mixed-origin" });
  await session.finish();
}

async function useExternalTool(): Promise<void> {
  const session = await openToolSession("external/echo");
  await session.send({ message: "hello", detail: { value: "detail" } });
  const step = await session.continue();
  if (step.status === "error") {
    throw new Error(step.error.message);
  }
  if (step.status === "done") {
    const output = step.output;
    void output;
  }
  await session.finish();
}

async function useTypedMcpTool(): Promise<void> {
  const session = await openToolSession("mcp/typed");
  await session.send({ query: "hello" });
  await session.finish();
}

async function useOpaqueMcpTool(): Promise<void> {
  const payload: JsonValue = { arbitrary: true };
  const session = await openToolSession("mcp/opaque");
  await session.send(payload);
  await session.finish();
}

export async function useMixedOriginTools(): Promise<void> {
  await useStaticTool();
  await useExternalTool();
  await useTypedMcpTool();
  await useOpaqueMcpTool();
}
