import { expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { Pool } from "pg";
import { benchmarkDatabaseName } from "./systems/postgres-db";

test("parallel adapter instances initialize one fresh schema", async () => {
  const group = `initrace_${randomUUID().replaceAll("-", "")}`;
  const prefix = process.env.HYPERAG_DB_PREFIX ?? "dev_";
  const db = benchmarkDatabaseName(group, "bm25", prefix);
  const admin = new Pool({ connectionString: process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag", max: 1 });
  // A child owns every test connection, so even a failed initialization can be cleaned up safely.
  const script = `
    import factory from ${JSON.stringify(new URL("./systems/hyperag.ts", import.meta.url).href)};
    const systems = await Promise.all(Array.from({ length: 4 }, () => factory("bm25")));
    const results = await Promise.allSettled(systems.map((s, i) => s.open(${JSON.stringify(group)} + "/conv-" + i)));
    await Promise.allSettled(systems.map(s => s.close()));
    for (const result of results) if (result.status === "rejected") console.error(result.reason?.message);
    process.exit(results.some(r => r.status === "rejected") ? 1 : 0);
  `;
  try {
    const child = Bun.spawn([process.execPath, "-e", script], { stdout: "ignore", stderr: "pipe", env: { ...process.env, HYPERAG_DB_PREFIX: prefix } });
    const [code, error] = await Promise.all([child.exited, new Response(child.stderr).text()]);
    if (code) console.error(error);
    expect(code).toBe(0);
  } finally {
    for (let i = 0; i < 40; i++) {
      const active = await admin.query("select 1 from pg_stat_activity where datname = $1", [db]);
      if (!active.rowCount) break;
      await Bun.sleep(25);
    }
    await admin.query(`drop database if exists "${db}"`);
    await admin.end();
  }
}, 30000);
