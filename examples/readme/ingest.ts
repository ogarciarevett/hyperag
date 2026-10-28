import assert from "node:assert/strict";
import { demo } from "./demo";

await demo(async ({ call, scope }) => {
  const saved = await call<{ id: string; version: number; created: boolean }>("/v1/memories", {
    scope, source: "retry-policy.md", text: "Failed jobs retry three times with exponential backoff.",
  });
  assert.equal(saved.version, 1);
  assert.equal(saved.created, true);
});
