// Runs one Hyperag variant over a frozen packaged suite (cross-team or SciFact) and writes raw
// request records for eval/score.py. Usage:
//   bun eval/run-suite.ts --variant V0 --data <suite dir> --out eval/results/cross-team-V0.jsonl
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { Pool } from "pg";

import { createLocalEmbedder, createLocalReranker, createOpenAICompatibleEmbedder } from "../src/adapters/embedders";
import { createNeo4jGraph } from "../src/adapters/neo4j";
import { createPostgresStore } from "../src/adapters/postgres";
import { Hyperag, type RetrieveOptions } from "../src/core";
import type { Embedder } from "../src/types";

const { values: args } = parseArgs({
  options: {
    variant: { type: "string" },
    data: { type: "string" },
    out: { type: "string" },
    suite: { type: "string", default: "cross-team" },
    concurrency: { type: "string", default: "1,4,8" },
  },
});
const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const NEO4J = { baseUrl: "http://127.0.0.1:57474", username: "neo4j", password: "fake-test-only" };

type Doc = { id: string; scope: string; text: string; source: string; language: string; parent_id: string };
type Query = { id: string; scope: string; query: string };

const VARIANTS: Record<string, { family: "bge" | "e5" | "lex"; graph: boolean; rerank: boolean }> = {
  V0: { family: "bge", graph: false, rerank: false },
  V1: { family: "bge", graph: true, rerank: false },
  V2: { family: "bge", graph: false, rerank: true },
  V3: { family: "e5", graph: false, rerank: false },
  V4: { family: "e5", graph: true, rerank: true },
  L: { family: "lex", graph: false, rerank: false },
};
const variant = VARIANTS[args.variant ?? ""];
if (!variant || !args.data || !args.out) throw new Error("--variant, --data and --out are required");

const embedders: Record<string, () => Embedder | null> = {
  // The baseline's dense model and runtime: fastembed 0.9.0 (Qdrant ONNX build of
  // BAAI/bge-small-en-v1.5) behind eval/fastembed_server.py, through the OpenAI-compatible adapter.
  bge: () =>
    createOpenAICompatibleEmbedder({
      baseUrl: process.env.HYPERAG_FASTEMBED_URL ?? "http://127.0.0.1:58321",
      model: "BAAI/bge-small-en-v1.5",
      revision: "fastembed-0.9.0 Qdrant/bge-small-en-v1.5-onnx-Q@aa8f8b060edb00e03bfdd08813a2949946c8ba55",
      dimensions: 384,
      normalized: true,
    }),
  e5: () => createLocalEmbedder(),
  lex: () => null,
};

const corpus = JSON.parse(readFileSync(`${args.data}/corpus.json`, "utf8")) as Doc[];
const queries = JSON.parse(readFileSync(`${args.data}/queries.json`, "utf8")) as Query[];
const scopes = [...new Set(corpus.map((d) => d.scope))];

// One database per embedding family and suite, ingested once and reused by the variants.
const dbName = `eval_${args.suite!.replace(/\W/gu, "_")}_${variant.family}`;
const admin = new Pool({ connectionString: PG, max: 1 });
const exists = (await admin.query("select 1 from pg_database where datname = $1", [dbName])).rowCount === 1;
if (!exists) await admin.query(`create database ${dbName}`);
await admin.end();
const pool = new Pool({ connectionString: PG.replace(/\/[^/]+$/u, `/${dbName}`), max: 8 });
const graph = createNeo4jGraph(NEO4J);
const store = createPostgresStore(pool);
const hyperag = new Hyperag({
  store,
  embedder: embedders[variant.family]!(),
  graph,
  reranker: variant.rerank ? createLocalReranker() : null,
  admins: ["eval-admin"],
});

let rssPeak = 0;
const sampler = setInterval(() => (rssPeak = Math.max(rssPeak, process.memoryUsage().rss)), 200);
const t0 = performance.now();
await hyperag.init();
const startupMs = performance.now() - t0;

