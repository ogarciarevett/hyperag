// The frozen suite through a Hyperag HTTP service (TypeScript or Rust build), with the same records
// as run-suite.ts so score.py scores both. Latency here is end to end over HTTP; RSS is the service
// process, sampled every 200 ms. Ingest and graph links run only on a fresh database: after a
// harness change, drop the eval database first or the old links are reused.
//   HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/run-suite-http.ts --build rust --variant V0 --data <suite dir> --out <file>
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { Pool } from "pg";

import { startService } from "./hyperag-http";

const { values: a } = parseArgs({
  options: {
    build: { type: "string", default: "rust" },
    variant: { type: "string" },
    data: { type: "string" },
    out: { type: "string" },
    concurrency: { type: "string", default: "1,4,8" },
    // shared: an agent reads every team's notes in its scope; individual: only its own team's.
    memory: { type: "string", default: "shared" },
  },
});
const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
type Doc = { id: string; scope: string; text: string; source: string; language: string; parent_id: string; origin_team: string };
type Query = { id: string; scope: string; query: string };

// Embedding per variant family and build. The TypeScript service offers only e5 (Xenova q8) or none.
const FAMILY: Record<string, { family: string; graph: boolean; rerank: boolean; bm25?: boolean }> = {
  V0: { family: "bge", graph: false, rerank: false },
  V1: { family: "bge", graph: true, rerank: false },
  V2: { family: "bge", graph: false, rerank: true },
  V3: { family: "e5", graph: false, rerank: false },
  V4: { family: "e5", graph: true, rerank: true },
  L: { family: "lex", graph: false, rerank: false },
  // Paper conditions: vector channel alone (C44), EmbeddingGemma 300m q8, 768-d (C47).
  Vv: { family: "bge", graph: false, rerank: false, bm25: false },
  G: { family: "gemma", graph: false, rerank: false },
  G4: { family: "gemma", graph: true, rerank: true },
};
const v = FAMILY[a.variant ?? ""];
if (!v || !a.data || !a.out) throw new Error("--variant, --data and --out are required");
const rust = a.build === "rust";
const embedding = { gemma: "hf:onnx-community/embeddinggemma-300m-ONNX?dtype=q8", bge: "hf:Qdrant/bge-small-en-v1.5-onnx-Q", e5: "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean", lex: "none" }[v.family]!;
if (!rust && (v.family === "bge" || v.family === "gemma")) throw new Error("the TypeScript service has only e5; use the Rust build");
const models = rust
  ? { embedding, reranker: v.rerank ? "hf:Xenova/bge-reranker-base?revision=280bcc27a84e0b898c251e06fddb25171bd9b101&dtype=q8" : "none" }
  : { env: { HYPERAG_EMBEDDER: v.family === "lex" ? "none" : "local", HYPERAG_RERANKER: v.rerank ? "local" : "none" } };

const corpus = JSON.parse(readFileSync(`${a.data}/corpus.json`, "utf8")) as Doc[];
const queries = JSON.parse(readFileSync(`${a.data}/queries.json`, "utf8")) as Query[];
const individual = a.memory === "individual";
const scopes = [...new Set(corpus.map((d) => d.scope))];
// Each note is stored under its scope and the team that wrote it; the memory condition decides
// which of those an agent of the scope may read.
const scopeOf = (d: Doc) => `${d.scope}:${d.origin_team}`;
const stored = [...new Set(corpus.map(scopeOf))];

const db = `evalhttp_${a.build}_${v.family}_teams`;
const admin = new Pool({ connectionString: PG, max: 1 });
const fresh = (await admin.query("select 1 from pg_database where datname = $1", [db])).rowCount === 0;
if (fresh) await admin.query(`create database ${db}`);
await admin.end();

const started = performance.now();
const svc = await startService({
  pgUrl: PG.replace(/\/[^/]+$/u, `/${db}`),
  principals: ["eval-admin", "eval-writer", ...scopes.map((s) => `reader:${s}`)],
  admins: ["eval-admin"],
  neo4j: true,
  ...models,
});
const startupMs = performance.now() - started;
let rssPeak = 0;
const sampler = setInterval(() => (rssPeak = Math.max(rssPeak, svc.rss() || 0)), 200);

