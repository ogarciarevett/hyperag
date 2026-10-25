import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { importSource, readSource } from "./engram-import";

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "hyperag-import-synthetic-"));
  const path = join(dir, "source.db");
  const db = new Database(path);
  db.exec(`create table observations (id integer, sync_id text, session_id text, type text, title text, content text, tool_name text, project text, scope text, topic_key text, pinned integer, created_at text, updated_at text, deleted_at text);
    create table observation_versions (observation_id integer, title text, content text, version integer, created_at text);
    create table memory_relations (source_id text, target_id text, relation text, superseded_at text);
    insert into observations values (1,'synthetic-a','s1','decision','Private synthetic title','Private synthetic content',null,'Demo','project','topic',1,'2026-01-01 00:00:00','2026-01-03 00:00:00',null),
    (2,'synthetic-b','s2','note','Other title','Other text',null,null,'personal',null,0,'2026-01-01 00:00:00','2026-01-01 00:00:00',null),
    (3,'deleted','s3','note','Deleted title','Deleted text',null,'Demo','project',null,0,'2026-01-01 00:00:00','2026-01-01 00:00:00','2026-01-02');
    insert into observation_versions values (1,'Old title','Old content',1,'2026-01-01 00:00:00'),(1,'Private synthetic title','Private synthetic content',2,'2026-01-03 00:00:00'),(3,'Deleted prior','Deleted prior',1,'2026-01-01 00:00:00');
    insert into memory_relations values ('synthetic-a','synthetic-b','related',null),('synthetic-a','deleted','related',null),('synthetic-b','synthetic-a','not_conflict',null);`);
  db.close();
  return { dir, path };
}
const hash = (path: string) => createHash("sha256").update(readFileSync(path)).digest("hex");

function target() {
  const memories = new Map<string, Array<Record<string, any>>>();
  const writes: Array<{ method: string; path: string; body: any }> = [];
  const edges: unknown[] = [];
  const replays = new Map<string, Record<string, unknown>>();
  const call = async (method: string, path: string, raw?: unknown): Promise<Record<string, unknown>> => {
    const b = raw as any;
    if (path === "/v1/whoami") return { principal: "personal-import" };
    if (path === "/v1/grants") return {};
    if (path === "/v1/relations") { edges.push(b); return {}; }
    const id = method === "POST" ? `${b.scope}/${b.id}` : decodeURIComponent(path.slice("/v1/memories/".length).replace(/\/explain$/u, ""));
    const history = memories.get(id);
    if (method === "GET") return history ? { id, currentVersion: history.length, deleted: null, versions: structuredClone(history) } : { missing: true };
    if (method !== "POST" && method !== "PATCH") throw new Error("unexpected target mutation");
    if (replays.has(b.idempotencyKey)) return replays.get(b.idempotencyKey)!;
    if (method === "POST" && history) throw new Error("already exists");
    if (method === "PATCH" && (!history || b.expectedVersion !== history.length)) throw new Error("version conflict");
    const versions = history ?? [];
    versions.push({ ...structuredClone(b), version: versions.length + 1, kind: versions[0]?.kind ?? b.kind });
    memories.set(id, versions);
    writes.push({ method, path, body: structuredClone(b) });
    const result = { id, version: versions.length, created: true };
    replays.set(b.idempotencyKey, result);
    return result;
  };
  return { call, memories, writes, edges };
}

