import assert from "node:assert/strict";
import { Hyperag, createOpenAICompatibleEmbedder, type Retrieval, type WriteResult } from "../../src/index";
import { demo } from "./demo";

await demo(async ({ hyperag, store, serve, call, scope }) => {
  const saved = await call<WriteResult>("/v1/memories", { scope, source: "policy.md", text: "Failed jobs retry three times." });
  const embedder = createOpenAICompatibleEmbedder({
    baseUrl: process.env.README_EMBEDDING_URL!, model: "example-embedding", revision: "v1",
    dimensions: 3, normalized: true,
  });
  const indexed = await hyperag.reindex(embedder);
  assert.equal(indexed.embedded, 1);
  const next = new Hyperag({ store, embedder, graph: null });
  await next.init(); // reindex alone does not switch the running instance.
  serve(next);
  const result = await call<Retrieval>("/v1/retrieve", { query: "retry", channels: { bm25: false, graph: false } });
  assert(result.pathsUsed.includes("vector"));
  assert.equal(result.items[0]?.memory.id, saved.id);
});
