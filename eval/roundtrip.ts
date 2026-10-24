// Export and import round trip through the Rust build: a fixture with versions, corrections,
// valid time, invalidation, deletion, grants, a revocation and a relation goes into stack A; it is
// exported, imported into stack B (same embedder, re-embedded on import) and into stack C (BM25 only,
// no graph); the same calls as the same principals must give the same answers.
//   bun eval/roundtrip.ts [--out eval/results/roundtrip.json]
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { Pool } from "pg";

import { startService } from "./hyperag-http";

const { values: a } = parseArgs({
  options: { out: { type: "string", default: "eval/results/roundtrip.json" } },
});
const PG =
  process.env.HYPERAG_PG_URL ??
  "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const NEO4J = {
  url: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474",
  user: process.env.HYPERAG_NEO4J_USER ?? "neo4j",
  password: process.env.HYPERAG_NEO4J_PASSWORD ?? "fake-test-only",
};
const BIN = "target/release/hyperag";
process.env.HYPERAG_SERVICE_CMD ??= `${BIN} serve`;
const E5 =
  "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean";
const principals = ["admin", "alice", "bob", "carol", "dave"];
const tag = `rs_rt${Date.now().toString(36)}`;
const eng = `${tag}-eng`;
const fin = `${tag}-fin`;
const dbUrl = (db: string) => PG.replace(/\/[^/]+$/u, `/${db}`);

const admin = new Pool({ connectionString: PG, max: 1 });
for (const db of ["a", "b", "c"])
  await admin.query(`create database ${tag}_${db}`);

// The CLI reports on stderr; a failed command stops the run.
const cli = (db: string, args: string[], env: Record<string, string>) => {
  const r = spawnSync(BIN, args, {
    env: {
      ...process.env,
      HYPERAG_PG_URL: dbUrl(db),
      HYPERAG_RERANKER: "none",
      HYPERAG_ADMINS: "admin",
      ...env,
    },
    encoding: "utf8",
  });
  if (r.status !== 0) throw new Error(`hyperag ${args[0]}: ${r.stderr}`);
  return r.stderr;
};
const graphEnv = {
  HYPERAG_NEO4J_URL: NEO4J.url,
  HYPERAG_NEO4J_USER: NEO4J.user,
  HYPERAG_NEO4J_PASSWORD: NEO4J.password,
};

// ---- stack A: the fixture ----
const A = await startService({
  pgUrl: dbUrl(`${tag}_a`),
  principals,
  admins: ["admin"],
  neo4j: true,
  embedding: E5,
  reranker: "none",
  env: graphEnv,
});
for (const [p, s, perm] of [
  ["alice", eng, "write"],
  ["alice", eng, "read"],
  ["bob", eng, "read"],
  ["carol", fin, "write"],
  ["carol", fin, "read"],
  ["dave", eng, "read"],
] as const)
  await A.must("admin", "POST", "/v1/grants", {
    principal: p,
    scope: s,
    permission: perm,
  });
await A.must("admin", "DELETE", "/v1/grants", {
  principal: "dave",
  scope: eng,
  permission: "read",
});
const put = async (as: string, body: Record<string, unknown>) =>
  String((await A.must(as, "POST", "/v1/memories", body)).id);
const deploy = await put("alice", {
  scope: eng,
  source: "runbook",
  text: "Deploys use blue-green on Fridays.",
  validFrom: "2026-01-01T00:00:00Z",
});
const incident = await put("alice", {
  scope: eng,
  source: "postmortem",
  text: "Incident INC-7 root cause: a disk filled up.",
});
await A.must("alice", "PATCH", `/v1/memories/${encodeURIComponent(incident)}`, {
  text: "Incident INC-7 root cause: an expired TLS certificate.",
  source: "postmortem",
  expectedVersion: 1,
});
const deploy2 = await put("alice", {
  scope: eng,
  source: "runbook",
  text: "Deploys use canary releases on Tuesdays.",
  supersedes: [deploy],
});
const budget = await put("carol", {
  scope: fin,
  source: "plan",
  text: "The Q3 budget is 40k.",
  validTo: "2026-06-30T00:00:00Z",
});
const gone = await put("alice", {
  scope: eng,
  source: "chat",
  text: "Temporary note about the staging cache.",
});
await A.must(
  "alice",
  "DELETE",
  `/v1/memories/${encodeURIComponent(gone)}?reason=roundtrip`,
);
const wrong = await put("alice", {
  scope: eng,
  source: "chat",
  text: "The on-call phone is in drawer 3.",
});
await A.must(
  "alice",
  "POST",
  `/v1/memories/${encodeURIComponent(wrong)}/invalidate`,
  { reason: "roundtrip" },
);
await A.must("alice", "POST", "/v1/relations", {
  from: incident,
  to: deploy2,
  type: "AFFECTS",
});