test("newer snapshots add new memories and leave unchanged memories alone", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const before = structuredClone([...t.memories]);
    const db = new Database(f.path);
    db.exec("insert into observations select 4,'synthetic-new',session_id,type,title,content,tool_name,project,scope,topic_key,pinned,created_at,updated_at,null from observations where id=2");
    db.close();
    t.writes.length = 0;
    const result = await importSource(readSource(f.path, true), t.call);
    expect(result).toMatchObject({ added: 1, changed: 0, unchanged: 2 });
    expect(t.writes.map(w => w.method)).toEqual(["POST"]);
    expect([...t.memories].slice(0, 2)).toEqual(before);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("changed observations append through the normal expected-version update", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const id = "project:demo/engram-synthetic-a", before = structuredClone(t.memories.get(id)!);
    const db = new Database(f.path);
    db.exec("update observations set content='Changed synthetic content', updated_at='2026-01-04 00:00:00' where id=1");
    db.close();
    t.writes.length = 0;
    const snapshot = readSource(f.path, true);
    expect(await importSource(snapshot, t.call)).toMatchObject({ added: 0, changed: 1, unchanged: 1 });
    expect(t.writes.map(w => [w.method, w.body.expectedVersion])).toEqual([["PATCH", 3]]);
    expect(t.memories.get(id)!.slice(0, 3)).toEqual(before);
    expect(t.memories.get(id)!.at(-1)!.text).toBe("Private synthetic title\nChanged synthetic content");
    t.writes.length = 0;
    expect(await importSource(snapshot, t.call)).toMatchObject({ added: 0, changed: 0, unchanged: 2, updates: 0 });
    expect(t.writes).toEqual([]);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("history-only changes are preserved and replay leaves target versions alone", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const id = "project:demo/engram-synthetic-a", before = structuredClone(t.memories.get(id)!);
    const db = new Database(f.path);
    db.exec("update observation_versions set content='Corrected synthetic history' where observation_id=1 and version=1");
    db.close();
    const snapshot = readSource(f.path, true);
    expect(await importSource(snapshot, t.call)).toMatchObject({ changed: 1, unchanged: 1 });
    expect(t.memories.get(id)!.slice(0, 3)).toEqual(before);
    expect(t.memories.get(id)!.some(v => v.text === "Old title\nCorrected synthetic history")).toBe(true);
    expect(t.memories.get(id)!.at(-1)!.text).toBe(before.at(-1)!.text);
    t.writes.length = 0;
    expect(await importSource(snapshot, t.call)).toMatchObject({ unchanged: 2, updates: 0 });
    expect(t.writes).toEqual([]);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("source-deleted observations are reported without deleting target memories", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const before = structuredClone([...t.memories]);
    const db = new Database(f.path);
    db.exec("update observations set deleted_at='2026-01-05' where id=1");
    db.close();
    t.writes.length = 0;
    expect(await importSource(readSource(f.path, true), t.call)).toMatchObject({ memories: 1, unchanged: 1, deletedInSource: 2 });
    expect([...t.memories]).toEqual(before);
    expect(t.writes).toEqual([]);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("an interrupted initial import resumes without duplicating stored versions", async () => {
  const f = fixture(), t = target();
  try {
    const snapshot = readSource(f.path, true);
    await expect(importSource(snapshot, async (method, path, body) => {
      if (method === "PATCH") throw new Error("synthetic interruption");
      return t.call(method, path, body);
    })).rejects.toThrow("synthetic interruption");
    expect(t.memories.get("project:demo/engram-synthetic-a")!.length).toBe(1);
    expect(await importSource(snapshot, t.call)).toMatchObject({ memories: 2, added: 1, changed: 1, updates: 2 });
    expect(t.memories.get("project:demo/engram-synthetic-a")!.length).toBe(3);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("an interrupted history catch-up resumes without duplicating appended source history", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const db = new Database(f.path);
    db.exec("update observation_versions set content='Corrected synthetic history' where observation_id=1 and version=1");
    db.close();
    const snapshot = readSource(f.path, true);
    await expect(importSource(snapshot, async (method, path, body) => {
      if (method === "PATCH" && (body as any).idempotencyKey.endsWith(":current")) throw new Error("synthetic interruption");
      return t.call(method, path, body);
    })).rejects.toThrow("synthetic interruption");
    expect(t.memories.get("project:demo/engram-synthetic-a")!.length).toBe(4);
    expect(await importSource(snapshot, t.call)).toMatchObject({ changed: 1, unchanged: 1, updates: 1 });
    expect(t.memories.get("project:demo/engram-synthetic-a")!.length).toBe(5);
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("a concurrent target update fails the expected-version check without overwriting it", async () => {
  const f = fixture(), t = target();
  try {
    await importSource(readSource(f.path, true), t.call);
    const db = new Database(f.path);
    db.exec("update observations set content='Changed synthetic content' where id=1");
    db.close();
    await expect(importSource(readSource(f.path, true), async (method, path, body) => {
      if (method === "PATCH") {
        const versions = t.memories.get("project:demo/engram-synthetic-a")!;
        versions.push({ ...versions.at(-1), version: 4, text: "Concurrent synthetic update" });
      }
      return t.call(method, path, body);
    })).rejects.toThrow("version conflict");
    expect(t.memories.get("project:demo/engram-synthetic-a")!.at(-1)!.text).toBe("Concurrent synthetic update");
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("default dry run prints only counts/ids, cannot contact a service, and leaves source and directory unchanged", () => {
  const f = fixture();
  try {
    const before = hash(f.path), files = readdirSync(f.dir);
    const r = Bun.spawnSync([process.execPath, "scripts/engram-import.ts", "--db", f.path], { env: { ...process.env, HYPERAG_TOKEN: "" } });
    expect(r.exitCode).toBe(0);
    expect(JSON.parse(r.stdout.toString())).toEqual({ counts: { memories: 2, scopes: 2, priorVersions: 2, judgedRelations: 2 }, ids: [1, 2] });
    expect(r.stderr.length).toBe(0);
    expect(hash(f.path)).toBe(before);
    expect(readdirSync(f.dir)).toEqual(files);
    expect(readSource(f.path)).not.toHaveProperty("observations");
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("apply preserves history metadata, scopes ids, links, verifies, and retries with stable keys", async () => {
  const f = fixture();
  try {
    const snapshot = readSource(f.path, true);
    const { call, memories, edges } = target();
    const expected = { memories: 2, added: 2, changed: 0, unchanged: 0, updates: 2, relations: 1, skippedRelations: 1, deletedInSource: 1 };
    expect(await importSource(snapshot, call)).toEqual(expected);
    expect(await importSource(snapshot, call)).toEqual({ ...expected, added: 0, unchanged: 2, updates: 0 });
    expect(memories.get("project:demo/engram-synthetic-a")?.length).toBe(3);
    expect(memories.get("project:demo/engram-synthetic-a")?.map(v => v.validFrom)).toEqual(["2026-01-01T00:00:00.000Z", "2026-01-03T00:00:00.000Z", "2026-01-03T00:00:00.000Z"]);
    for (const v of memories.get("project:demo/engram-synthetic-a")!) expect(v.metadata.engram.topic_key).toBe("topic");
    expect(memories.get("project:demo/engram-synthetic-a")?.map(v => v.metadata.engram.version_ordinal)).toEqual([1, 2, 3]);
    expect(memories.get("project:demo/engram-synthetic-a")?.map(v => v.metadata.engram.source_version)).toEqual([1, 2, null]);
    expect(edges[0]).toEqual({ from: "project:demo/engram-synthetic-a", to: "personal/engram-synthetic-b", type: "RELATED" });
  } finally { rmSync(f.dir, { recursive: true }); }
});

test("bad source and non-memory target errors do not expose arguments or private values", () => {
  for (const args of [["--db", "/missing/Private-title.db"], ["--db", "unused", "--apply", "--url", "http://127.0.0.1:7480"], ["--token", "secret-argument"]]) {
    const r = Bun.spawnSync([process.execPath, "scripts/engram-import.ts", ...args]);
    expect(r.exitCode).toBe(1);
    expect(r.stdout.length).toBe(0);
    expect(r.stderr.toString()).toBe("Engram import failed; source unchanged. Check configuration and counts before retrying.\n");
  }
});
