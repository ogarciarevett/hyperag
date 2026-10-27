import assert from "node:assert/strict";
import type { Context, Retrieval, WriteResult } from "../../src/index";
import { demo } from "./demo";

await demo(async ({ call, scope }) => {
  const saved = await call<WriteResult>("/v1/memories", {
    scope, source: "retry-policy.md", text: "Failed jobs retry three times with exponential backoff.",
  });
  const context = await call<Context & { retrieval: Retrieval }>("/v1/context", {
    query: "Failed jobs retry", maxTokens: 400,
  });
  assert.equal(context.retrieval.insufficientEvidence, false);
  assert.equal(context.citations[0]?.memoryId, saved.id);
  console.log(context.text);
});