const ingestFile = `eval/results/${db}.ingest.json`;
if (fresh) {
  for (const sub of stored) {
    await svc.must("eval-admin", "POST", "/v1/grants", { principal: "eval-writer", scope: sub, permission: "write" });
    await svc.must("eval-admin", "POST", "/v1/grants", { principal: "eval-writer", scope: sub, permission: "read" });
  }
  const t = performance.now();
  const memories = corpus.map((d) => ({ scope: scopeOf(d), id: d.id, text: d.text, source: d.source, metadata: { language: d.language, parent_id: d.parent_id } }));
  for (let i = 0; i < memories.length; i += 256) {
    const batch = memories.slice(i, i + 256);
    const r = await svc.call("eval-writer", "POST", "/v1/memories/batch", { memories: batch });
    // The TypeScript service has no batch route: one call per memory.
    if (r.status === 404) for (const m of batch) await svc.must("eval-writer", "POST", "/v1/memories", m);
    else if (r.status !== 200) throw new Error(JSON.stringify(r.json));
  }
  const ingestMs = performance.now() - t;
  const g = performance.now();
  const groups = new Map<string, string[]>();
  for (const doc of corpus)
    for (const ident of new Set(doc.text.match(/\b(?:[A-Z][a-z]+-\d+|[A-Z]{2,}-[A-Z0-9]+)\b/gu) ?? [])) {
      // Links join notes of one scope across the teams that wrote them, as before the team split;
      // in the individual condition the reader's grants drop the paths it may not read.
      const key = `${doc.scope}|${ident}`;
      groups.set(key, [...(groups.get(key) ?? []), `${scopeOf(doc)}/${doc.id}`]);
    }
  let edges = 0;
  for (const members of groups.values())
    for (const member of members.slice(1)) {
      await svc.must("eval-writer", "POST", "/v1/relations", { from: member, to: members[0], type: "SHARES_ID" });
      edges++;
    }
  writeFileSync(ingestFile, JSON.stringify({ documents: corpus.length, ingestMs, graphEdges: edges, graphMs: performance.now() - g }, null, 2));
}

// Readers are granted per run, so one ingested database serves both memory conditions.
for (const scope of scopes) {
  const reader = `reader:${scope}`;
  for (const sub of stored.filter((x) => x.startsWith(`${scope}:`))) {
    const own = sub === `${scope}:${scope}`;
    const allowed = own || !individual;
    await svc.call("eval-admin", allowed ? "POST" : "DELETE", "/v1/grants", { principal: reader, scope: sub, permission: "read" });
  }
}
const body = (q: Query) => ({
  query: q.query,
  limit: 10,
  candidatesPerChannel: 24,
  channels: { bm25: v.bm25 ?? true, vector: v.family !== "lex", graph: v.graph, rerank: v.rerank },
});
const ask = async (q: Query) => {
  const t = performance.now();
  const r = await svc.call(`reader:${q.scope}`, "POST", "/v1/retrieve", body(q));
  const latency = performance.now() - t;
  if (r.status !== 200) return { response: { ok: false, error: JSON.stringify(r.json), hits: [] }, latency };
  const res = r.json as unknown as { items: Array<{ memory: { id: string } }>; insufficientEvidence: boolean; pathsUsed: string[] };
  const hits = res.items.map((i) => ({ id: i.memory.id.slice(i.memory.id.indexOf("/") + 1) }));
  return { response: { ok: true, hits, abstained: res.insufficientEvidence, paths: res.pathsUsed }, latency };
};
writeFileSync(a.out, "");
const record = (q: Query, r: Awaited<ReturnType<typeof ask>>, workload: string, c: number) =>
  appendFileSync(a.out!, `${JSON.stringify({ query_id: q.id, workload, concurrency: c, latency_ms: r.latency, response: r.response })}\n`);
for (const q of queries.slice(0, 8)) await ask(q);
for (const q of queries) record(q, await ask(q), "quality", 1);
const throughput: Record<string, number> = {};
for (const c of a.concurrency!.split(",").map(Number)) {
  let next = 0;
  const t = performance.now();
  await Promise.all(
    Array.from({ length: c }, async () => {
      while (next < queries.length) {
        const q = queries[next++]!;
        record(q, await ask(q), "load", c);
      }
    }),
  );
  throughput[`c${c}`] = queries.length / ((performance.now() - t) / 1000);
}
clearInterval(sampler);
await svc.stop();
writeFileSync(
  `${a.out}.perf.json`,
  JSON.stringify({ build: a.build, variant: a.variant, memory: a.memory, database: db, serviceStartupMs: startupMs, ingest: JSON.parse(readFileSync(ingestFile, "utf8")), throughputQps: throughput, serviceRssPeakBytes: rssPeak }, null, 2),
);
console.log(`done ${a.build} ${a.variant}: throughput ${JSON.stringify(throughput)}, service RSS peak ${(rssPeak / 1e6).toFixed(0)} MB`);
