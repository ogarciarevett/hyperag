// Retrieval-only check on the frozen LongMemEval-S dev split (never the test subset): for each dev
// question, does the top k hold a turn from one of its evidence sessions? No generator, no judge,
// so it runs on CPU. It compares profiles of one system, for example with and without +temporal.
//   bun eval/public/dev_retrieval.ts --system hyperag-rust --profile bm25+graph+temporal \
//     --split eval/public/splits/lme-s-dev.json [--k 10] [--limit 50] [--out <file>]
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";

import type { SystemFactory } from "./system";

const { values: a } = parseArgs({
  options: {
    system: { type: "string" },
    profile: { type: "string" },
    split: { type: "string" },
    data: { type: "string", default: "data/memory" },
    k: { type: "string", default: "10" },
    limit: { type: "string" },
    out: { type: "string" },
    // Wait before each question while the machine is over its pause limits (macOS: swap, load, free memory).
    guard: { type: "boolean", default: false },
  },
});
const sh = (cmd: string[]) => Bun.spawnSync(cmd).stdout.toString();
const overLimits = () => {
  const swapMb = Number(/used = ([\d.]+)M/u.exec(sh(["sysctl", "vm.swapusage"]))?.[1] ?? 0);
  const load = Number(sh(["sysctl", "-n", "vm.loadavg"]).trim().replace(/[{}]/gu, "").trim().split(/\s+/u)[0] ?? 0);
  const free = Number(/free percentage: (\d+)%/u.exec(sh(["memory_pressure"]))?.[1] ?? 100);
  // The machine rule: pause above load 30, swap 11 GB, or under 30 percent free memory.
  return swapMb > 11 * 1024 || load > 30 || free < 30 ? `swap ${swapMb.toFixed(0)} MB, load ${load}, free ${free}%` : null;
};
if (!a.system || !a.profile || !a.split) throw new Error("--system, --profile and --split are required");
const split = JSON.parse(readFileSync(a.split, "utf8")) as { dev: string[]; test: string[]; source: { sha256: string } };
const dev = new Set(split.dev);
// Guard: the test subset is never read here.
if (split.test.some((id) => dev.has(id))) throw new Error("the split's dev and test overlap");
type Entry = { question_id: string; question_type: string; question: string; question_date: string; answer_session_ids: string[]; haystack_session_ids: string[]; haystack_dates: string[]; haystack_sessions: Array<Array<{ role: string; content: string }>> };
const raw = readFileSync(`${a.data}/longmemeval_s_cleaned.json`);
if (createHash("sha256").update(raw).digest("hex") !== split.source.sha256) throw new Error("the data file differs from the one the split was made from");
const all = JSON.parse(raw.toString("utf8")) as Entry[];
let questions = all.filter((e) => dev.has(e.question_id)).sort((p, q) => (p.question_id < q.question_id ? -1 : 1));
if (a.limit) questions = questions.slice(0, Number(a.limit));

// The same turn text and date as memory.ts writes, so the result carries over to the benchmark.
const isoDate = (d: string) => {
  const m = /(\d{4})\/(\d{2})\/(\d{2})/u.exec(d);
  return m ? `${m[1]}-${m[2]}-${m[3]}` : undefined;
};
const cut = (s: string) => (s.length > 15_000 ? s.slice(0, 15_000) : s);
const factory = (await import(`./systems/${a.system}.ts`)).default as SystemFactory;
const s = await factory(a.profile);
const k = Number(a.k);
const rows: Array<{ id: string; type: string; hit: boolean }> = [];
for (const e of questions) {
  for (let why = a.guard ? overLimits() : null; why; why = overLimits()) {
    console.error(`paused: ${why}`);
    await Bun.sleep(60_000);
  }
  const ns = `lmedev/${e.question_id}`;
  await s.open(ns);
  for (const [i, turns] of e.haystack_sessions.entries())
    await s.ingest(ns, turns.map((t, j) => ({ id: `${e.haystack_session_ids[i]}:${j}`, speaker: t.role, timestamp: e.haystack_dates[i], text: cut(`[${e.haystack_dates[i]}] ${t.role}: ${t.content}`) })));
  const asOf = isoDate(e.question_date);
  const search = s.search as (ns: string, q: string, k: number, o?: { asOf?: string }) => Promise<ReadonlyArray<{ id: string }>>;
  const hits = await search(ns, e.question, k, asOf ? { asOf } : undefined);
  const evidence = new Set(e.answer_session_ids);
  rows.push({ id: e.question_id, type: e.question_type, hit: hits.some((h) => evidence.has(h.id.slice(0, h.id.lastIndexOf(":")))) });
}
const described = await s.describe();
await s.close();

const rate = (r: typeof rows) => (r.length ? r.filter((x) => x.hit).length / r.length : null);
const byType = Object.fromEntries([...new Set(rows.map((r) => r.type))].sort().map((t) => [t, { n: rows.filter((r) => r.type === t).length, recall: rate(rows.filter((r) => r.type === t)) }]));
const report = { split: a.split, system: a.system, profile: a.profile, describe: described, k, questions: rows.length, evidenceRecallAtK: rate(rows), byType, rows };
if (a.out) writeFileSync(a.out, JSON.stringify(report, null, 1));
console.log(JSON.stringify({ profile: a.profile, k, questions: rows.length, evidenceRecallAtK: report.evidenceRecallAtK, byType }, null, 1));
