// The spec's behaviours as pass/fail tasks, run against Hyperag and against Engram through the same
// task list. A system that cannot express a step fails it, with the reason recorded.
//   bun eval/scenarios.ts --out eval/results/scenarios.json
import { writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { Pool } from "pg";

import { createLocalEmbedder, createLocalReranker } from "../src/adapters/embedders";
import { createNeo4jGraph } from "../src/adapters/neo4j";
import { createPostgresStore } from "../src/adapters/postgres";
import { Hyperag } from "../src/core";
import { engramMcp } from "./engram-mcp";
import { startService } from "./hyperag-http";

const { values: args } = parseArgs({
  options: {
    out: { type: "string", default: "eval/results/scenarios.json" },
    // in-process (the TypeScript library) or http (any build's service, via HYPERAG_SERVICE_CMD).
    hyperag: { type: "string", default: "in-process" },
    build: { type: "string", default: "ts" },
    systems: { type: "string", default: "hyperag,engram,engram-any" },
  },
});

class Unsupported extends Error {}

/** What every system under test must offer the tasks. `agent` is who acts. */
type System = {
  name: string;
  restart(): Promise<void>;
  write(agent: string, scope: string, text: string, source: string, opts?: { supersedes?: string; validTo?: string }): Promise<string>;
  update(agent: string, id: string, text: string, expectedVersion: number): Promise<"ok" | "conflict">;
  search(agent: string, scope: string, query: string, opts?: { asOf?: string }): Promise<{ texts: string[]; sources: string[]; abstained: boolean | null }>;
  remove(agent: string, id: string): Promise<void>;
  allow(agent: string, scope: string, perm: "read" | "write"): Promise<void>;
  revoke(agent: string, scope: string): Promise<void>;
  history(agent: string, id: string): Promise<string[]>;
  close(): Promise<void>;
};

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";

async function hyperagSystem(): Promise<System> {
  const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}eval_scenarios_${Date.now()}`;
  const admin = new Pool({ connectionString: PG, max: 1 });
  await admin.query(`create database ${name}`);
  const url = PG.replace(/\/[^/]+$/u, `/${name}`);
  const embedder = createLocalEmbedder();
  const reranker = createLocalReranker();
  const graph = createNeo4jGraph({ baseUrl: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474", username: "neo4j", password: "fake-test-only" });
  let pool: Pool;
  let h: Hyperag;
  const open = async () => {
    pool = new Pool({ connectionString: url, max: 4 });
    h = new Hyperag({ store: createPostgresStore(pool), embedder, reranker, graph, admins: ["admin"] });
    await h.init();
  };
  await open();
  const tag = Date.now().toString(36);
  const sc = (s: string) => `${s}-${tag}`;
  return {
    name: "hyperag",
    restart: async () => {
      await pool.end();
      await open();
    },
    write: async (agent, scope, text, source, o = {}) =>
      (await h.ingest(agent, { scope: sc(scope), text, source, ...(o.supersedes ? { supersedes: [o.supersedes] } : {}), ...(o.validTo ? { validTo: o.validTo } : {}) })).id,
    update: async (agent, id, text, expectedVersion) => {
      try {
        await h.update(agent, id, { text, source: "update", expectedVersion });
        return "ok";
      } catch (e) {
        if ((e as { code?: string }).code === "version_conflict") return "conflict";
        throw e;
      }
    },
    search: async (agent, scope, query, o = {}) => {
      const r = await h.retrieve(agent, query, { limit: 5, ...(o.asOf ? { asOf: o.asOf } : {}) });
      const own = r.items.filter((i) => i.memory.scope === sc(scope) || scope === "*");
      return { texts: (scope === "*" ? r.items : own).map((i) => i.memory.text), sources: r.items.map((i) => i.memory.source), abstained: r.insufficientEvidence };
    },
    remove: (agent, id) => h.delete(agent, id, "scenario"),
    allow: (agent, scope, perm) => h.grant("admin", agent, sc(scope), perm),
    revoke: (agent, scope) => h.revoke("admin", agent, sc(scope), "read"),
    history: async (agent, id) => (await h.explain(agent, id)).versions.map((v) => `${v.status}:${v.text}`),
    close: async () => {
      await pool.end();
      await admin.query(`drop database ${name} with (force)`);
      await admin.end();
    },
  };
}

/** Hyperag through its HTTP service: the same tasks for the TypeScript and the Rust build. */
async function hyperagHttpSystem(): Promise<System> {
  const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}eval_scenarios_http_${Date.now()}`;
  const admin = new Pool({ connectionString: PG, max: 1 });
  await admin.query(`create database ${name}`);
  const models =
    args.build === "rust"
      ? { embedding: "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean", reranker: "hf:Xenova/bge-reranker-base?revision=280bcc27a84e0b898c251e06fddb25171bd9b101&dtype=q8" }
      : { env: { HYPERAG_EMBEDDER: "local", HYPERAG_RERANKER: "local" } };
  const svc = await startService({
    pgUrl: PG.replace(/\/[^/]+$/u, `/${name}`),
    principals: ["admin", "coder", "alpha", "beta", "gamma", "cfo"],
    admins: ["admin"],
    neo4j: true,
    ...models,
  });
  const tag = Date.now().toString(36);
  const sc = (s: string) => `${s}-${tag}`;
  return {
    name: `hyperag-http-${args.build}`,
    restart: () => svc.restart(),
    write: async (agent, scope, text, source, o = {}) =>
      String((await svc.must(agent, "POST", "/v1/memories", { scope: sc(scope), text, source, ...(o.supersedes ? { supersedes: [o.supersedes] } : {}), ...(o.validTo ? { validTo: o.validTo } : {}) })).id),
    update: async (agent, id, text, expectedVersion) => {
      const r = await svc.call(agent, "PATCH", `/v1/memories/${encodeURIComponent(id)}`, { text, source: "update", expectedVersion });
      if (r.status === 200) return "ok";
      if (r.json.error?.code === "version_conflict") return "conflict";
      throw new Error(JSON.stringify(r.json));
    },
    search: async (agent, scope, query, o = {}) => {
      const r = (await svc.must(agent, "POST", "/v1/retrieve", { query, limit: 5, ...(o.asOf ? { asOf: o.asOf } : {}) })) as unknown as {
        items: Array<{ memory: { text: string; source: string; scope: string } }>;
        insufficientEvidence: boolean;
      };
      return {
        texts: r.items.filter((i) => i.memory.scope === sc(scope)).map((i) => i.memory.text),
        sources: r.items.map((i) => i.memory.source),
        abstained: r.insufficientEvidence,
      };
    },
    remove: async (agent, id) => void (await svc.must(agent, "DELETE", `/v1/memories/${encodeURIComponent(id)}?reason=scenario`)),
    allow: async (agent, scope, perm) => void (await svc.must("admin", "POST", "/v1/grants", { principal: agent, scope: sc(scope), permission: perm })),
    revoke: async (agent, scope) => void (await svc.must("admin", "DELETE", "/v1/grants", { principal: agent, scope: sc(scope), permission: "read" })),
    history: async (agent, id) =>
      ((await svc.must(agent, "GET", `/v1/memories/${encodeURIComponent(id)}/explain`)) as unknown as { versions: Array<{ status: string; text: string }> }).versions.map(
        (v) => `${v.status}:${v.text}`,
      ),
    close: async () => {
      await svc.stop();
      await admin.query(`drop database ${name} with (force)`);
      await admin.end();
    },
  };
}

