// Ingests the dev suite into Hyperag once and saves every query's retrieval, so both harness sides
// read the same memory output. Config: default embedder (multilingual-e5-small), no re-ranker, no graph.
// Run: docker compose -p hyperag-h2 up -d postgres && bun eval/harness2/retrieve.ts
import { readFileSync, writeFileSync } from "node:fs";
import { Pool } from "pg";

import { createLocalEmbedder, createPostgresStore, Hyperag } from "../../src";

type Doc = { id: string; scope: string; title: string; text: string; source: string; created_at: string } & Record<string, unknown>;
type Query = { id: string; scope: string; query: string };

const dir = new URL("./data/", import.meta.url);
const docs: Doc[] = JSON.parse(readFileSync(new URL("parents.json", dir), "utf8"));
const queries: Query[] = JSON.parse(readFileSync(new URL("queries.json", dir), "utf8"));
const pool = new Pool({ connectionString: process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag", max: 4 });
const embedder = createLocalEmbedder();
const h = new Hyperag({ store: createPostgresStore(pool), embedder, reranker: null, graph: null, admins: ["admin"] });
await h.init();

const scopes = [...new Set(docs.map((d) => d.scope))];
for (const scope of scopes) {
  await h.grant("admin", `writer-${scope}`, scope, "write");
  await h.grant("admin", `reader-${scope}`, scope, "read");
}
const started = performance.now();
for (const d of docs) {
  // The artifact kind is a label of the generator, not something a writer would record.
  const { id, scope, title, text, source, created_at, artifact: _artifact, ...metadata } = d;
  await h.ingest(`writer-${scope}`, { id, scope, text: `${title}. ${text}`, source, validFrom: created_at, metadata });
}
const ingestSeconds = (performance.now() - started) / 1000;

const hits: Record<string, unknown> = {};
for (const q of queries) {
  // Each query is asked by a principal that can read only the query's scope.
  const r = await h.retrieve(`reader-${q.scope}`, q.query, { limit: 10 });
  hits[q.id] = {
    insufficient_evidence: r.insufficientEvidence,
    hits: r.items.map((i) => ({ id: i.memory.id.slice(i.memory.scope.length + 1), text: i.memory.text, score: i.score,
                                 version: i.memory.version, recorded_at: i.memory.recordedAt, valid_from: i.memory.validFrom,
                                 metadata: i.memory.metadata })),
  };
}
const config = { embedder: embedder.describeModel(), reranker: null, graph: null, limit: 10,
                 ingest_seconds: Math.round(ingestSeconds), documents: docs.length, queries: queries.length };
writeFileSync(new URL("hits.json", dir), JSON.stringify({ config, hits }, null, 1) + "\n");
console.log(JSON.stringify(config));
await pool.end();
