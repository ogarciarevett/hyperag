import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Pool } from "pg";
import { createPostgresStore } from "../src/adapters/postgres";
import { freshDatabase } from "./helpers";

const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const db = await freshDatabase();
const observer = new Pool({ connectionString: db.url, max: 1 });
const migration = new Pool({ connectionString: db.url, max: 1 });
const writer = new Pool({ connectionString: db.url, max: 1 });
let teardown = false;
for (const pool of [observer, migration, writer]) pool.on("error", (e: Error & {code?: string}) => {
  if (!(teardown && e.code === "57P01")) throw e;
});
beforeAll(async () => {
  await createPostgresStore(observer).migrate();
  await observer.query(read("crates/hyperag/migrations/0004_lexical_cache.sql"));
  await observer.query("insert into memories(id,scope,kind,created_by,current_version) values('fixture','fixture','fact','fixture',1); insert into memory_versions(memory_id,version,text,source,author,content_hash) values('fixture',1,'quartz fixture','fixture','fixture','fixture')");
});
afterAll(async () => { teardown = true; await Promise.all([observer.end(), migration.end(), writer.end()]); await db.drop(); });

test("real TypeScript invalidate versus guarded migration has no table/advisory inversion", async () => {
  const pid = (await writer.query("select pg_backend_pid() as pid")).rows[0].pid;
  // Pin the migration backend as the shared-lock owner before starting the other real Store call.
  // A session lock survives migrate's own BEGIN/COMMIT; production uses the same key transactionally.
  await migration.query("select pg_advisory_lock(1213811052,1)");
  const invalidated = createPostgresStore(writer).invalidate("fixture", "fixture", "fixture").then(() => null, (e: Error & {code?: string}) => e.code ?? e.message);
  let migrationError: string | null = null;
  try {
    let waiting = false;
    for (let n=0;n<500;n++) {
      waiting = (await observer.query("select cardinality(pg_blocking_pids($1))>0 as waiting", [pid])).rows[0].waiting;
      if (waiting) break;
      await Bun.sleep(10);
    }
    expect(waiting).toBe(true);
    await createPostgresStore(migration).migrate();
  } catch (e) { migrationError = (e as Error & {code?: string}).code ?? String(e); }
  finally { await migration.query("select pg_advisory_unlock(1213811052,1)"); }
  const invalidateError = await invalidated;
  expect({ migrationError, invalidateError }).toEqual({ migrationError: null, invalidateError: null });
}, 15000);

test("grep audit: direct pool access is confined to explicitly read-only Store methods", () => {
  const files = [
    { path: "src/adapters/postgres.ts", header: /^    async (\w+)\(/gm, reads: ["scopes","scopeOf","bm25","vector","load","history","missingVectors","stats"] },
    { path: "crates/hyperag/src/store.rs", header: /^    (?:pub(?:\(crate\))? )?async fn (\w+)\(/gm, reads: ["scopes","scope_of","bm25","vector","load","recent","history","missing_vectors","existing","spaces","setting","begin_write"] },
  ];
  const violations: string[] = [];
  for (const {path, header, reads} of files) {
    const source = read(path), methods = [...source.matchAll(header)];
    for (const [i, match] of methods.entries()) {
      const body = source.slice(match.index, methods[i+1]?.index ?? source.length);
      if (!reads.includes(match[1]!) && /pool\s*\.\s*(?:query|connect|begin)\s*\(|(?:execute|fetch_\w+)\s*\(\s*&self\.pool\s*\)/u.test(body)) violations.push(`${path}:${match[1]}`);
    }
  }
  expect(violations).toEqual([]);
});
