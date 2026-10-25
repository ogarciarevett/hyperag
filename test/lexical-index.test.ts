import { afterAll, beforeAll, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Pool } from "pg";
import { freshDatabase } from "./helpers";

// Execute the shipped Rust SQL, with its real parameter types and default planner settings.
const source = readFileSync(new URL("../crates/hyperag/src/store.rs", import.meta.url), "utf8");
const rankable = source.match(/const RANKABLE: &str = "([\s\S]*?)";/)![1]!;
const template = source.match(/fn bm25_sql\(\) -> String \{\s*format!\(\s*"([\s\S]*?)"\s*\)/)![1]!;
const sql = template.replaceAll("{RANKABLE}", rankable).replaceAll("{BM25_K1}", "1.2").replaceAll("{BM25_B}", "0.75");
const db = await freshDatabase();
const pool = new Pool({ connectionString: db.url, max: 1 });
const params = [["allowed"], null, "quartz nebula", 3];

beforeAll(async () => {
  await pool.query(readFileSync(new URL("../crates/hyperag/migrations/0001_init.sql", import.meta.url), "utf8"));
  await pool.query(`insert into memories(id,scope,kind,created_by,current_version)
    select 'filler-'||n,'allowed','fact','fixture',1 from generate_series(1,20000) n;
    insert into memory_versions(memory_id,version,text,source,author,content_hash)
    select id,1,'ordinary maintenance schedule and neutral background words','fixture','fixture',id from memories`);
  const rows = [
    ["a-best", "allowed", 1, "quartz nebula quartz nebula quartz nebula", "active"],
    ["b-next", "allowed", 1, "quartz nebula neutral neutral neutral neutral", "active"],
    ["c-last", "allowed", 1, "quartz neutral neutral neutral neutral neutral", "active"],
    ["stale", "allowed", 2, "neutral current text", "active"],
    ["invalid", "allowed", 1, "quartz nebula ".repeat(80), "invalidated"],
    ["deleted", "allowed", 1, "quartz nebula ".repeat(80), "active"],
    ["future", "allowed", 1, "quartz nebula ".repeat(80), "active"],
  ];
  for (const [id, scope, current, text, status] of rows) {
    await pool.query("insert into memories(id,scope,kind,created_by,current_version) values($1,$2,'fact','fixture',$3)", [id,scope,current]);
    await pool.query("insert into memory_versions(memory_id,version,text,source,author,content_hash,status) values($1,$2,$3,'fixture','fixture',$1,$4)", [id,current,text,status]);
  }
  // Deliberately active old version: the current-version check must still exclude it.
  await pool.query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values('stale',1,$1,'fixture','fixture','old')", ["quartz nebula ".repeat(80)]);
  await pool.query("update memories set deleted_at=now() where id='deleted'; update memory_versions set valid_from='2100-01-01' where memory_id='future'");
  // Higher scoring denied rows must not consume the candidate budget before authorization.
  await pool.query(`insert into memories(id,scope,kind,created_by,current_version)
    select 'hidden-'||n,'denied','fact','fixture',1 from generate_series(1,50) n;
    insert into memory_versions(memory_id,version,text,source,author,content_hash)
    select id,1,repeat('quartz nebula ',80),'fixture','fixture',id from memories where scope='denied';
    analyze memories; analyze memory_versions`);
  // Normal post-bulk maintenance merges the pending GIN list; planner settings stay default.
  await pool.query("vacuum (analyze) memory_versions");
}, 60_000);
afterAll(async () => { await pool.end(); await db.drop(); });

const at = "2026-01-01T00:00:00Z";
test("Rust lexical SQL uses the GIN Bitmap Index Scan on a selective synthetic corpus", async () => {
  const r = await pool.query(`explain (analyze, buffers, format json) ${sql}`, [params[0],at,params[2],params[3]]);
  const plan = r.rows[0]["QUERY PLAN"][0].Plan;
  const nodes: any[] = [];
  const walk = (p: any) => { nodes.push(p); for (const child of p.Plans ?? []) walk(child); };
  walk(plan);
  const indexed = nodes.some(n => n["Node Type"] === "Bitmap Index Scan" && n["Index Name"] === "memory_versions_text_search_idx");
  if (!indexed) throw new Error("Missing GIN bitmap path: " + JSON.stringify(nodes.map(n => ({ node: n["Node Type"], index: n["Index Name"] }))));
  expect(indexed).toBe(true);
  console.info("lexical-fixture-plan", JSON.stringify({ ginBitmapIndex: "memory_versions_text_search_idx", rows: plan["Actual Rows"], executionMs: r.rows[0]["QUERY PLAN"][0]["Execution Time"] }));
  expect(nodes.some(n => n["Node Type"] === "Function Scan" && n["Function Name"] === "unnest")).toBe(false);
});

test("fixture ranking is preserved and denied, deleted, stale, invalid or future rows cannot displace candidates", async () => {
  const r = await pool.query(sql, [params[0],at,params[2],params[3]]);
  expect(r.rows.map(r => r.id)).toEqual(["a-best", "b-next", "c-last"]);
  expect(r.rows.every(r => Number.isFinite(Number(r.score)) && Number(r.score) > 0)).toBe(true);
  expect((await pool.query(sql, [[],at,params[2],3])).rows).toEqual([]);
  expect((await pool.query(sql, [params[0],at,"no-synthetic-match",3])).rows).toEqual([]);
});
