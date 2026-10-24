import { afterAll, beforeAll, describe, expect, test } from "bun:test";

import { HyperagError } from "../src/types";
import { createNeo4jGraph } from "../src/adapters/neo4j";
import { freshDatabase, NEO4J, open, uniq } from "./helpers";

// Integration tests: they need the compose stack (`docker compose -p hyperag up -d`).
const db = await freshDatabase();
const team = uniq("team");
const alicePrivate = uniq("alice");
const finance = uniq("finance");
let a: Awaited<ReturnType<typeof open>>;

beforeAll(async () => {
  a = await open(db.url);
  const h = a.hyperag;
  for (const p of ["alice", "bob"]) {
    await h.grant("admin", p, team, "read");
    await h.grant("admin", p, team, "write");
  }
  await h.grant("admin", "alice", alicePrivate, "read");
  await h.grant("admin", "alice", alicePrivate, "write");
  await h.grant("admin", "carol", finance, "read");
  await h.grant("admin", "carol", finance, "write");
}, 60_000);

afterAll(async () => {
  await a.close();
  await db.drop();
});

describe("individual memory", () => {
  test("an agent retrieves its decision after a restart", async () => {
    await a.hyperag.ingest("alice", {
      scope: alicePrivate,
      kind: "decision",
      source: "PR #42",
      text: "Decision: the ingestion queue uses Postgres advisory locks instead of Redis.",
    });
    await a.close();
    a = await open(db.url); // a new pool and a new Hyperag, as after a process restart
    const r = await a.hyperag.retrieve("alice", "which lock does the ingestion queue use?");
    expect(r.items[0]?.memory.text).toContain("advisory locks");
    expect(r.items[0]?.memory.source).toBe("PR #42");
    expect(r.pathsUsed).toEqual(expect.arrayContaining(["bm25", "vector", "graph"]));
  });

  test("a retried write is idempotent", async () => {
    const input = { scope: alicePrivate, source: "notes", text: "Retry-safe fact about the cache TTL of 30 seconds." };
    const first = await a.hyperag.ingest("alice", input);
    const again = await a.hyperag.ingest("alice", input);
    expect(again).toEqual({ id: first.id, version: 1, created: false });
    const keyed = await a.hyperag.ingest("alice", { ...input, text: "Another fact.", idempotencyKey: "k1" });
    const replay = await a.hyperag.ingest("alice", { ...input, text: "Changed on retry.", idempotencyKey: "k1" });
    expect(replay.id).toBe(keyed.id);
    expect(replay.created).toBe(false);
  });
});

describe("shared memory", () => {
  test("two agents share a finding with supporting evidence", async () => {
    const evidence = await a.hyperag.ingest("alice", {
      scope: team,
      kind: "evidence",
      source: "ci run 981",
      text: "CI run 981: the flaky test checkout_spec fails only when TZ is not UTC.",
    });
    const finding = await a.hyperag.ingest("alice", {
      scope: team,
      kind: "finding",
      source: "alice analysis",
      text: "Finding: set TZ=UTC in the test runner to stop checkout_spec flaking.",
    });
    await a.hyperag.relate("alice", { from: finding.id, to: evidence.id, type: "SUPPORTED_BY" });
    const r = await a.hyperag.retrieve("bob", "how do we fix the flaky checkout test?");
    const ids = r.items.map((i) => i.memory.id);
    expect(ids).toContain(finding.id);
    expect(ids).toContain(evidence.id);
    const rels = await a.hyperag.relations("bob", finding.id);
    expect(rels).toEqual([expect.objectContaining({ from: finding.id, to: evidence.id, type: "SUPPORTED_BY", createdBy: "alice" })]);
  });

  test("a correction supersedes an earlier fact and the history explains it", async () => {
    const old = await a.hyperag.ingest("alice", {
      scope: team,
      source: "runbook v1",
      text: "The staging database port is 5433.",
    });
    const fix = await a.hyperag.ingest("bob", {
      scope: team,
      source: "runbook v2",
      text: "Correction: the staging database port is 6543, not 5433.",
      supersedes: [old.id],
    });
    const r = await a.hyperag.retrieve("alice", "staging database port");
    expect(r.items[0]?.memory.id).toBe(fix.id);
    expect(r.items.map((i) => i.memory.id)).not.toContain(old.id);
    const why = await a.hyperag.explain("alice", old.id);
    expect(why.versions[0]).toMatchObject({ status: "superseded", supersededBy: fix.id });
    expect(why.relations).toEqual([expect.objectContaining({ from: fix.id, to: old.id, type: "SUPERSEDES" })]);
  });

  test("two agents updating one memory: the stale writer gets version_conflict", async () => {
    const m = await a.hyperag.ingest("alice", { scope: team, source: "plan", text: "Release train leaves on Tuesday." });
    const [x, y] = await Promise.allSettled([
      a.hyperag.update("alice", m.id, { text: "Release train leaves on Wednesday.", source: "plan", expectedVersion: 1 }),
      a.hyperag.update("bob", m.id, { text: "Release train leaves on Thursday.", source: "plan", expectedVersion: 1 }),
    ]);
    const outcomes = [x, y].map((o) => (o.status === "fulfilled" ? "ok" : (o.reason as HyperagError).code)).sort();
    expect(outcomes).toEqual(["ok", "version_conflict"]);
    const history = await a.hyperag.explain("bob", m.id);
    expect(history.versions.map((v) => v.status)).toEqual(["superseded", "active"]);
  });
});

