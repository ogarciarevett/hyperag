import { afterAll, beforeAll, expect, spyOn, test } from "bun:test";
import { chmod, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Pool } from "pg";

import { loadPlugin, PluginError } from "hyperag/plugin";
import { createPostgresStore } from "../src/adapters/postgres";
import { Hyperag } from "../src/core";
import { createHandler, tokenHash } from "../src/server";
import { freshDatabase } from "./helpers";

const dir = await mkdtemp(join(tmpdir(), "hyperag-plugin-"));
const tokenFile = join(dir, "agent.token");
const secret = "private-test-agent-token";
await writeFile(tokenFile, secret, { mode: 0o600 });
let db: Awaited<ReturnType<typeof freshDatabase>>;
let pool: Pool;
let server: ReturnType<typeof Bun.serve>;
let hyperag: Hyperag;
let plugin: Awaited<ReturnType<typeof loadPlugin>>;
beforeAll(async () => {
  db = await freshDatabase();
  pool = new Pool({ connectionString: db.url, max: 2 });
  hyperag = new Hyperag({ store: createPostgresStore(pool), embedder: null, graph: null, admins: ["admin"] });
  await hyperag.init();
  await hyperag.grant("admin", "host", "project:demo", "write");
  await hyperag.grant("admin", "host", "project:demo", "read");
  await hyperag.grant("admin", "admin", "secret", "write");
  await hyperag.ingest("admin", { scope: "secret", source: "private", text: "Hidden decision: cedar." });
  server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: createHandler(hyperag, { tokens: { [tokenHash(secret)]: "host" } }) });
  expect([7481, 7482]).not.toContain(server.port);
  plugin = await loadPlugin({ url: server.url.origin, tokenFile });
});
afterAll(async () => {
  server?.stop(true);
  await pool?.end();
  await db?.drop();
  await rm(dir, { recursive: true, force: true });
});

test("built package saves, recalls, searches and checks authenticated health", async () => {
  expect(await plugin.health()).toEqual({ principal: "host" });
  const saved = await plugin.save({ scope: "project:demo", source: "host", text: "Deployment decision: cedar.", idempotencyKey: "cedar" });
  expect(saved.created).toBe(true);
  expect((await plugin.recall("cedar")).items.map((item) => item.memory.id)).toEqual([saved.id]);
  const context = await plugin.search("cedar");
  expect(context.citations[0]?.memoryId).toBe(saved.id);
  expect(context.retrieval.insufficientEvidence).toBe(false);
  await expect(plugin.save({ scope: "secret", source: "host", text: "private" })).rejects.toMatchObject({ code: "http_error", status: 403 });
  await hyperag.revoke("admin", "host", "project:demo", "read");
  expect((await plugin.recall("cedar")).items).toEqual([]);
});

test("invalid credentials, unsafe files and URL configuration fail closed", async () => {
  const bad = join(dir, "bad.token");
  await writeFile(bad, "wrong-token", { mode: 0o600 });
  const invalid = await loadPlugin({ url: server.url.origin, tokenFile: bad });
  await expect(invalid.health()).rejects.toMatchObject({ code: "http_error", status: 401 });
  await chmod(bad, 0o644);
  await expect(loadPlugin({ url: server.url.origin, tokenFile: bad })).rejects.toMatchObject({ code: "invalid_token_file" });
  const link = join(dir, "link");
  await symlink(tokenFile, link);
  for (const file of [link, dir, join(dir, "missing")])
    await expect(loadPlugin({ url: server.url.origin, tokenFile: file })).rejects.toMatchObject({ code: "invalid_token_file" });
  await chmod(bad, 0o600);
  for (const value of ["", "token\ninjection", "x".repeat(4097)]) {
    await writeFile(bad, value);
    await expect(loadPlugin({ url: server.url.origin, tokenFile: bad })).rejects.toMatchObject({ code: "invalid_token_file" });
  }
  for (const url of ["http://example.com", "https://user:pass@example.com", "https://example.com/path", "https://example.com/?token=secret"])
    await expect(loadPlugin({ url, tokenFile })).rejects.toMatchObject({ code: "invalid_url" });
  await expect(loadPlugin({ url: server.url.origin, tokenFile, timeoutMs: 0 })).rejects.toMatchObject({ code: "invalid_timeout" });
});

test("HTTP, redirect, timeout and malformed response errors cannot echo private data", async () => {
  let mode = "http";
  let redirected = 0;
  const target = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => { redirected++; return Response.json({}); } });
  const failing = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch() {
    if (mode === "redirect") return Response.redirect(target.url);
    if (mode === "timeout") await Bun.sleep(150);
    if (mode === "malformed") return new Response(secret);
    return Response.json({ error: { message: secret } }, { status: 500 });
  } });
  const log = spyOn(console, "error").mockImplementation(() => {});
  try {
    const client = await loadPlugin({ url: failing.url.origin, tokenFile, timeoutMs: 1_000 });
    for (mode of ["http", "redirect", "timeout", "malformed"]) {
      const active = mode === "timeout" ? await loadPlugin({ url: failing.url.origin, tokenFile, timeoutMs: 30 }) : client;
      const error = await active.health().catch((error: unknown) => error);
      expect(error).toMatchObject({ code: mode === "http" ? "http_error" : "request_failed", status: mode === "http" ? 500 : 0 });
      expect(error).toBeInstanceOf(PluginError);
      expect(JSON.stringify(error)).not.toContain(secret);
      expect(String(error)).not.toContain(secret);
    }
    expect(redirected).toBe(0);
    // Driver exceptions may contain memory text; the reference service must suppress them.
    const ingest = spyOn(hyperag, "ingest").mockRejectedValueOnce(new Error(secret));
    try { await expect(plugin.save({ scope: "project:demo", source: "x", text: secret })).rejects.toMatchObject({ status: 500 }); }
    finally { ingest.mockRestore(); }
    expect(log.mock.calls).toEqual([["hyperag request failed"]]);
  } finally {
    log.mockRestore();
    failing.stop(true);
    target.stop(true);
  }
});
