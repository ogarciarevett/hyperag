import { afterAll, beforeAll, expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { Pool, type PoolClient } from "pg";
import { freshDatabase } from "./helpers";

const db = await freshDatabase();
const pool = new Pool({ connectionString: db.url, max: 4 });
async function add(q: Pool | PoolClient, id: string, scope: string, text: string, version = 1) {
  await q.query("insert into memories(id,scope,kind,created_by,current_version) values($1,$2,'fact','fixture',$3)", [id,scope,version]);
  await q.query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values($1,$2,$3,'fixture','fixture',$1)", [id,version,text]);
}
beforeAll(async () => {
  for (const name of ["0001_init.sql","0002_settings.sql","0003_memory_text_limit.sql"])
    await pool.query(readFileSync(new URL(`../crates/hyperag/migrations/${name}`, import.meta.url), "utf8"));
  for (const [id,scope,text] of [["a","allowed","quartz quartz nebula"],["b","allowed","quartz cloud"],["punct","allowed","!!!"],["hidden","hidden","quartz quartz"],["future","allowed","quartz"],["invalid","allowed","old fact"],["deleted","allowed","old fact"]]) await add(pool,id!,scope!,text!);
  await add(pool,"stale","allowed","current comet",2);
  await pool.query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values('stale',1,'quartz quartz quartz','fixture','fixture','old'); update memory_versions set valid_from='2030-01-01',valid_to='2040-01-01' where memory_id='future'; update memory_versions set status='invalidated' where memory_id='invalid'; update memories set deleted_at=now() where id='deleted'");
  const migration = new URL("../crates/hyperag/migrations/0004_lexical_cache.sql", import.meta.url);
  if (existsSync(migration)) await pool.query(readFileSync(migration, "utf8"));
});
afterAll(async () => { await pool.end(); await db.drop(); });

// Oracle uses legacy tsvector semantics: dl counts unique lexemes; tf counts stored positions.
const source = `select m.id as memory_id,v.version,m.scope,
  coalesce(v.valid_from,'-infinity'::timestamptz) as valid_from,
  coalesce(v.valid_to,'infinity'::timestamptz) as valid_to,length(v.text_search) as doc_len,
  coalesce((select jsonb_object_agg(u.lexeme,coalesce(array_length(u.positions,1),1)) from unnest(v.text_search) u),'{}'::jsonb) as tf
  from memories m join memory_versions v on v.memory_id=m.id and v.version=m.current_version
  where m.deleted_at is null and v.status='active'`;
async function coherent() {
  const columns = "memory_id,version,scope,valid_from::text,valid_to::text,doc_len,tf";
  expect((await pool.query(`select ${columns} from lexical_documents order by memory_id`)).rows)
    .toEqual((await pool.query(`with s as (${source}) select ${columns} from s order by memory_id`)).rows);
  expect((await pool.query("select scope,valid_from::text,valid_to::text,n::int,total_len::int from lexical_corpus_stats order by scope,valid_from,valid_to")).rows)
    .toEqual((await pool.query(`with s as (${source}) select scope,valid_from::text,valid_to::text,count(*)::int as n,sum(doc_len)::int as total_len from s group by scope,valid_from,valid_to order by scope,valid_from,valid_to`)).rows);
  expect((await pool.query("select scope,valid_from::text,valid_to::text,lexeme,df::int from lexical_term_stats order by scope,valid_from,valid_to,lexeme")).rows)
    .toEqual((await pool.query(`with s as (${source}) select scope,valid_from::text,valid_to::text,keys.lexeme,count(*)::int as df from s cross join lateral jsonb_object_keys(tf) as keys(lexeme) group by scope,valid_from,valid_to,keys.lexeme order by scope,valid_from,valid_to,keys.lexeme`)).rows);
}

test("gated-size backfill is idempotent and readiness waits for complete current-row coverage", async () => {
  expect((await pool.query("select value from settings where key='lexical_cache_ready'")).rows[0]?.value).toBe("false");
  let total = 0;
  for (let tries=0;tries<10;tries++) { const n=Number((await pool.query("select lexical_cache_backfill(2) as n")).rows[0].n); total+=n; if (!n) break; }
  expect(total).toBe(6);
  expect((await pool.query("select value from settings where key='lexical_cache_ready'")).rows[0].value).toBe("true");
  await coherent();
  await pool.query("select lexical_refresh_document('a'); select lexical_refresh_document('a')");
  expect(Number((await pool.query("select lexical_cache_backfill(2) as n")).rows[0].n)).toBe(0);
  await coherent();
});

test("writes, text/TF changes, scopes, validity, current versions and deletion maintain exact statistics", async () => {
  await add(pool,"mutable","allowed","quartz nebula"); await coherent();
  await pool.query("update memory_versions set text='quartz quartz cosmic' where memory_id='mutable'"); await coherent();
  await pool.query("update memory_versions set valid_from='2030-01-01',valid_to='2031-01-01' where memory_id='mutable'; update memories set scope='moved' where id='mutable'"); await coherent();
  await pool.query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values('mutable',2,'fresh comet','fixture','fixture','v2')"); await coherent();
  await pool.query("update memories set current_version=2 where id='mutable'"); await coherent();
  await pool.query("update memory_versions set status='invalidated' where memory_id='mutable' and version=2"); await coherent();
  await pool.query("update memory_versions set status='active' where memory_id='mutable' and version=2"); await coherent();
  await pool.query("update memories set deleted_at=now() where id='mutable'; delete from memory_versions where memory_id='mutable'"); await coherent();
  await pool.query("delete from memories where id='hidden'"); await coherent();
});

test("concurrent writers do not lose frequency or length deltas", async () => {
  await Promise.all(Array.from({length:8}, async (_,n) => {
    const client=await pool.connect();
    try { await client.query("begin"); await add(client,`race-${n}`,"allowed","concurrent quartz"); await client.query("commit"); }
    catch(e) { await client.query("rollback"); throw e; }
    finally { client.release(); }
  }));
  await coherent();
});
