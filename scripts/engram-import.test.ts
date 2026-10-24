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
    const memories = new Map<string, Array<Record<string, any>>>();
    const replays = new Map<string, Record<string, unknown>>();
    const edges: unknown[] = [];
    const call = async (method: string, path: string, raw?: unknown): Promise<Record<string, unknown>> => {
      const b = raw as Record<string, any>;
      if (path === "/v1/whoami") return { principal: "personal-import" };
      if (path === "/v1/grants") return {};
      if (path === "/v1/relations") { edges.push(b); return {}; }
      if (method === "GET") {
        const id = decodeURIComponent(path.slice("/v1/memories/".length, -"/explain".length));
        expect(memories.has(id)).toBe(true);
        return { versions: memories.get(id)! };
      }
      if (replays.has(b.idempotencyKey)) return replays.get(b.idempotencyKey)!;
      const id = method === "POST" ? `${b.scope}/${b.id}` : decodeURIComponent(path.slice("/v1/memories/".length));
      const history = memories.get(id) ?? [];
      if (method === "PATCH") {
        // Rust returns the current version without recording a key on an identical payload.
        const fields = (v: Record<string, any>) => JSON.stringify([v.text, v.source, v.validFrom, v.metadata]);
        if (fields(history.at(-1)!) === fields(b)) return { id, version: history.length };
        expect(b.expectedVersion).toBe(history.length);
      }
      history.push({ ...b, kind: history[0]?.kind ?? b.kind });
      memories.set(id, history);
      const r = { id, version: history.length };
      replays.set(b.idempotencyKey, r);
      return r;
    };
    const expected = { memories: 2, updates: 2, relations: 1, skippedRelations: 1 };
    expect(await importSource(snapshot, call)).toEqual(expected);
    expect(await importSource(snapshot, call)).toEqual(expected);
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