describe("authorization", () => {
  test("a query cannot reach restricted information", async () => {
    await a.hyperag.ingest("carol", { scope: finance, source: "ledger", text: "Q3 acquisition budget for Project Lynx is 4.2 million." });
    const r = await a.hyperag.retrieve("bob", "Project Lynx acquisition budget");
    expect(r.items.every((i) => i.memory.scope !== finance)).toBe(true);
    await expect(a.hyperag.ingest("bob", { scope: finance, source: "x", text: "y" })).rejects.toMatchObject({ code: "forbidden" });
  });

  test("an unrelated query reports insufficient evidence", async () => {
    const r = await a.hyperag.retrieve("bob", "what is the airspeed velocity of an unladen swallow");
    expect(r.insufficientEvidence).toBe(true);
    const ctx = await a.hyperag.context("bob", "what is the airspeed velocity of an unladen swallow");
    expect(ctx.citations).toEqual([]);
  });

  test("revoking a grant stops search and graph reads at once", async () => {
    const scope = uniq("handoff");
    await a.hyperag.grant("admin", "dave", scope, "read");
    await a.hyperag.grant("admin", "alice", scope, "write");
    await a.hyperag.grant("admin", "alice", scope, "read");
    const m = await a.hyperag.ingest("alice", { scope, source: "handoff", text: "Handoff note: rotate the vault token on Friday." });
    expect((await a.hyperag.retrieve("dave", "rotate the vault token")).items[0]?.memory.id).toBe(m.id);
    await a.hyperag.revoke("admin", "dave", scope, "read");
    expect((await a.hyperag.retrieve("dave", "rotate the vault token")).items).toEqual([]);
    await expect(a.hyperag.explain("dave", m.id)).rejects.toMatchObject({ code: "not_found" });
  });

  test("deletion removes the memory from search, graph and history text", async () => {
    const m = await a.hyperag.ingest("alice", { scope: team, source: "chat", text: "Temporary password hint: blue-falcon." });
    const other = await a.hyperag.ingest("alice", { scope: team, source: "chat", text: "Password hints must never be stored." });
    await a.hyperag.relate("alice", { from: other.id, to: m.id, type: "REFERS_TO" });
    await a.hyperag.delete("alice", m.id, "secret in memory");
    const r = await a.hyperag.retrieve("bob", "temporary password hint blue-falcon");
    expect(r.items.map((i) => i.memory.id)).not.toContain(m.id);
    const why = await a.hyperag.explain("alice", m.id);
    expect(why.deleted).toMatchObject({ by: "alice", reason: "secret in memory" });
    expect(why.versions).toEqual([]);
    expect(await a.hyperag.relations("alice", other.id)).toEqual([]);
    const graph = createNeo4jGraph(NEO4J);
    expect(await graph.expand([other.id], [team], 2, 10)).toEqual([]);
  });

  test("invalidated facts stop ranking but keep their history", async () => {
    const m = await a.hyperag.ingest("alice", { scope: team, source: "wiki", text: "The on-call rotation uses PagerDuty schedule ALPHA." });
    await a.hyperag.invalidate("bob", m.id, "schedule retired");
    const r = await a.hyperag.retrieve("alice", "PagerDuty schedule ALPHA on-call rotation");
    expect(r.items.map((i) => i.memory.id)).not.toContain(m.id);
    const why = await a.hyperag.explain("alice", m.id);
    expect(why.versions[0]?.status).toBe("invalidated");
    expect(why.versions[0]?.statusReason).toContain("schedule retired");
  });
});

