import assert from "node:assert/strict";
import type { Retrieval } from "../../src/index";
import { demo } from "./demo";

// Neo4j is the built-in backend (used by relate.ts); null disables the graph slot.
await demo(async ({ call, scope }) => {
  await call("/v1/memories", { scope, source: "policy.md", text: "Failed jobs retry three times." });
  const result = await call<Retrieval>("/v1/retrieve", { query: "retry" });
  assert.equal(result.items.length, 1);
  assert(!result.pathsUsed.includes("graph"));
  assert(result.limitations.some((limit) => limit.includes("no graph configured")));
}, { graph: null });
