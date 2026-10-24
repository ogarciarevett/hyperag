// A client for the Hyperag HTTP API and a launcher for one service process, so the TypeScript and
// the Rust builds are scored by the same harness commands. The service command comes from
// HYPERAG_SERVICE_CMD, for example `bun src/main.ts` or `target/release/hyperag serve`.
import { type ChildProcess, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export type ServiceOptions = Readonly<{
  pgUrl: string;
  principals: readonly string[];
  admins: readonly string[];
  embedding?: string;
  reranker?: string;
  neo4j?: boolean;
  port?: number;
  /** Extra environment for the service (the TS build reads HYPERAG_EMBEDDER / HYPERAG_RERANKER). */
  env?: Record<string, string>;
}>;

const tokenOf = (principal: string) => `token-${principal}`;

export async function startService(o: ServiceOptions) {
  const dir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "hyperag-svc-"));
  const tokens = Object.fromEntries(o.principals.map((p) => [createHash("sha256").update(tokenOf(p)).digest("hex"), p]));
  writeFileSync(join(dir, "tokens.json"), JSON.stringify(tokens));
  const port = o.port ?? 7600 + Math.floor(Math.random() * 300);
  const cmd = (process.env.HYPERAG_SERVICE_CMD ?? "bun src/main.ts").split(" ");
  const env: Record<string, string> = {
    ...(process.env as Record<string, string>),
    HYPERAG_PG_URL: o.pgUrl,
    HYPERAG_TOKENS_FILE: join(dir, "tokens.json"),
    HYPERAG_ADMINS: o.admins.join(","),
    HYPERAG_PORT: String(port),
    ...(o.embedding ? { HYPERAG_EMBEDDING: o.embedding } : {}),
    ...(o.reranker ? { HYPERAG_RERANKER: o.reranker } : {}),
    ...(o.neo4j ? { HYPERAG_NEO4J_URL: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474", HYPERAG_NEO4J_USER: "neo4j", HYPERAG_NEO4J_PASSWORD: "fake-test-only" } : {}),
    ...o.env,
  };
  let child: ChildProcess = spawn(cmd[0]!, cmd.slice(1), { env, stdio: ["ignore", "ignore", "inherit"] });
  const base = `http://127.0.0.1:${port}`;
  const ready = async () => {
    for (let i = 0; i < 600; i++) {
      const r = await fetch(`${base}/v1/whoami`, { headers: { authorization: `Bearer ${tokenOf(o.principals[0]!)}` } }).catch(() => null);
      if (r?.ok) return;
      await Bun.sleep(100);
    }
    throw new Error("service did not start");
  };
  await ready();
  const call = async (as: string, method: string, path: string, body?: unknown) => {
    const r = await fetch(base + path, {
      method,
      headers: { authorization: `Bearer ${tokenOf(as)}`, "content-type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const json = (await r.json().catch(() => ({}))) as Record<string, unknown> & { error?: { code: string } };
    return { status: r.status, json };
  };
  const must = async (as: string, method: string, path: string, body?: unknown) => {
    const r = await call(as, method, path, body);
    if (r.status !== 200) throw Object.assign(new Error(`${method} ${path}: ${r.status} ${JSON.stringify(r.json)}`), { code: r.json.error?.code });
    return r.json;
  };
  return {
    base,
    call,
    must,
    pid: () => child.pid,
    async restart() {
      child.kill();
      await new Promise((r) => child.once("exit", r));
      child = spawn(cmd[0]!, cmd.slice(1), { env, stdio: ["ignore", "ignore", "inherit"] });
      await ready();
    },
    async stop() {
      child.kill();
      await new Promise((r) => child.once("exit", r));
    },
    /** macOS physical footprint in bytes (what the memory pressure counts), or null elsewhere. */
    footprint: () => {
      const out = Bun.spawnSync(["vmmap", "--summary", String(child.pid)]).stdout.toString();
      const m = /Physical footprint:\s+([\d.]+)([KMG])/u.exec(out);
      return m ? Number(m[1]) * { K: 1e3, M: 1e6, G: 1e9 }[m[2] as "K"] : null;
    },
    /** Resident memory of the service process in bytes (macOS/Linux `ps`). */
    rss: () => Number(Bun.spawnSync(["ps", "-o", "rss=", "-p", String(child.pid)]).stdout.toString().trim()) * 1024,
  };
}

export type Service = Awaited<ReturnType<typeof startService>>;
