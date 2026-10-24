// One agent records a decision, the process "restarts" (new pool, new Hyperag), and the agent
// recalls the decision with its source and version.
// Run: docker compose -p hyperag up -d && bun examples/single-agent.ts
import { Pool } from "pg";

import { createLocalEmbedder, createNeo4jGraph, createPostgresStore, Hyperag } from "../src";

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const graph = createNeo4jGraph({ baseUrl: "http://127.0.0.1:57474", username: "neo4j", password: "fake-test-only" });
const embedder = createLocalEmbedder();

async function session<T>(fn: (h: Hyperag) => Promise<T>): Promise<T> {
  const pool = new Pool({ connectionString: PG });
  const h = new Hyperag({ store: createPostgresStore(pool), embedder, graph, admins: ["admin"] });
  await h.init();
  try {
    return await fn(h);
  } finally {
    await pool.end();
  }
}

const scope = "example:coder";
await session(async (h) => {
  await h.grant("admin", "coder", scope, "read");
  await h.grant("admin", "coder", scope, "write");
  const w = await h.ingest("coder", {
    scope,
    kind: "decision",
    source: "PR #17 review",
    text: "Decision: retries use exponential backoff capped at 30 seconds; no retry on HTTP 4xx.",
  });
  console.log("session 1 wrote", w);
});

await session(async (h) => {
  const ctx = await h.context("coder", "what is our retry policy for failed HTTP calls?", { maxTokens: 300 });
  console.log("session 2 context:\n" + ctx.text);
  console.log("citations:", ctx.citations);
  console.log("paths:", ctx.retrieval.pathsUsed, "limitations:", ctx.retrieval.limitations);
});
