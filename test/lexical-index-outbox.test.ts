import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Pool } from "pg";
import { createPostgresStore } from "../src/adapters/postgres";
import { freshDatabase } from "./helpers";

const db = await freshDatabase();
const pool = new Pool({ connectionString: db.url, max: 4 });
const store = createPostgresStore(pool);
pool.on("error", () => undefined);
const state = async () => (await pool.query("select database_id::text,epoch::text,retained_after::text,fence_token::text from lexical_index_state")).rows[0];
beforeAll(async () => {
  for (const name of ["0001_init.sql", "0002_settings.sql", "0003_memory_text_limit.sql"])
    await pool.query(readFileSync(new URL(`../crates/hyperag/migrations/${name}`, import.meta.url), "utf8"));
  await pool.query(`insert into memories(id,scope,kind,created_by,current_version) values('legacy','allowed','fact','fixture',1);
    insert into memory_versions(memory_id,version,text,source,author,content_hash)
    values('legacy',1,'synthetic-old-private-marker','fixture','fixture','old')`);
  await store.migrate();
});
afterAll(async () => { await pool.end(); await db.drop(); });

test("upgrading a populated TS store creates a stable identity/fence without changing source", async () => {
  expect((await pool.query("select to_regclass('lexical_index_state') as name")).rows[0].name).not.toBeNull();
  const first = await state();
  expect(first.database_id).toMatch(/^[0-9a-f-]{36}$/);
  expect(first.epoch).toBe("0");
  await store.migrate();
  expect(await state()).toEqual(first);
  expect((await pool.query("select text from memory_versions where memory_id='legacy'")).rows[0].text).toBe("synthetic-old-private-marker");
});

test("source changes and the ID-only outbox commit or roll back together", async () => {
  const before = await state();
  await pool.query(`begin; select pg_advisory_xact_lock(1213811052,1);
    update memory_versions set text='rolled-back-marker' where memory_id='legacy'; rollback`);
  expect(await state()).toEqual(before);
  expect((await pool.query("select count(*)::int n from lexical_index_changes")).rows[0].n).toBe(0);
  await pool.query("update memory_versions set text='synthetic-new-private-marker' where memory_id='legacy'");
  const after = await state();
  expect(BigInt(after.epoch)).toBe(BigInt(before.epoch) + 1n);
  const events = (await pool.query("select epoch::text,memory_id,kind from lexical_index_changes order by epoch")).rows;
  expect(events).toEqual([{ epoch: after.epoch, memory_id: "legacy", kind: "memory" }]);
  expect(Object.keys((await pool.query("select * from lexical_index_changes limit 1")).rows[0]).sort()).toEqual(["epoch", "fence_token", "kind", "memory_id", "previous_token"]);
  expect(JSON.stringify(events)).not.toContain("private-marker");
});

test("divergent writes at the same epoch cannot reuse a checkpoint token", async () => {
  const writer = await pool.connect();
  try {
    await writer.query("begin; select pg_advisory_xact_lock(1213811052,1); savepoint branch");
    const before = (await writer.query("select epoch::text,fence_token::text from lexical_index_state")).rows[0];
    await writer.query("update memory_versions set text='branch-a-marker' where memory_id='legacy'");
    const first = (await writer.query("select epoch::text,fence_token::text from lexical_index_state")).rows[0];
    await writer.query("rollback to branch");
    await writer.query("update memory_versions set text='branch-b-marker' where memory_id='legacy'");
    const second = (await writer.query("select epoch::text,fence_token::text from lexical_index_state")).rows[0];
    expect(first.epoch).toBe(second.epoch);
    expect(first.fence_token).not.toBe(second.fence_token);
    expect(second.fence_token).not.toBe(before.fence_token);
    const event = (await writer.query("select previous_token::text,fence_token::text from lexical_index_changes where epoch=$1", [second.epoch])).rows[0];
    expect(event).toEqual({ previous_token: before.fence_token, fence_token: second.fence_token });
  } finally { await writer.query("rollback"); writer.release(); }
});

test("a repeatable-read fence, events and final source state share one snapshot", async () => {
  const reader = await pool.connect();
  try {
    await reader.query("begin isolation level repeatable read read only");
    const fence = (await reader.query("select epoch::text from lexical_index_state")).rows[0].epoch;
    await pool.query("update memory_versions set text='newer-committed-marker' where memory_id='legacy'");
    expect((await reader.query("select epoch::text from lexical_index_state")).rows[0].epoch).toBe(fence);
    expect((await reader.query("select text from memory_versions where memory_id='legacy'")).rows[0].text).toBe("synthetic-new-private-marker");
    expect((await reader.query("select count(*)::int n from lexical_index_changes where epoch>$1", [fence])).rows[0].n).toBe(0);
    await reader.query("commit");
    expect(BigInt((await state()).epoch)).toBeGreaterThan(BigInt(fence));
  } finally { await reader.query("rollback"); reader.release(); }
});

