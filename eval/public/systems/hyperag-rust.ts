import { spawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { Pool } from "pg";

import { type Conversation, newConversation, sessionEdges } from "../conversation-graph";
import { memoryBatches } from "../memory-batches";
import { containerBytes } from "../rss";
import type { MemorySystem, SystemFactory } from "../system";
import { ensureBenchmarkDatabase } from "./postgres-db";

/**
 * Hyperag's Rust build through its HTTP service, the product as users run it (the `hyperag`
 * adapter drives the frozen TypeScript library). For the memory benchmarks: the service caps a
 * search at 50 results, so BEIR stays on the `hyperag` adapter.
 *
 * Profiles: an embedder (`egemma2`, `e5`, `bm25`), then any of `+bgev2m3` or `+rerank` (re-ranker),
 * `+graph` (edges by the rules in `conversation-graph.ts`), `+temporal` (the temporal channel,
 * `docs/temporal.md`; each hit's text gets its resolved dates) and `+nodates` (the channel without
 * the dates in the text, for the ablation). The binary is `HYPERAG_BIN` (default `target/release/hyperag`); build it
 * from the commit under test.
 */
const EMBEDDINGS: Record<string, string> = {
  egemma2: "hf:onnx-community/embeddinggemma-2-ONNX?revision=daa72c51243991dfcaf9f9137d2c573d8f7790c0&dtype=q8&pooling=none&output=sentence_embedding&dims=768",
  e5: "hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean",
  bm25: "none",
};
const RERANKERS: Record<string, string> = {
  bgev2m3: "hf:onnx-community/bge-reranker-v2-m3-ONNX?revision=6f5ff65298512715a1e669753bc754d2bc8f367b&dtype=q8",
  rerank: "hf:Xenova/bge-reranker-base?revision=280bcc27a84e0b898c251e06fddb25171bd9b101&dtype=q8",
};
const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const NEO4J = { url: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474", user: process.env.HYPERAG_NEO4J_USER ?? "neo4j", password: process.env.HYPERAG_NEO4J_PASSWORD ?? "fake-test-only" };

type Service = { base: string; child: ChildProcess };

const create = async (profile: string): Promise<MemorySystem> => {
  const [base, ...extras] = profile.split("+");
  if (!base || !(base in EMBEDDINGS)) throw new Error(`unknown hyperag-rust profile ${profile}`);
  const rerankers = extras.filter((x) => x in RERANKERS);
  if (rerankers.length > 1) throw new Error(`hyperag-rust takes one re-ranker, got ${rerankers.join(", ")}`);
  const rerankName = rerankers[0];
  const graph = extras.includes("graph");
  const temporal = extras.includes("temporal");
  if (extras.includes("nodates") && !temporal) throw new Error("+nodates needs +temporal");
  const dates = temporal && !extras.includes("nodates");
  const unknown = extras.filter((x) => !(x in RERANKERS) && !["graph", "temporal", "nodates"].includes(x));
  if (unknown.length) throw new Error(`unknown hyperag-rust extras ${unknown.join(", ")}`);

  // One tokens file for every service: the service re-reads it when it changes, so each reader
  // principal (one per conversation) gets its token when its namespace opens.
  const dir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "hyperag-rust-"));
  const tokensFile = join(dir, "tokens.json");
  const tokens: Record<string, string> = {};
  const tokenOf = (principal: string) => `bench-${createHash("sha256").update(dir + principal).digest("hex").slice(0, 32)}`;
  const addToken = (principal: string) => {
    tokens[createHash("sha256").update(tokenOf(principal)).digest("hex")] = principal;
    writeFileSync(tokensFile, JSON.stringify(tokens));
  };
  addToken("bench");

  const services = new Map<string, Promise<Service>>();
  const conversations = new Map<string, Conversation>();
  const opened = new Set<string>();
  // Ids sent per namespace in this run. LongMemEval-S repeats a session id with other dates in 13 of
  // 500 questions; the first copy is kept, as the `hyperag` adapter does by skipping stored ids.
  const sent = new Map<string, Set<string>>();
  const split = (ns: string) => {
    const at = ns.indexOf("/");
    return at < 0 ? { group: ns, scope: "corpus" } : { group: ns.slice(0, at), scope: ns };
  };
  const reader = (scope: string) => (scope === "corpus" ? "bench" : `reader:${scope}`);

  const start = async (group: string): Promise<Service> => {
    // Its own database per set and embedding family, never a `pub_*` name of the `hyperag` adapter:
    // `rs_*` for benchmark rows, `HYPERAG_DB_PREFIX=dev` for the dev checks.
    const db = `${process.env.HYPERAG_DB_PREFIX ?? "rs"}_${group.replace(/\W/gu, "_")}_${base}`.toLowerCase().slice(0, 60);
    const admin = new Pool({ connectionString: PG, max: 1 });
    try {
      await ensureBenchmarkDatabase(admin, db);
    } finally {
      await admin.end();
    }
    // A port the OS reports free, so two services in one run never share one.
    const port = await new Promise<number>((resolve, reject) => {
      const probe = createServer().listen(0, "127.0.0.1", () => {
        const p = (probe.address() as { port: number }).port;
        probe.close(() => resolve(p));
      });
      probe.on("error", reject);
    });
    const child = spawn(process.env.HYPERAG_BIN ?? "target/release/hyperag", ["serve"], {
      stdio: ["ignore", "ignore", "inherit"],
      env: {
        ...process.env,
        HYPERAG_PG_URL: PG.replace(/\/[^/]+$/u, `/${db}`),
        HYPERAG_PORT: String(port),
        HYPERAG_TOKENS_FILE: tokensFile,
        HYPERAG_ADMINS: "bench",
        HYPERAG_EMBEDDING: EMBEDDINGS[base]!,
        HYPERAG_RERANKER: rerankName ? RERANKERS[rerankName]! : "none",
        ...(graph ? { HYPERAG_NEO4J_URL: NEO4J.url, HYPERAG_NEO4J_USER: NEO4J.user, HYPERAG_NEO4J_PASSWORD: NEO4J.password } : { HYPERAG_NEO4J_URL: "" }),
      },
    });
    const svc = { base: `http://127.0.0.1:${port}`, child };
    for (let i = 0; i < 1200; i++) {
      if (child.exitCode !== null) throw new Error(`hyperag serve exited with ${child.exitCode}`);
      const r = await fetch(`${svc.base}/v1/whoami`, { headers: { authorization: `Bearer ${tokenOf("bench")}` } }).catch(() => null);
      if (r?.ok) return probeChannels(svc);
      await Bun.sleep(250);
    }
    throw new Error("hyperag serve did not start");
  };
  // Every channel the profile names must be live in this binary, or the profile is refused: a
  // service built without the temporal channel ignores `channels.temporal` without a word.
  const probeChannels = async (svc: Service) => {
    const r = (await call("bench", svc, "POST", "/v1/retrieve", { query: "what happened yesterday", limit: 1, channels: { bm25: true, vector: base !== "bm25", graph, rerank: false, temporal } })) as { pathsUsed: string[]; limitations: string[] };
    const missing = [...(base !== "bm25" ? ["vector"] : []), ...(graph ? ["graph"] : []), ...(temporal ? ["temporal"] : [])].filter((c) => !r.pathsUsed.includes(c));
    const models = (await call("bench", svc, "GET", "/v1/models")) as { reranker?: string | null };
    if (rerankName && models.reranker !== RERANKERS[rerankName]) missing.push(`rerank (loaded: ${models.reranker ?? "none"})`);
    // A build without the `anchor` option ignores it; this one refuses a bad anchor.
    if (temporal) {
      const bad = await fetch(`${svc.base}/v1/retrieve`, { method: "POST", headers: { authorization: `Bearer ${tokenOf("bench")}`, "content-type": "application/json" }, body: JSON.stringify({ query: "yesterday", limit: 1, anchor: "not-a-date", channels: { temporal: true } }) });
      if (bad.status !== 400) missing.push("temporal anchor");
    }
    if (missing.length) {
      svc.child.kill();
      throw new Error(`hyperag-rust ${profile}: channel(s) ${missing.join(", ")} not live in ${process.env.HYPERAG_BIN ?? "target/release/hyperag"} (${r.limitations.join("; ")})`);
    }
    return svc;
  };
  const service = (group: string) => {
    let s = services.get(group);
    if (!s) services.set(group, (s = start(group)));
    return s;
  };
  // A Postgres restart (a few seconds) fails the requests in flight with a 500: retry for up to 30 s.
  // Every call is safe to repeat: writes are idempotent by id, relations are merges, reads are reads.
  const call = async (as: string, svc: Service, method: string, path: string, body?: unknown) => {
    for (const started = Date.now(); ; ) {
      const r = await fetch(svc.base + path, {
        method,
        headers: { authorization: `Bearer ${tokenOf(as)}`, "content-type": "application/json" },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        signal: AbortSignal.timeout(600_000),
      });
      const json = (await r.json().catch(() => ({}))) as Record<string, unknown>;
      if (r.ok) return json;
      if (r.status < 500 || Date.now() - started > 30_000) throw new Error(`${method} ${path}: ${r.status} ${JSON.stringify(json).slice(0, 300)}`);
      await Bun.sleep(1_000);
    }
  };
  const open = async (ns: string) => {
    const { group, scope } = split(ns);
    const svc = await service(group);
    if (!opened.has(scope)) {
      addToken(reader(scope));
      await call("bench", svc, "POST", "/v1/grants", { principal: "bench", scope, permission: "write" });
      // relate() reads both ends, so the writer also reads; each conversation has its own reader.
      await call("bench", svc, "POST", "/v1/grants", { principal: "bench", scope, permission: "read" });
      await call("bench", svc, "POST", "/v1/grants", { principal: reader(scope), scope, permission: "read" });
      opened.add(scope);
    }
    return { svc, scope };
  };
  const local = (scope: string, id: string) => `${scope}/${id.replaceAll("/", "%2F")}`;

  return {
    rssBytes: async () => containerBytes([process.env.HYPERAG_PG_CONTAINER ?? "hyperag-postgres-1"]),
    describe: async () => ({
      system: "hyperag-rust",
      version: "0.1.0",
      profile,
      config: {
        embedding: EMBEDDINGS[base],
        reranker: rerankName ? RERANKERS[rerankName] : null,
        graph: graph ? "conversation-graph.ts: NEXT_TURN, SAME_ENTITY (exact match); 2 hops" : null,
        temporal: temporal ? `docs/temporal.md: date rules, temporal list in RRF${dates ? ", dates in each hit's text" : ""}` : null,
        asOf: temporal ? "anchor (retrieve option anchor: resolves relative dates in the question; filters nothing)" : "none (only the temporal channel reads a date)",
        rrfK: 60,
      },
    }),
    open: async (ns) => void (await open(ns)),
    ingest: async (ns, docs) => {
      const { svc, scope } = await open(ns);
      const seen = sent.get(ns) ?? new Set<string>();
      sent.set(ns, seen);
      // Marked as they pass, so a repeat inside one session is skipped too.
      const fresh = docs.filter((d) => !seen.has(d.id) && Boolean(seen.add(d.id)));
      const memories = fresh.map((d) => ({
        scope,
        id: d.id.replaceAll("/", "%2F"),
        source: d.speaker ?? "corpus",
        text: d.speaker ? d.text : `${d.title ? `${d.title}\n` : ""}${d.text}`,
        ...(d.timestamp ? { metadata: { timestamp: d.timestamp } } : {}),
      }));
      // Ingest is idempotent by id, so a resumed run re-sends a session without duplicating it.
      for (const batch of memoryBatches(memories)) await call("bench", svc, "POST", "/v1/memories/batch", { memories: batch });
      if (graph && scope !== "corpus") {
        const c = conversations.get(ns) ?? newConversation();
        conversations.set(ns, c);
        for (const e of sessionEdges(c, fresh)) await call("bench", svc, "POST", "/v1/relations", { from: local(scope, e.from), to: local(scope, e.to), type: e.type });
      }
    },
    // `asOf` is the question's date (the runner's rule: a native parameter that anchors relative
    // dates, never a time filter). Hyperag takes it as `anchor`, used by the temporal channel only.
    search: async (ns, query, k, opts?: Readonly<{ asOf?: string }>) => {
      const { svc, scope } = await open(ns);
      const r = (await call(reader(scope), svc, "POST", "/v1/retrieve", {
        query,
        limit: Math.min(k, 50),
        candidatesPerChannel: Math.min(Math.max(24, k), 150),
        channels: { bm25: true, vector: base !== "bm25", graph, rerank: rerankName !== undefined, temporal },
        ...(temporal && opts?.asOf ? { anchor: opts.asOf } : {}),
      })) as { items: Array<{ memory: { id: string; text: string }; score: number; rerankScore?: number; dates?: string }>; limitations: string[] };
      // A failed channel is an error, so a row never silently runs without a channel it names.
      const failed = r.limitations.find((l) => / failed/u.test(l) || l.startsWith("no graph configured") || l.startsWith("no embedder configured"));
      if (failed) throw new Error(failed);
      return r.items.map((i) => ({ id: decodeURIComponent(i.memory.id.slice(scope.length + 1)), score: i.rerankScore ?? i.score, text: dates && i.dates ? `${i.memory.text} [${i.dates}]` : i.memory.text }));
    },
    close: async () => {
      for (const s of services.values()) (await s).child.kill();
    },
  };
};
/**
 * The runner opens one system per worker in one process. They share one instance per profile, so a
 * set gets one service (one model load, one schema migration) instead of one per worker; its
 * services stop when the last worker closes.
 */
const shared = new Map<string, { system: Promise<MemorySystem>; users: number }>();
const factory: SystemFactory = async (profile) => {
  const entry = shared.get(profile) ?? { system: create(profile), users: 0 };
  shared.set(profile, entry);
  entry.users++;
  const system = await entry.system;
  let closed = false;
  return {
    ...system,
    close: async () => {
      if (closed) return;
      closed = true;
      if (--entry.users === 0) {
        shared.delete(profile);
        await system.close();
      }
    },
  };
};
export default factory;