describe("review fixes", () => {
  test("only an admin grants or revokes", async () => {
    await expect(a.hyperag.grant("bob", "bob", finance, "read")).rejects.toMatchObject({ code: "forbidden" });
    await expect(a.hyperag.revoke("bob", "carol", finance, "read")).rejects.toMatchObject({ code: "forbidden" });
  });

  test("writes cannot tell a missing id from an unreadable one", async () => {
    const hidden = await a.hyperag.ingest("carol", { scope: finance, source: "ledger", text: "Hidden payroll fact." });
    for (const call of [
      () => a.hyperag.update("bob", hidden.id, { text: "x", source: "x", expectedVersion: 1 }),
      () => a.hyperag.invalidate("bob", hidden.id, "x"),
      () => a.hyperag.delete("bob", hidden.id, "x"),
      () => a.hyperag.update("bob", "m_does_not_exist", { text: "x", source: "x", expectedVersion: 1 }),
    ])
      await expect(call()).rejects.toMatchObject({ code: "not_found" });
    // The same explicit id in two scopes is two memories.
    const x = await a.hyperag.ingest("carol", { scope: finance, id: "plan", source: "s", text: "Finance plan." });
    const y = await a.hyperag.ingest("bob", { scope: team, id: "plan", source: "s", text: "Team plan." });
    expect(x.id).not.toBe(y.id);
    await expect(a.hyperag.ingest("bob", { scope: team, id: "b/x", source: "s", text: "t" })).rejects.toMatchObject({ code: "invalid_input" });
  });

  test("explain hides ids the reader cannot read", async () => {
    const shared = uniq("shared");
    await a.hyperag.grant("admin", "carol", shared, "read");
    await a.hyperag.grant("admin", "carol", shared, "write");
    await a.hyperag.grant("admin", "bob", shared, "read");
    const old = await a.hyperag.ingest("carol", { scope: shared, source: "s", text: "Shared fact before correction." });
    await a.hyperag.ingest("carol", { scope: finance, source: "s", text: "Private correction.", supersedes: [old.id] });
    const why = await a.hyperag.explain("bob", old.id);
    expect(why.versions[0]?.supersededBy).toBe("[not readable]");
    expect(why.versions[0]?.statusReason).toBe("corrected");
    expect(why.relations).toEqual([]);
  });

  test("a corrected or invalidated fact is not revived by update or re-invalidated", async () => {
    const old = await a.hyperag.ingest("alice", { scope: team, source: "s", text: "Old fact to correct." });
    await a.hyperag.ingest("alice", { scope: team, source: "s", text: "New fact.", supersedes: [old.id] });
    await expect(a.hyperag.update("alice", old.id, { text: "Revived.", source: "s", expectedVersion: 1 })).rejects.toMatchObject({ code: "not_active" });
    await expect(a.hyperag.invalidate("alice", old.id, "x")).rejects.toMatchObject({ code: "not_active" });
    await expect(a.hyperag.ingest("alice", { scope: team, source: "s", text: "Second correction.", supersedes: [old.id] })).rejects.toMatchObject({ code: "not_active" });
    await expect(a.hyperag.ingest("alice", { scope: team, source: "s", text: "Bad date.", validFrom: "tuesday" })).rejects.toMatchObject({ code: "invalid_input" });
    expect((await a.hyperag.explain("alice", old.id)).versions[0]?.statusReason).toBe("corrected");
  });

  test("supersede racing an update never leaves the corrected fact active", async () => {
    const old = await a.hyperag.ingest("alice", { scope: team, source: "s", text: "Race target fact." });
    await Promise.allSettled([
      a.hyperag.update("bob", old.id, { text: "Race target fact, edited.", source: "s", expectedVersion: 1 }),
      a.hyperag.ingest("alice", { scope: team, source: "s", text: "Race correction.", supersedes: [old.id] }),
    ]);
    const why = await a.hyperag.explain("alice", old.id);
    const current = why.versions.at(-1);
    // Either order: the correction applies to whatever version is current when it commits.
    expect(current?.status).toBe("superseded");
  });

  test("a validity-only update writes a new version", async () => {
    const m = await a.hyperag.ingest("alice", { scope: team, source: "s", text: "Validity fact." });
    const u = await a.hyperag.update("alice", m.id, { text: "Validity fact.", source: "s", expectedVersion: 1, validTo: "2026-12-31T00:00:00Z" });
    expect(u).toMatchObject({ version: 2, created: true });
  });

  test("a failed graph delete is repaired by retrying, and reads ignore it meanwhile", async () => {
    const real = createNeo4jGraph(NEO4J);
    let fail = true;
    const flaky = { ...real, deleteMemory: async (id: string) => { if (fail) throw new Error("graph down"); return real.deleteMemory(id); } };
    const f = await open(db.url, { graph: flaky });
    const m = await f.hyperag.ingest("alice", { scope: team, source: "s", text: "Graph delete repair target." });
    const n = await f.hyperag.ingest("alice", { scope: team, source: "s", text: "Graph delete repair neighbour." });
    await f.hyperag.relate("alice", { from: n.id, to: m.id, type: "REFERS_TO" });
    await expect(f.hyperag.delete("alice", m.id, "x")).rejects.toThrow("graph down");
    expect(await f.hyperag.relations("alice", n.id)).toEqual([]);
    fail = false;
    await f.hyperag.delete("alice", m.id, "x");
    expect(await real.relations(n.id, [team])).toEqual([]);
    await f.close();
  });
});

describe("valid time", () => {
  test("asOf returns the fact valid at that instant", async () => {
    await a.hyperag.ingest("alice", {
      scope: team,
      source: "policy",
      text: "Deploy freeze window policy: freezes start at 18:00.",
      validFrom: "2026-01-01T00:00:00Z",
      validTo: "2026-06-01T00:00:00Z",
    });
    const before = await a.hyperag.retrieve("alice", "deploy freeze window policy", { asOf: "2026-03-01T00:00:00Z" });
    const after = await a.hyperag.retrieve("alice", "deploy freeze window policy", { asOf: "2026-09-01T00:00:00Z" });
    expect(before.items.some((i) => i.memory.source === "policy")).toBe(true);
    expect(after.items.some((i) => i.memory.source === "policy")).toBe(false);
  });
});
