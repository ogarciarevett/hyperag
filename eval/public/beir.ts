// BEIR retrieval through any system adapter. Official test split; writes a TREC run file and a
// metadata file for score_beir.py (pytrec_eval, as the BEIR toolkit scores).
//   bun eval/public/beir.ts --system hyperag --profile bge --dataset scifact --data <dir with BEIR sets>
import { createReadStream, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { createInterface } from "node:readline";

import type { Doc, SystemFactory } from "./system";
import { childrenRssBytes } from "./rss";
import { pyAdapter } from "./systems/py";

const { values: a } = parseArgs({
  options: {
    system: { type: "string" },
    profile: { type: "string", default: "default" },
    dataset: { type: "string" },
    data: { type: "string", default: "data/beir" },
    out: { type: "string", default: "eval/public/results" },
    k: { type: "string", default: "100" },
    limit: { type: "string" },
  },
});
if (!a.system || !a.dataset) throw new Error("--system and --dataset are required");
const dir = `${a.data}/${a.dataset}`;
const k = Number(a.k);

async function* jsonl<T>(path: string): AsyncGenerator<T> {
  for await (const line of createInterface({ input: createReadStream(path) })) if (line.trim()) yield JSON.parse(line) as T;
}
const testQids = new Set(readFileSync(`${dir}/qrels/test.tsv`, "utf8").split("\n").slice(1).filter(Boolean).map((l) => l.split("\t")[0]!));
const queries: Array<{ id: string; text: string }> = [];
for await (const q of jsonl<{ _id: string; text: string }>(`${dir}/queries.jsonl`)) if (testQids.has(q._id)) queries.push({ id: q._id, text: q.text });
const asked = a.limit ? queries.slice(0, Number(a.limit)) : queries;

const factory = existsSync(`${import.meta.dir}/systems/${a.system}.ts`)
  ? ((await import(`./systems/${a.system}.ts`)).default as SystemFactory)
  : pyAdapter(a.system);
const system = await factory(a.profile!);
const meta = await system.describe();
const ns = `beir-${a.dataset}`;
await system.open(ns);

let rssPeak = 0;
const sampler = setInterval(() => (rssPeak = Math.max(rssPeak, process.memoryUsage().rss)), 500);
let systemRssPeak = 0;
// an adapter that does not report its RAM is measured as this process's children (its servers)
const sampleSystem = async () => (systemRssPeak = Math.max(systemRssPeak, system.rssBytes ? await system.rssBytes() : childrenRssBytes()));
const t0 = performance.now();
let batch: Doc[] = [];
let docs = 0;
for await (const d of jsonl<{ _id: string; title?: string; text: string }>(`${dir}/corpus.jsonl`)) {
  // BEIR concatenates title and text for every model; empty documents are skipped (they cannot rank).
  if (!d.text.trim() && !d.title?.trim()) continue;
  batch.push({ id: d._id, text: d.text.slice(0, 15_000), ...(d.title ? { title: d.title } : {}) });
  docs++;
  if (batch.length === 512) {
    await system.ingest(ns, batch);
    batch = [];
    await sampleSystem();
  }
}
if (batch.length) await system.ingest(ns, batch);
const ingestMs = performance.now() - t0;
await sampleSystem();

const run: string[] = [];
const latencies: number[] = [];
const errors: Record<string, string> = {};
for (const [n, q] of asked.entries()) {
  const t = performance.now();
  // a query the system itself fails on scores 0 and is listed, instead of ending the run
  const hits = await system.search(ns, q.text, k).catch((e: Error) => {
    if (/adapter (exited|protocol)/.test(e.message)) throw e;  // a broken bridge ends the run
    return (errors[q.id] = e.message.slice(0, 300)), [];
  });
  latencies.push(performance.now() - t);
  if (n % 200 === 0) await sampleSystem();
  hits.forEach((h, i) => run.push(`${q.id} Q0 ${h.id} ${i + 1} ${(h.score ?? 1 / (i + 1)).toFixed(6)} ${meta.system}`));
}
clearInterval(sampler);
await sampleSystem();
await system.close();
const pct = (p: number) => [...latencies].sort((x, y) => x - y)[Math.min(latencies.length - 1, Math.ceil(p * latencies.length) - 1)];
const tag = `${a.dataset}.${meta.system}.${meta.profile.replaceAll("+", "_")}`;
mkdirSync(a.out!, { recursive: true });
writeFileSync(`${a.out}/${tag}.run`, `${run.join("\n")}\n`);
writeFileSync(
  `${a.out}/${tag}.meta.json`,
  JSON.stringify(
    { ...meta, transportRetry: process.env.EVAL_GPU_TRANSPORT_NOTE ?? "pre-common-proxy policy; see run notes", dataset: a.dataset, split: "test", documents: docs, queries: asked.length, k, ingestMs, latencyP50Ms: pct(0.5), latencyP95Ms: pct(0.95), rssPeakBytes: rssPeak, systemRssPeakBytes: systemRssPeak, failedQueries: Object.keys(errors).length, errors, ...(a.limit ? { askedIds: asked.map((q) => q.id) } : {}), date: new Date().toISOString() },
    null,
    2,
  ),
);
console.log(`${tag}: ${docs} docs ingested in ${(ingestMs / 1000).toFixed(1)} s, ${asked.length} queries, p50 ${pct(0.5)?.toFixed(1)} ms`);
