import { test, expect } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { archiveSource, decodeArchiveChunk } from "./engram-archive";

test("archives every logical row and schema losslessly, verifies, and replays without writes", async () => {
  const dir = mkdtempSync(join(tmpdir(), "engram-archive-test-"));
  const path = join(dir, "source.db"), db = new Database(path);
  db.exec(`create table sessions(id text primary key, summary text, ended_at text);
    create table user_prompts(id integer primary key, content text, session_id text);
    create table observations(id integer primary key, content text, deleted_at text, embedding blob);
    create table memory_relations(id integer primary key, relation text, reason text);
    create table sync_state(target text primary key, n integer) without rowid;
    create table empty_table(id integer);
    create virtual table prompts_fts using fts5(content);
    insert into sessions values ('s1','Synthetic session','2026-01-01');
    insert into user_prompts values (1,'Synthetic prompt','s1');
    insert into observations values (1,'Synthetic deleted','2026-01-02',x'00ff');
    insert into memory_relations values (1,'pending','Synthetic reason');
    insert into sync_state values ('local',7);`);
  db.query("insert into user_prompts values (2,?,'s1')").run("🙂".repeat(100_001));
  db.query("insert into user_prompts values (3,?,'s1')").run(" ".repeat(160_001));
  db.close();
  const before = readFileSync(path), memories = new Map<string, any>(), writes: any[] = [];
  const call = async (method: string, route: string, body?: any): Promise<any> => {
    if (route === "/v1/whoami") return { principal: "personal-import" };
    if (route === "/v1/grants") return {};
    if (method === "GET") return memories.get(decodeURIComponent(route.slice(13, -8))) ?? { missing: true };
    expect(method).toBe("POST");
    expect(route).toBe("/v1/memories");
    expect(body.text.trim().length).toBeGreaterThan(0);
    expect([...body.text].length).toBeLessThanOrEqual(80_000);
    expect(JSON.stringify(body).length).toBeLessThan(1_048_576);
    const id = `${body.scope}/${body.id}`;
    expect(memories.has(id)).toBe(false);
    memories.set(id, { id, currentVersion: 1, versions: [{ ...body, version: 1 }] });
    writes.push(body);
    return { id, version: 1 };
  };
  try {
    const result = await archiveSource(path, call);
    expect(result.tables).toEqual({ empty_table: 0, memory_relations: 1, observations: 1, sessions: 1, sync_state: 1, user_prompts: 3 });
    expect(result.rows).toBe(7);
    expect(result.excludedDerivedTables).toContain("prompts_fts");
    const rows = writes.filter(b => b.metadata.engramArchive.record === "row");
    const grouped = new Map<string, any[]>();
    for (const b of rows) {
      const m = b.metadata.engramArchive, key = JSON.stringify([m.table, m.identity]);
      grouped.set(key, [...(grouped.get(key) ?? []), b]);
    }
    const decoded = [...grouped.values()].map(parts => JSON.parse(parts.sort((a,b) => a.metadata.engramArchive.part-b.metadata.engramArchive.part).map(p => decodeArchiveChunk(p.text)).join("")));
    expect(decoded).toContainEqual({ id: 1, content: "Synthetic prompt", session_id: "s1" });
    expect(decoded).toContainEqual({ id: 1, content: "Synthetic deleted", deleted_at: "2026-01-02", embedding: { $blob: "AP8=" } });
    expect(decoded).toContainEqual({ id: 3, content: " ".repeat(160_001), session_id: "s1" });
    expect(decoded).toContainEqual({ id: 2, content: "🙂".repeat(100_001), session_id: "s1" });
    const n = writes.length;
    expect((await archiveSource(path, call)).created).toBe(0);
    expect(writes.length).toBe(n);
    expect(readFileSync(path)).toEqual(before);
    const edit = new Database(path);edit.exec("update sessions set summary='Changed synthetic session'");edit.close();
    expect((await archiveSource(path, call)).created).toBe(3);
    expect(writes.length).toBe(n + 3);
  } finally { rmSync(dir, { recursive: true }); }
});

test("rejects a foreign principal before any archive write", async () => {
  await expect(archiveSource("unused", async () => ({ principal: "other" }))).rejects.toThrow("dedicated import principal required");
});

 test("CLI errors never print private paths or token arguments", () => {
  for (const args of [["--db", "/missing/Private-title.db", "--apply"], ["--token", "secret-argument"]]) {
    const result = Bun.spawnSync([process.execPath, "scripts/engram-archive.ts", ...args], { env: { ...process.env, HYPERAG_TOKEN: "synthetic-test-token" } });
    expect(result.exitCode).toBe(1);
    expect(result.stdout.length).toBe(0);
    expect(result.stderr.toString()).toBe("Engram archive failed; source unchanged. Check configuration and counts before retrying.\n");
  }
});

test("chunk decoder rejects unknown formats", () => {
  expect(() => decodeArchiveChunk("unsupported")).toThrow("unsupported archive chunk format");
});