// The same calls on every stack; every answer is kept whole (status and body).
type Svc = typeof A;
// Rust flattens history into the explain response; only relations depend on the graph slot.
const historyResponse = (r: { status: number; json: Record<string, unknown> }) => {
  const { relations: _relations, ...history } = r.json;
  return { status: r.status, history };
};
const probes = async (s: Svc, graph: boolean) => {
  const out: Record<string, unknown> = {};
  for (const as of ["alice", "bob", "carol", "dave"])
    for (const query of [
      "how do we deploy",
      "INC-7 root cause",
      "Q3 budget",
      "on-call phone",
      "staging cache",
    ]) {
      out[`retrieve ${as} ${query}`] = await s.call(
        as,
        "POST",
        "/v1/retrieve",
        { query, limit: 5 },
      );
      out[`retrieve ${as} ${query} asOf`] = await s.call(
        as,
        "POST",
        "/v1/retrieve",
        { query, limit: 5, asOf: "2026-03-01T00:00:00Z" },
      );
    }
  for (const id of [deploy, incident, deploy2, budget, gone, wrong])
    for (const as of ["alice", "carol"]) {
      const r = await s.call(
        as,
        "GET",
        `/v1/memories/${encodeURIComponent(id)}/explain`,
      );
      // Without a graph (stack C), only the history is compared.
      out[`explain ${as} ${id}`] = graph ? r : historyResponse(r);
    }
  return out;
};
const before = await probes(A, true);
await A.stop();

const dir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "hyperag-rt-"));
const file = join(dir, "export.jsonl");
const exported = cli(`${tag}_a`, ["export", "--out", file], {
  ...graphEnv,
  HYPERAG_EMBEDDING: "none",
});
const lines = readFileSync(file, "utf8").trim().split("\n");

// Neo4j holds every stack's graph by memory id, so A's nodes go before B imports them.
const neo = await fetch(`${NEO4J.url}/db/neo4j/query/v2`, {
  method: "POST",
  headers: {
    authorization: `Basic ${Buffer.from(`${NEO4J.user}:${NEO4J.password}`).toString("base64")}`,
    "content-type": "application/json",
  },
  body: JSON.stringify({
    statement:
      "MATCH (m:Memory) WHERE m.scope STARTS WITH $tag DETACH DELETE m",
    parameters: { tag },
  }),
});
if (!neo.ok) throw new Error(`neo4j cleanup: ${neo.status}`);

// ---- stack B: same embedder, graph on ----
const importedB = cli(`${tag}_b`, ["import", file, "--reranker", "none", "--as", "admin"], {
  ...graphEnv,
  HYPERAG_EMBEDDING: E5,
});
const importedPool = new Pool({ connectionString: dbUrl(`${tag}_b`), max: 1 });
const spaces = await importedPool.query("select id from embedding_spaces");
const space = String(spaces.rows[0]?.id ?? "");
if (!/^[a-f0-9]{16}$/u.test(space)) throw new Error("missing embedding space");
const vectorsB = Number((await importedPool.query(`select count(*) from vectors_${space}`)).rows[0].count);
await importedPool.end();
const reimport = cli(`${tag}_b`, ["import", file], {
  ...graphEnv,
  HYPERAG_EMBEDDING: E5,
});
const B = await startService({
  pgUrl: dbUrl(`${tag}_b`),
  principals,
  admins: ["admin"],
  neo4j: true,
  embedding: E5,
  reranker: "none",
  env: graphEnv,
});
const afterB = await probes(B, true);
await B.stop();

// ---- stack C: the user picks BM25 only and no graph on import ----
const importedC = cli(
  `${tag}_c`,
  ["import", file, "--embedding", "none", "--as", "admin", "--without-graph"],
  { HYPERAG_EMBEDDING: E5 },
);
const C = await startService({
  pgUrl: dbUrl(`${tag}_c`),
  principals,
  admins: ["admin"],
  neo4j: false,
  embedding: "none",
  reranker: "none",
  env: { HYPERAG_NEO4J_URL: "" },
});
const afterC = await probes(C, false);
await C.stop();

const diff = (x: Record<string, unknown>, y: Record<string, unknown>) =>
  Object.keys(x).filter((k) => JSON.stringify(x[k]) !== JSON.stringify(y[k]));
const historyOnly = Object.fromEntries(
  Object.entries(before)
    .filter(([k]) => k.startsWith("explain"))
    .map(([k, v]) => {
      const r = v as { status: number; json: Record<string, unknown> };
      return [k, historyResponse(r)];
    }),
);
const checks: Array<[string, boolean, unknown]> = [
  ["reranker-only import fills the active embedding space", vectorsB === 5, { vectors: vectorsB }],
  [
    "B answers equal A (every retrieve and explain)",
    diff(before, afterB).length === 0,
    diff(before, afterB),
  ],
  [
    "C keeps every history (BM25 only, no graph)",
    diff(
      historyOnly,
      Object.fromEntries(
        Object.entries(afterC).filter(([k]) => k.startsWith("explain")),
      ),
    ).length === 0,
    null,
  ],
  [
    "re-import is a no-op",
    /"memories":0/u.test(reimport) && /"skipped":6/u.test(reimport),
    reimport.trim(),
  ],
  [
    "the export holds no token or secret",
    !/token|password|secret/iu.test(readFileSync(file, "utf8")),
    null,
  ],
];
for (const db of ["a", "b", "c"])
  await admin.query(`drop database ${tag}_${db}`);
await admin.end();

const report = {
  sourceCommit: spawnSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).stdout.trim(),
  measuredAt: new Date().toISOString(),
  build: "rust",
  probes: { sameModels: Object.keys(before).length, historyOnly: Object.keys(historyOnly).length },
  tag,
  records: lines.length,
  exported: exported.trim(),
  importedB: importedB.trim(),
  importedC: importedC.trim(),
  checks: checks.map(([name, pass, detail]) => ({
    name,
    pass,
    ...(pass ? {} : { detail }),
  })),
};
writeFileSync(a.out!, JSON.stringify(report, null, 2));
for (const c of report.checks)
  console.log(`${c.pass ? "ok  " : "FAIL"} ${c.name}`);
process.exit(report.checks.every((c) => c.pass) ? 0 : 1);
