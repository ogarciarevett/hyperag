// Runs Engram (the `engram` binary on this machine) over a frozen packaged suite through its HTTP API,
// in a throwaway data directory, and writes records in the same shape as run-suite.ts.
//   bun eval/run-engram.ts --data <suite dir> --out eval/results/cross-team-engram.jsonl
// Each scope is one Engram project; a query searches only its own scope's project (limit 10).
// Engram has no abstention signal, so `abstained` is left unset. `--match all` is Engram's default
// (every query word must match); `--match any` is its OR mode.
import { spawn } from "node:child_process";
import { appendFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

const { values: args } = parseArgs({ options: { data: { type: "string" }, out: { type: "string" }, port: { type: "string", default: "7498" }, match: { type: "string", default: "all" } } });
if (!args.data || !args.out) throw new Error("--data and --out are required");
type Doc = { id: string; scope: string; text: string };
type Query = { id: string; scope: string; query: string };
const corpus = JSON.parse(readFileSync(`${args.data}/corpus.json`, "utf8")) as Doc[];
const queries = JSON.parse(readFileSync(`${args.data}/queries.json`, "utf8")) as Query[];

const dir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "engram-eval-"));
const base = `http://127.0.0.1:${args.port}`;
const server = spawn("engram", ["serve", args.port!], {
  env: { ...process.env, ENGRAM_DATA_DIR: dir, ENGRAM_NO_UPDATE_CHECK: "1" },
  stdio: "ignore",
});
const post = (path: string, body: unknown) =>
  fetch(base + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });

try {
  for (let i = 0; i < 50; i++) {
    if (await fetch(`${base}/health`).then((r) => r.ok, () => false)) break;
    await Bun.sleep(100);
  }
  const version = ((await (await fetch(`${base}/health`)).json()) as { version: string }).version;
  for (const scope of new Set(corpus.map((d) => d.scope))) await post("/sessions", { id: `eval-${scope}`, project: scope });
  const started = performance.now();
  for (const doc of corpus) {
    const r = await post("/observations", { session_id: `eval-${doc.scope}`, project: doc.scope, title: doc.id, content: doc.text, type: "discovery" });
    if (!r.ok) throw new Error(`engram save failed: ${r.status} ${await r.text()}`);
  }
  const ingestMs = performance.now() - started;

  const ask = async (q: Query) => {
    const t = performance.now();
    const url = `${base}/search?${new URLSearchParams({ q: q.query, project: q.scope, limit: "10", match_mode: args.match! })}`;
    const r = await fetch(url);
    const latency = performance.now() - t;
    if (!r.ok) return { response: { ok: false, error: `http_${r.status}`, hits: [] }, latency };
    const rows = (await r.json()) as Array<{ title: string }>;
    return { response: { ok: true, hits: rows.map((row) => ({ id: row.title })) }, latency };
  };
  writeFileSync(args.out, "");
  for (const q of queries.slice(0, 8)) await ask(q);
  for (const q of queries) {
    const r = await ask(q);
    appendFileSync(args.out, `${JSON.stringify({ query_id: q.id, workload: "quality", concurrency: 1, latency_ms: r.latency, response: r.response })}\n`);
  }
  const du = Bun.spawnSync(["du", "-sk", dir]).stdout.toString().split("\t")[0];
  writeFileSync(`${args.out}.perf.json`, JSON.stringify({ engine: "engram", version, matchMode: args.match, documents: corpus.length, ingestMs, dataDirBytes: Number(du) * 1024 }, null, 2));
  console.log(`done engram ${version}: ingest ${ingestMs.toFixed(0)} ms`);
} finally {
  server.kill();
  rmSync(dir, { recursive: true, force: true });
}