/**
 * Engram as agents on this machine use it: its MCP server (`engram mcp`, one per project) and the
 * mem_save / mem_search / mem_update / mem_delete / mem_get_observation tools, in a throwaway data
 * directory. A scope is a project. A correction reuses the corrected memory's topic_key, which is
 * how Engram replaces a fact. Engram has no principals, so `agent` is ignored and access rules
 * cannot be expressed. `matchMode` undefined is Engram's default (all words must match).
 */
async function engramSystem(matchMode?: "any"): Promise<System> {
  const e = engramMcp(matchMode);
  const meta = new Map<string, { scope: string; topic: string }>();
  let n = 0;
  return {
    name: matchMode ? `engram-mcp-${matchMode}` : "engram-mcp-default",
    restart: () => e.restart(),
    write: async (_agent, scope, text, source, o = {}) => {
      if (o.validTo) throw new Unsupported("no valid-time fields");
      const topic = o.supersedes ? meta.get(o.supersedes)!.topic : `scenario/t${n++}`;
      const id = String(await e.save(scope, source, text, topic));
      meta.set(id, { scope, topic });
      return id;
    },
    update: async (_agent, id, text) => {
      // mem_update has no expected-version check: the last writer wins without a conflict.
      await e.update(meta.get(id)!.scope, Number(id), text);
      return "ok";
    },
    search: async (_agent, scope, query) => {
      const rows = await e.search(scope, query, 5);
      return { texts: rows.map((r) => r.text), sources: rows.map((r) => r.title), abstained: null };
    },
    remove: (_agent, id) => e.remove(meta.get(id)!.scope, Number(id)),
    allow: async () => undefined,
    revoke: async () => {
      throw new Unsupported("no per-principal permissions: any caller can read any project");
    },
    history: (_agent, id) => e.history(meta.get(id)!.scope, Number(id)),
    close: () => e.close(),
  };
}