const ingestFile = `eval/results/${dbName}.ingest.json`;
const count = Number((await pool.query("select count(*) from memories")).rows[0].count);
if (count < corpus.length) {
  for (const scope of scopes) {
    await hyperag.grant("eval-admin", "eval-writer", scope, "write");
    await hyperag.grant("eval-admin", "eval-writer", scope, "read");
    await hyperag.grant("eval-admin", `reader:${scope}`, scope, "read");
  }
  const started = performance.now();
  for (const doc of corpus)
    await hyperag.ingest("eval-writer", {
      scope: doc.scope,
      id: doc.id,
      text: doc.text,
      source: doc.source,
      metadata: { language: doc.language, parent_id: doc.parent_id },
    });
  const ingestMs = performance.now() - started;
  // Graph edges from identifiers in the text only (protocol): same scope, same identifier.
  const graphStarted = performance.now();
  const groups = new Map<string, string[]>();
  for (const doc of corpus)
    for (const ident of new Set(doc.text.match(/\b(?:[A-Z][a-z]+-\d+|[A-Z]{2,}-[A-Z0-9]+)\b/gu) ?? [])) {
      const key = `${doc.scope}|${ident}`;
      groups.set(key, [...(groups.get(key) ?? []), `${doc.scope}/${doc.id}`]);
    }
  let edges = 0;
  for (const members of groups.values())
    for (const member of members.slice(1)) {
      await hyperag.relate("eval-writer", { from: member, to: members[0]!, type: "SHARES_ID" });
      edges++;
    }
  writeFileSync(
    ingestFile,
    JSON.stringify({ documents: corpus.length, ingestMs, graphEdges: edges, graphMs: performance.now() - graphStarted }, null, 2),
  );
}

const options: RetrieveOptions = {
  limit: 10,
  candidatesPerChannel: 24,
  channels: { bm25: true, vector: variant.family !== "lex", graph: variant.graph, rerank: variant.rerank },
};
const ask = async (q: Query) => {
  const started = performance.now();
  try {
    const r = await hyperag.retrieve(`reader:${q.scope}`, q.query, options);
    const latency = performance.now() - started;
    // Stored ids are `<scope>/<corpus id>`; the scorer wants the corpus id.
    const hits = r.items.map((i) => ({ id: i.memory.id.slice(i.memory.id.indexOf("/") + 1) }));
    return { response: { ok: true, hits, abstained: r.insufficientEvidence, paths: r.pathsUsed }, latency };
  } catch (error) {
    return { response: { ok: false, error: String(error), hits: [] }, latency: performance.now() - started };
  }
};

writeFileSync(args.out, "");
for (const q of queries.slice(0, 8)) await ask(q); // warm-up, not recorded
const record = (q: Query, r: Awaited<ReturnType<typeof ask>>, workload: string, concurrency: number) =>
  appendFileSync(
    args.out!,
    `${JSON.stringify({ query_id: q.id, workload, concurrency, latency_ms: r.latency, response: r.response })}\n`,
  );
for (const q of queries) record(q, await ask(q), "quality", 1);

const throughput: Record<string, number> = {};
for (const c of args.concurrency!.split(",").map(Number)) {
  let next = 0;
  const started = performance.now();
  await Promise.all(
    Array.from({ length: c }, async () => {
      while (next < queries.length) {
        const q = queries[next++]!;
        record(q, await ask(q), "load", c);
      }
    }),
  );
  throughput[`c${c}`] = queries.length / ((performance.now() - started) / 1000);
}
clearInterval(sampler);
const stats = await store.stats();
writeFileSync(
  `${args.out}.perf.json`,
  JSON.stringify(
    {
      variant: args.variant,
      database: dbName,
      startupMs,
      ingest: JSON.parse(readFileSync(ingestFile, "utf8")),
      throughputQps: throughput,
      rssPeakBytes: rssPeak,
      ...stats,
    },
    null,
    2,
  ),
);
await pool.end();
console.log(`done ${args.variant}: ${queries.length} queries, throughput ${JSON.stringify(throughput)}`);
