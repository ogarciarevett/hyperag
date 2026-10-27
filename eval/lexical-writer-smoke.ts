// Model-free service ownership, publication, replay and shutdown on an isolated database.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Pool } from "pg";
import { startService, type Service } from "./hyperag-http";

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const bin = (process.env.HYPERAG_SERVICE_CMD ?? "target/release/hyperag serve").split(" ")[0]!;
const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}lexical_writer_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
const identifier = `"${name.replaceAll('"', '""')}"`;
const pgUrl = PG.replace(/\/[^/]+$/u, `/${name}`);
const home = realpathSync(mkdtempSync(join(tmpdir(), "lexical-writer-")));
const env = { HOME: home, HYPERAG_PG_URL: pgUrl, HYPERAG_LEXICAL_SCORER: "tantivy",
  HYPERAG_LEXICAL_INDEX_ROOT: "", HYPERAG_EMBEDDING: "none", HYPERAG_RERANKER: "none",
  HYPERAG_NEO4J_URL: "", HYPERAG_HOST: "127.0.0.1" };
const admin = new Pool({ connectionString: PG, max: 1 });
const probe = new Pool({ connectionString: pgUrl, max: 1 });
let svc: Service | undefined;
let created = false;
const modelsList = () => execFileSync(bin, ["models", "list"], {
  env: { ...process.env, ...env }, timeout: 15_000, stdio: "pipe",
});
const launch = () => startService({ pgUrl, principals: ["admin", "reader"], admins: ["admin"],
  embedding: "none", reranker: "none", env });
const stop = async () => {
  const active = svc;
  if (!active) return;
  svc = undefined;
  assert.deepEqual(await active.stop(), { code: 0, signal: null }, "service must drain and exit successfully");
};
const published = async (root: string) => {
  const deadline = Date.now() + 15_000;
  do {
    const { rows: [fence] } = await probe.query("select database_id::text,epoch::text,fence_token::text from lexical_index_state");
    try {
      const generation = JSON.parse(readFileSync(join(root, "CURRENT"), "utf8")) as string;
      assert.match(generation, /^gen-[a-f0-9]{32}$/u);
      const metadata = JSON.parse(readFileSync(join(root, generation, "meta.json"), "utf8"));
      const manifest = JSON.parse(metadata.payload);
      if (manifest.complete && manifest.checkpoint.database_id === fence.database_id
        && String(manifest.checkpoint.epoch) === fence.epoch
        && manifest.checkpoint.fence_token === fence.fence_token) return generation;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    await Bun.sleep(50);
  } while (Date.now() < deadline);
  throw new Error("writer did not publish the current source fence within 15 seconds");
};

try {
  await admin.query(`create database ${identifier}`);
  created = true;
  modelsList();
  const { rows: [{ database_id: databaseId }] } = await probe.query("select database_id::text from lexical_index_state");
  const root = join(home, `.hyperag-tantivy-${databaseId}`);
  assert.equal(existsSync(root), false, "shared CLI open must not create a writer root");
  svc = await launch();
  await published(root);
  assert.equal(statSync(root).mode & 0o777, 0o700);
  modelsList(); // The shared CLI must also succeed while Serve owns the index lock.
  for (const permission of ["read", "write"]) {
    await svc.must("admin", "POST", "/v1/grants", { principal: "reader", scope: "fixture", permission });
  }
  const memory = await svc.must("reader", "POST", "/v1/memories", {
    scope: "fixture", source: "synthetic", text: "quartz fixture initial",
  });
  await published(root);
  await svc.must("reader", "PATCH", `/v1/memories/${memory.id}`, {
    text: "quartz fixture updated", source: "synthetic", expectedVersion: 1,
  });
  const generation = await published(root);
  await stop();
  svc = await launch();
  assert.equal(await published(root), generation, "restart must recover the published generation");
  const retrieved = await svc.must("reader", "POST", "/v1/retrieve", {
    query: "quartz", channels: { vector: false, graph: false, rerank: false },
  });
  assert.deepEqual((retrieved.items as { memory: { id: string; version: number } }[])
    .map(({ memory: hit }) => [hit.id, hit.version]), [[memory.id, 2]]);
  await svc.must("reader", "DELETE", `/v1/memories/${memory.id}`);
  await published(root);
  await stop();
  console.log("lexical writer smoke: CLI isolation, default root, publish/replay, restart and bounded shutdown passed");
} finally {
  try { await stop(); } finally {
    await probe.end();
    let dropped = !created;
    try {
      if (created) {
        let count = -1;
        for (let i = 0; i < 20 && count !== 0; i++) {
          const result = await admin.query<{ count: number }>("select count(*)::int as count from pg_stat_activity where datname=$1", [name]);
          const row = result.rows[0];
          assert(row, "session count query returned no row");
          count = row.count;
          if (count !== 0) await Bun.sleep(50);
        }
        assert.equal(count, 0, "service shutdown must release every owned database connection");
        await admin.query(`drop database ${identifier}`);
        dropped = true;
      }
    } finally { await admin.end(); if (dropped) rmSync(home, { recursive: true, force: true }); }
  }
}