type Outcome = { task: string; pass: boolean; note: string };
const TASKS: Array<[string, (s: System) => Promise<[boolean, string]>]> = [
  ["retention across restart (en)", async (s) => {
    await s.allow("coder", "repo", "read"); await s.allow("coder", "repo", "write");
    await s.write("coder", "repo", "Decision in PR #88: migrations run in a single transaction per file.", "PR #88");
    await s.restart();
    const r = await s.search("coder", "repo", "how do we run migrations per file?");
    return [r.texts[0]?.includes("single transaction") === true, `top: ${r.texts[0]?.slice(0, 50)}`];
  }],
  ["retention across restart (es)", async (s) => {
    await s.write("coder", "repo", "Decisión en el PR #91: los reintentos usan backoff exponencial con tope de 30 segundos.", "PR #91");
    await s.restart();
    const r = await s.search("coder", "repo", "¿qué política de reintentos usamos?");
    return [r.texts[0]?.includes("backoff") === true, `top: ${r.texts[0]?.slice(0, 50)}`];
  }],
  ["handoff: finding and its evidence reach the second agent", async (s) => {
    for (const a of ["alpha", "beta"]) { await s.allow(a, "team", "read"); await s.allow(a, "team", "write"); }
    await s.write("alpha", "team", "Evidence: load test 14 shows p95 rising to 900 ms when the connection pool is 5.", "load test 14");
    await s.write("alpha", "team", "Finding: raise the connection pool from 5 to 20 to fix the p95 latency regression.", "alpha analysis");
    const r = await s.search("beta", "team", "why did p95 latency regress and what fixes it?");
    const both = r.texts.some((t) => t.includes("Finding")) && r.texts.some((t) => t.includes("Evidence"));
    return [both, `${r.texts.length} results`];
  }],
  ["correction supersedes an outdated fact", async (s) => {
    const old = await s.write("alpha", "team", "The billing service runs on port 8080.", "wiki 2025");
    await s.write("beta", "team", "Correction: the billing service runs on port 9090, not 8080.", "wiki 2026", { supersedes: old });
    const r = await s.search("alpha", "team", "which port does the billing service run on?");
    const pass = r.texts[0]?.includes("9090") === true && !r.texts.some((t) => t === "The billing service runs on port 8080.");
    return [pass, `results: ${r.texts.map((t) => t.slice(0, 30)).join(" | ")}`];
  }],
  ["a correction still holds after a restart", async (s) => {
    await s.restart();
    const r = await s.search("beta", "team", "which port does the billing service run on?");
    return [r.texts[0]?.includes("9090") === true && !r.texts.some((t) => t === "The billing service runs on port 8080."), `top: ${r.texts[0]?.slice(0, 40)}`];
  }],
  ["conflicting concurrent update is detected", async (s) => {
    const id = await s.write("alpha", "team", "The release owner is Dana.", "roster");
    const [x, y] = await Promise.all([s.update("alpha", id, "The release owner is Eli.", 1), s.update("beta", id, "The release owner is Fay.", 1)]);
    return [[x, y].includes("conflict"), `outcomes: ${x}, ${y}`];
  }],
  ["history explains a correction", async (s) => {
    const id = await s.write("alpha", "team", "Standup is at 09:30.", "calendar");
    await s.update("alpha", id, "Standup is at 10:00.", 1);
    const h = await s.history("alpha", id);
    return [h.length >= 2 && h.some((v) => v.includes("09:30")), `versions: ${h.length}`];
  }],
  ["restricted scope is not reachable", async (s) => {
    await s.allow("cfo", "finance", "read"); await s.allow("cfo", "finance", "write");
    await s.write("cfo", "finance", "Confidential: the acquisition offer for Orbit Labs is 12 million.", "board memo");
    // beta has no grant on finance and asks for it by name.
    const r = await s.search("beta", "finance", "acquisition offer Orbit Labs");
    return [r.texts.length === 0, `${r.texts.length} results returned to an unauthorized agent`];
  }],
  ["revoked access stops reads", async (s) => {
    await s.allow("gamma", "team", "read");
    await s.revoke("gamma", "team");
    const r = await s.search("gamma", "team", "billing service port");
    return [r.texts.length === 0, `${r.texts.length} results after revocation`];
  }],
  ["deleted memory is not returned", async (s) => {
    const id = await s.write("alpha", "team", "Temporary token for the staging bucket: tok-7731.", "chat");
    await s.remove("alpha", id);
    const r = await s.search("alpha", "team", "staging bucket temporary token tok-7731");
    return [!r.texts.some((t) => t.includes("tok-7731")), `${r.texts.length} results`];
  }],
  ["insufficient evidence is reported", async (s) => {
    const r = await s.search("alpha", "team", "what is the boiling point of tungsten?");
    // Engram has no signal; an empty result is counted as abstaining, in its favour.
    const abstained = r.abstained ?? r.texts.length === 0;
    return [abstained, `abstained=${r.abstained}, results=${r.texts.length}`];
  }],
  ["valid time: an expired fact is not current", async (s) => {
    await s.write("alpha", "team", "The office wifi password policy rotates monthly.", "it policy", { validTo: "2026-01-01T00:00:00Z" });
    const r = await s.search("alpha", "team", "office wifi password policy", { asOf: "2026-10-23T00:00:00Z" });
    return [!r.texts.some((t) => t.includes("wifi")), `${r.texts.length} results`];
  }],
  ["answer carries its source", async (s) => {
    const r = await s.search("coder", "repo", "how do we run migrations per file?");
    return [r.sources.includes("PR #88"), `sources: ${r.sources.slice(0, 3).join(", ")}`];
  }],
];