test("a waiting writer publishes the next contiguous fence after an earlier writer rolls back", async () => {
  const before = await state(), first = await pool.connect(), second = await pool.connect();
  let pending: Promise<unknown> | undefined;
  try {
    await first.query("begin; select pg_advisory_xact_lock(1213811052,1); update memory_versions set text='uncommitted-marker' where memory_id='legacy'");
    const pid = (await second.query("select pg_backend_pid() as pid")).rows[0].pid;
    pending = second.query("begin; select pg_advisory_xact_lock(1213811052,1); update memory_versions set text='committed-marker' where memory_id='legacy'; commit");
    let waiting = false;
    for (let attempt = 0; attempt < 100; attempt++) {
      waiting = (await pool.query("select wait_event='advisory' as waiting from pg_stat_activity where pid=$1", [pid])).rows[0]?.waiting;
      if (waiting) break;
      await Bun.sleep(10);
    }
    expect(waiting).toBe(true);
    expect(await state()).toEqual(before);
    await first.query("rollback");
    await pending;
    const after = await state();
    expect(BigInt(after.epoch)).toBe(BigInt(before.epoch) + 1n);
    expect((await pool.query("select previous_token::text from lexical_index_changes where epoch=$1", [after.epoch])).rows[0].previous_token).toBe(before.fence_token);
  } finally { await first.query("rollback"); await pending?.catch(() => undefined); await second.query("rollback"); first.release(); second.release(); }
});

test("repeated source writes coalesce within one transaction without repeatedly rewriting the checkpoint", async () => {
  const before = await state(), writer = await pool.connect();
  try {
    await writer.query("begin; select pg_advisory_xact_lock(1213811052,1)");
    const writesBefore = Number((await writer.query("select n_tup_upd from pg_stat_xact_user_tables where relname='lexical_index_state'")).rows[0].n_tup_upd);
    await writer.query("do $$ begin for i in 1..64 loop update memory_versions set text='batch-marker-'||i where memory_id='legacy'; end loop; end $$");
    const fence = (await writer.query("select epoch::text from lexical_index_state")).rows[0].epoch;
    expect(BigInt(fence)).toBe(BigInt(before.epoch) + 1n);
    expect((await writer.query("select count(*)::int n from lexical_index_changes where epoch=$1", [fence])).rows[0].n).toBe(1);
    expect(Number((await writer.query("select n_tup_upd from pg_stat_xact_user_tables where relname='lexical_index_state'")).rows[0].n_tup_upd) - writesBefore).toBe(1);
    await expect(writer.query("select lexical_index_prune($1)", [fence])).rejects.toThrow();
  } finally { await writer.query("rollback"); writer.release(); }
  expect(await state()).toEqual(before);
});

test("grant changes advance the fence but no-op grants do not manufacture changes", async () => {
  const before = BigInt((await state()).epoch);
  await store.grant("fixture", "allowed", "read", "fixture");
  expect(BigInt((await state()).epoch)).toBe(before + 1n);
  await store.grant("fixture", "allowed", "read", "fixture");
  expect(BigInt((await state()).epoch)).toBe(before + 1n);
  await store.revoke("fixture", "allowed", "read", "fixture");
  expect(BigInt((await state()).epoch)).toBe(before + 2n);
  expect((await pool.query("select memory_id,kind from lexical_index_changes where kind='authorization'")).rows).toEqual([
    { memory_id: null, kind: "authorization" }, { memory_id: null, kind: "authorization" },
  ]);
});

test("pruning records a durable gap boundary without changing source epoch or identity", async () => {
  const before = await state(), through = (BigInt(before.epoch) - 1n).toString();
  await expect(pool.query("select lexical_index_prune($1)", [(BigInt(before.epoch) + 1n).toString()])).rejects.toThrow();
  await expect(pool.query("select lexical_index_prune(-1)")).rejects.toThrow();
  await expect(pool.query("select lexical_index_prune(null)")).rejects.toThrow();
  await pool.query("select lexical_index_prune($1)", [through]);
  expect(await state()).toEqual({ ...before, retained_after: through });
  expect((await pool.query("select count(*)::int n from lexical_index_changes where epoch<=$1", [through])).rows[0].n).toBe(0);
  await pool.query("select lexical_index_prune(0)");
  expect((await state()).retained_after).toBe(through);
});

test("truncation creates an explicit rebuild marker", async () => {
  const before = BigInt((await state()).epoch);
  await pool.query("begin; select pg_advisory_xact_lock(1213811052,1); truncate memory_versions cascade; commit");
  expect(BigInt((await state()).epoch)).toBeGreaterThan(before);
  expect((await pool.query("select kind,memory_id from lexical_index_changes order by epoch desc limit 1")).rows[0]).toEqual({ kind: "reset", memory_id: null });
});
