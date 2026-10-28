// No remote provider: this loopback fixture tests only the embedding HTTP contract.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

let requests = 0;
const provider = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
  assert.equal(new URL(request.url).pathname, "/v1/embeddings");
  const body = await request.json() as { model: string; input: string[] };
  assert.equal(body.model, "example-embedding");
  requests++;
  return Response.json({ data: body.input.map((_, index) => ({ index, embedding: [1, 0, 0] })) });
} });
assert(![7481, 7482].includes(provider.port!));
process.env.README_EMBEDDING_URL = provider.url.origin;
try {
  const readme = await readFile(new URL("../../README.md", import.meta.url), "utf8");
  for (const name of ["ingest", "search", "relate", "rerank", "plugin", "embeddings", "graph"]) {
    const source = (await readFile(new URL(`./${name}.ts`, import.meta.url), "utf8")).trim();
    assert(readme.includes("```ts\n" + source + "\n```"), `${name}: README must match the runnable file`);
    await import(`./${name}.ts`);
    console.log(`PASS README ${name}`);
  }
  assert(requests >= 2, "embedding switch must embed documents and query through HTTP");
} finally { provider.stop(true); }
