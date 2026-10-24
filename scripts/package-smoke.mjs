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
for (const file of ["dist/index.js", "dist/index.d.ts", "crates/hyperag/migrations/0001_init.sql", "crates/hyperag/migrations/0002_settings.sql", "LICENSE", "NOTICE"])
  assert.ok(listing.stdout.split("\n").includes(`package/${file}`), `missing package file: ${file}`);
assert.ok(!listing.stdout.includes("package/eval/"));

const { createPostgresStore } = await import(pathToFileURL(join(root, "dist/index.js")));
const sql = [];
await createPostgresStore({ query: async (statement) => { sql.push(statement); return { rows: [] }; } }).migrate();
assert.ok(sql.some((statement) => statement.toLowerCase().includes("create table")), "migration SQL was not loaded");

const temp = await mkdtemp(join(tmpdir(), "hyperag-npm-consumer-"));
try {
  await mkdir(join(temp, "node_modules"));
  await symlink(root, join(temp, "node_modules/hyperag"), "dir");
  await writeFile(join(temp, "consumer.mts"), 'import { Hyperag, createPostgresStore } from "hyperag";\nvoid Hyperag; void createPostgresStore;\n');
  const check = spawnSync(process.execPath, [join(root, "node_modules/typescript/bin/tsc"), "--noEmit", "--strict", "--skipLibCheck", "--module", "NodeNext", "--moduleResolution", "NodeNext", "--target", "ES2022", join(temp, "consumer.mts")], { encoding: "utf8" });
  assert.equal(check.status, 0, check.stdout + check.stderr);
} finally {
  await rm(temp, { recursive: true, force: true });
}
console.log("Package smoke: SQL assets, library migration and NodeNext consumer passed.");
