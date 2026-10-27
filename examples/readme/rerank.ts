import assert from "node:assert/strict";
import { createLocalReranker, type Retrieval } from "../../src/index";
import { demo } from "./demo";

await demo(async ({ call, scope }) => {
  for (const text of ["Failed jobs retry three times.", "Failed jobs appear in the monitoring dashboard."])
    await call("/v1/memories", { scope, source: "operations.md", text });
  const ranked = await call<Retrieval>("/v1/retrieve", { query: "How many times do failed jobs retry?" });
  assert.equal(ranked.items.length, 2);
  assert(ranked.pathsUsed.some((path) => path.startsWith("rerank:")));
  assert(ranked.items.every((item) => Number.isFinite(item.rerankScore)));
}, { reranker: createLocalReranker({ threads: 1 }) });
