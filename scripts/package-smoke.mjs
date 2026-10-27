// Run after `npm pack`; no database, registry upload or model load is needed.
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const root = resolve(process.argv[2] ?? ".");
const archive = resolve(process.argv[3]);
const listing = spawnSync("tar", ["-tzf", archive], { encoding: "utf8" });
assert.equal(listing.status, 0);
for (const file of ["dist/index.js", "dist/index.d.ts", "dist/plugin.js", "dist/plugin.d.ts", "docs/plugin.md", "examples/plugin-host.ts", "crates/hyperag/migrations/0001_init.sql", "crates/hyperag/migrations/0002_settings.sql", "crates/hyperag/migrations/0003_memory_text_limit.sql", "crates/hyperag/migrations/0006_lexical_index_outbox.sql", "LICENSE", "NOTICE"])
  assert.ok(listing.stdout.split("\n").includes(`package/${file}`), `missing package file: ${file}`);
assert.ok(!listing.stdout.includes("package/eval/"));

const { createPostgresStore } = await import(pathToFileURL(join(root, "dist/index.js")));
const sql = [];
const client = { query: async (statement) => { sql.push(statement); return { rows: [] }; }, release() {} };
await createPostgresStore({ connect: async () => client }).migrate();
assert.deepEqual(sql.slice(0, 2), ["begin", "select pg_advisory_xact_lock(1213811052,1)"]);
assert.equal(sql.at(-1), "commit");
assert.ok(sql.some((statement) => statement.toLowerCase().includes("create table")), "migration SQL was not loaded");

const temp = await mkdtemp(join(tmpdir(), "hyperag-npm-consumer-"));
try {
  await mkdir(join(temp, "node_modules"));
  const unpack = spawnSync("tar", ["-xzf", archive, "-C", temp], { encoding: "utf8" });
  assert.equal(unpack.status, 0, unpack.stderr);
  await symlink(join(temp, "package"), join(temp, "node_modules/hyperag"), "dir");
  await writeFile(join(temp, "consumer.mts"), 'import { Hyperag, createPostgresStore } from "hyperag";\nimport { loadPlugin } from "hyperag/plugin";\nvoid Hyperag; void createPostgresStore; void loadPlugin;\n');
  await writeFile(join(temp, "consumer.mjs"), 'import { loadPlugin } from "hyperag/plugin";\nif (typeof loadPlugin !== "function") process.exit(1);\n');
  const runtime = spawnSync(process.execPath, [join(temp, "consumer.mjs")], { encoding: "utf8" });
  assert.equal(runtime.status, 0, runtime.stdout + runtime.stderr);
  const check = spawnSync(process.execPath, [join(root, "node_modules/typescript/bin/tsc"), "--ignoreConfig", "--noEmit", "--strict", "--skipLibCheck", "--module", "NodeNext", "--moduleResolution", "NodeNext", "--target", "ES2022", join(temp, "consumer.mts")], { encoding: "utf8" });
  assert.equal(check.status, 0, check.stdout + check.stderr);
} finally {
  await rm(temp, { recursive: true, force: true });
}
console.log("Package smoke: SQL assets, library migration and NodeNext consumer passed.");
