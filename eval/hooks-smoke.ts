// The harness hooks and the recent-context endpoint against a running service, on a tiny
// synthetic scope: a prompt is stored, the next session start shows it, a session end is
// recorded, another agent cannot read the scope, and a hook never fails the session.
//   HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/hooks-smoke.ts
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Pool } from "pg";

import { startService } from "./hyperag-http";

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const bin = (process.env.HYPERAG_SERVICE_CMD ?? "target/release/hyperag serve").split(" ")[0]!;
const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}hooks_smoke_${Date.now()}`;
const admin = new Pool({ connectionString: PG, max: 1 });
await admin.query(`create database ${name}`);
const proof = new Pool({ connectionString: PG.replace(/\/[^/]+$/u, `/${name}`), max: 1 });
const svc = await startService({ pgUrl: PG.replace(/\/[^/]+$/u, `/${name}`), principals: ["admin", "agent-a", "agent-b"], admins: ["admin"], embedding: "none", reranker: "none" });
// A synthetic project folder: its git root names the scope.
const project = join(mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "hooks-")), "Demo-Repo");
mkdirSync(join(project, ".git"), { recursive: true });
mkdirSync(join(project, "src"), { recursive: true });

const hook = (event: string, input: unknown, token = "token-agent-a") => {
  const r = Bun.spawnSync([bin, "hook", event], {
    stdin: new TextEncoder().encode(JSON.stringify(input)),
    env: { PATH: process.env.PATH, HOME: process.env.HOME, HYPERAG_URL: svc.base, HYPERAG_TOKEN: token },
  });
  return { code: r.exitCode, out: r.stdout.toString(), err: r.stderr.toString() };
};
const failures: string[] = [];
let checks = 0;
const check = (what: string, ok: boolean, detail = "") => {
  checks++;
  console.log(`${ok ? "pass" : "FAIL"}  ${what}${detail ? `  (${detail})` : ""}`);
  if (!ok) failures.push(what);
};
try {
  for (const p of ["read", "write"]) await svc.must("admin", "POST", "/v1/grants", { principal: "agent-a", scope: "project:demo-repo", permission: p });
  const cwd = join(project, "src");
  check("session start on an empty scope prints nothing", hook("session-start", { session_id: "s1", cwd }).out.trim() === "");
  check("prompt hook exits 0", hook("prompt", { session_id: "s1", cwd, prompt: "Use the blue deploy queue for the demo service." }).code === 0);
  hook("session-end", { session_id: "s1", cwd, reason: "logout" });
  const next = hook("session-start", { session_id: "s2", cwd });
  check("next session start shows the prompt", next.out.includes("blue deploy queue") && next.out.includes("scope project:demo-repo"));
  check("next session start shows the session end", next.out.includes("Session s1 ended (logout)."));
  const recent = await svc.must("agent-a", "GET", "/v1/recent?scope=project%3Ademo-repo&limit=1");
  check("recent honours its limit", (recent.items as unknown[]).length === 1);
  const other = await svc.call("agent-b", "GET", "/v1/recent?scope=project%3Ademo-repo");
  check("another agent cannot read the scope", other.status === 404);
  const scopes = await svc.must("agent-a", "GET", "/v1/scopes") as { read: string[]; write: string[] };
  check("scope preflight exposes only the caller's grants", JSON.stringify(scopes) === JSON.stringify({ read: ["project:demo-repo"], write: ["project:demo-repo"] }));
  const emptyScopes = await svc.must("agent-b", "GET", "/v1/scopes") as { read: string[]; write: string[] };
  check("scope preflight does not expose another principal's grants", emptyScopes.read.length === 0 && emptyScopes.write.length === 0);
  await svc.must("admin", "POST", "/v1/grants", { principal: "agent-b", scope: "project:demo-repo", permission: "read" });
  const snapshot = async () => JSON.stringify((await proof.query(`select json_build_object(
    'grants', (select coalesce(json_agg(g order by principal, scope, permission), '[]') from grants g),
    'grant_log', (select count(*) from grant_log), 'memories', (select count(*) from memories),
    'versions', (select count(*) from memory_versions)) as state`)).rows[0].state);
  const beforeSkip = await snapshot();
  const ungranted = join(project, "..", "Not-Granted");
  mkdirSync(ungranted);
  for (const event of ["session-start", "prompt", "session-end"]) {
    const skipped = hook(event, { session_id: "skipped", cwd: ungranted, prompt: "synthetic input" });
    check(`ungranted project ${event} quietly skips`, skipped.code === 0 && skipped.out === "" && skipped.err === "");
  }
  const readOnly = hook("prompt", { session_id: "read-only", cwd, prompt: "synthetic input" }, "token-agent-b");
  check("read-only project prompt quietly skips", readOnly.code === 0 && readOnly.out === "" && readOnly.err === "");
  check("quiet skips leave memory, versions, grants and grant log unchanged", beforeSkip === await snapshot());
  const bad = hook("session-start", { session_id: "s3", cwd }, "token-nobody");
  check("a bad token never fails the session", bad.code === 0 && bad.out.trim() === "" && bad.err.includes("401"));
  // A real prompt, so the hook does try the network and the connection fails.
  const down = Bun.spawnSync([bin, "hook", "prompt"], {
    stdin: new TextEncoder().encode(JSON.stringify({ session_id: "s4", cwd, prompt: "x" })),
    env: { PATH: process.env.PATH, HOME: process.env.HOME, HYPERAG_URL: "http://127.0.0.1:9", HYPERAG_TOKEN: "x" },
  });
  check("an unreachable service never fails the session", down.exitCode === 0 && down.stderr.toString().includes("hyperag hook prompt"), down.stderr.toString().trim().slice(0, 80));
  for (let i = 0; i < 5; i++) await svc.must("agent-a", "POST", "/v1/memories", { scope: "project:demo-repo", source: "notes", text: `Budget line ${i} with some words to fill the line.` });
  const small = (await svc.must("agent-a", "GET", "/v1/recent?scope=project%3Ademo-repo&maxBytes=200")) as { text: string; items: unknown[] };
  check("recent stays within maxBytes", Buffer.byteLength(small.text) <= 200 && small.items.length >= 1, `${Buffer.byteLength(small.text)} bytes, ${small.items.length} items`);
  await svc.must("agent-a", "POST", "/v1/memories", { scope: "project:demo-repo", source: "doc", text: "y".repeat(3000) });
  const big = (await svc.must("agent-a", "GET", "/v1/recent?scope=project%3Ademo-repo&maxBytes=500")) as { items: unknown[] };
  check("a memory larger than the budget does not hide older ones", big.items.length >= 1, `${big.items.length} items`);
} finally {
  await svc.stop();
  await proof.end();
  await admin.query(`drop database ${name} with (force)`);
  await admin.end();
}
console.log(failures.length === 0 ? `hooks smoke: all ${checks} checks passed` : `hooks smoke: ${failures.length} of ${checks} failed`);
process.exit(failures.length === 0 ? 0 : 1);
