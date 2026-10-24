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
const svc = await startService({ pgUrl: PG.replace(/\/[^/]+$/u, `/${name}`), principals: ["admin", "agent-a", "agent-b"], admins: ["admin"], embedding: "none", reranker: "none" });
// A synthetic project folder: its git root names the scope.
const project = join(mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "hooks-")), "Demo-Repo");
mkdirSync(join(project, ".git"), { recursive: true });
mkdirSync(join(project, "src"), { recursive: true });

const hook = (event: string, input: unknown, token = "token-agent-a") => {
  const r = Bun.spawnSync([bin, "hook", event], {
    stdin: new TextEncoder().encode(JSON.stringify(input)),
    env: { ...process.env, HYPERAG_URL: svc.base, HYPERAG_TOKEN: token },
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
  const bad = hook("session-start", { session_id: "s3", cwd }, "token-nobody");
  check("a bad token never fails the session", bad.code === 0 && bad.out.trim() === "" && bad.err.includes("401"));
  // A real prompt, so the hook does try the network and the connection fails.
  const down = Bun.spawnSync([bin, "hook", "prompt"], {
    stdin: new TextEncoder().encode(JSON.stringify({ session_id: "s4", cwd, prompt: "x" })),
    env: { ...process.env, HYPERAG_URL: "http://127.0.0.1:9", HYPERAG_TOKEN: "x" },
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
  await admin.query(`drop database ${name} with (force)`);
  await admin.end();
}
console.log(failures.length === 0 ? `hooks smoke: all ${checks} checks passed` : `hooks smoke: ${failures.length} of ${checks} failed`);
process.exit(failures.length === 0 ? 0 : 1);