const report: Record<string, Outcome[]> = {};
const makers: Record<string, () => Promise<System>> = {
  hyperag: args.hyperag === "http" ? hyperagHttpSystem : hyperagSystem,
  engram: () => engramSystem(),
  "engram-any": () => engramSystem("any"),
};
for (const make of args.systems!.split(",").map((n) => makers[n]!)) {
  const s = await make();
  const outcomes: Outcome[] = [];
  for (const [task, run] of TASKS) {
    try {
      const [pass, note] = await run(s);
      outcomes.push({ task, pass, note });
    } catch (e) {
      outcomes.push({ task, pass: false, note: e instanceof Unsupported ? `unsupported: ${e.message}` : `error: ${String(e).slice(0, 120)}` });
    }
  }
  await s.close();
  report[s.name] = outcomes;
  console.log(`${s.name}: ${outcomes.filter((o) => o.pass).length}/${outcomes.length}`);
}
writeFileSync(args.out!, JSON.stringify(report, null, 2));
const names = Object.keys(report);
console.log(`| task | ${names.join(" | ")} |`);
for (const [i, [task]] of TASKS.entries())
  console.log(`| ${task} | ${names.map((n) => (report[n]?.[i]?.pass ? "pass" : `FAIL (${report[n]?.[i]?.note.slice(0, 60)})`)).join(" | ")} |`);
