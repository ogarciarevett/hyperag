import { afterAll, beforeAll, expect, test } from "bun:test";

import { createHandler, tokenHash } from "../src/server";
import { freshDatabase, open, uniq } from "./helpers";

const db = await freshDatabase();
const scope = uniq("svc");
let ctx: Awaited<ReturnType<typeof open>>;
let server: ReturnType<typeof Bun.serve>;
const call = (token: string | null, method: string, path: string, body?: unknown) =>
  fetch(new URL(path, server.url), {
    method,
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });

beforeAll(async () => {
  ctx = await open(db.url, { admins: ["root"] });
  server = Bun.serve({
    port: 0,
    fetch: createHandler(ctx.hyperag, {
      tokens: { [tokenHash("root-token")]: "root", [tokenHash("ann-token")]: "ann", [tokenHash("ben-token")]: "ben" },
    }),
  });
}, 60_000);
afterAll(async () => {
  server.stop();
  await ctx.close();
  await db.drop();
});

test("no token or an unknown token is 401", async () => {
  expect((await call(null, "GET", "/v1/whoami")).status).toBe(401);
  expect((await call("forged", "GET", "/v1/whoami")).status).toBe(401);
});

test("identity comes from the token, never from the body", async () => {
  expect(await (await call("ann-token", "GET", "/v1/whoami")).json()).toEqual({ principal: "ann" });
  // ann claims to be root in the body; the grant route ignores it and refuses a non-admin.
  const r = await call("ann-token", "POST", "/v1/grants", { principal: "ann", scope, permission: "write", as: "root" });
  expect(r.status).toBe(403);
});

test("scopes returns only the authenticated caller's current read and write grants", async () => {
  expect((await call(null, "GET", "/v1/scopes")).status).toBe(401);
  expect((await call("forged", "GET", "/v1/scopes")).status).toBe(401);
  expect(await (await call("ann-token", "GET", "/v1/scopes")).json()).toEqual({ read: [], write: [] });
  const readScope = uniq("scope_read");
  const writeScope = uniq("scope_write");
  const foreignScope = uniq("scope_foreign");
  for (const [principal, scope, permission] of [
    ["ann", readScope, "read"], ["ann", writeScope, "write"], ["ben", foreignScope, "read"],
  ])
    expect((await call("root-token", "POST", "/v1/grants", { principal, scope, permission })).status).toBe(200);
  const response = await call("ann-token", "GET", "/v1/scopes?principal=ben");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ read: [readScope], write: [writeScope] });
  expect((await call("ann-token", "GET", "/v1/scopes/ben")).status).toBe(404);
  expect((await call("ann-token", "POST", "/v1/scopes", { principal: "ben" })).status).toBe(404);
  expect((await call("root-token", "DELETE", "/v1/grants", { principal: "ann", scope: writeScope, permission: "write" })).status).toBe(200);
  expect(await (await call("ann-token", "GET", "/v1/scopes")).json()).toEqual({ read: [readScope], write: [] });
  expect((await call("root-token", "DELETE", "/v1/grants", { principal: "ann", scope: readScope, permission: "read" })).status).toBe(200);
});

test("grant, write, read, revoke through the service", async () => {
  for (const [p, perm] of [["ann", "write"], ["ann", "read"], ["ben", "read"]])
    expect((await call("root-token", "POST", "/v1/grants", { principal: p, scope, permission: perm })).status).toBe(200);
  const w = await call("ann-token", "POST", "/v1/memories", { scope, source: "svc", text: "Service test: the build cache lives in bucket hyperag-cache." });
  expect(w.status).toBe(200);
  const { id } = (await w.json()) as { id: string };
  expect((await call("ben-token", "POST", "/v1/memories", { scope, source: "svc", text: "ben may not write" })).status).toBe(403);
  const ctxRes = (await (await call("ben-token", "POST", "/v1/context", { query: "where does the build cache live?" })).json()) as {
    citations: Array<{ memoryId: string }>;
  };
  expect(ctxRes.citations[0]?.memoryId).toBe(id);
  await call("root-token", "DELETE", "/v1/grants", { principal: "ben", scope, permission: "read" });
  const after = (await (await call("ben-token", "POST", "/v1/retrieve", { query: "build cache bucket" })).json()) as { items: unknown[] };
  expect(after.items).toEqual([]);
  expect((await call("ben-token", "GET", `/v1/memories/${id}/explain`)).status).toBe(404);
});

test("invalid bodies are 400, never 500", async () => {
  expect((await call("ann-token", "POST", "/v1/memories", { scope, source: "x" })).status).toBe(400);
  expect((await call("ann-token", "POST", "/v1/memories", { scope, source: "x", text: 5 })).status).toBe(400);
  expect((await call("ann-token", "PATCH", "/v1/memories/m_x", { text: "t", source: "s" })).status).toBe(400);
  expect((await call("ann-token", "POST", "/v1/retrieve", { query: "q", limit: 10_000 })).status).toBe(400);
});

test("an explicit id is addressable with its encoded scope prefix", async () => {
  const w = (await (await call("ann-token", "POST", "/v1/memories", { scope, id: "runbook-7", source: "svc", text: "Runbook 7 restarts the queue." })).json()) as { id: string };
  expect(w.id).toBe(`${scope}/runbook-7`);
  const r = await call("ann-token", "GET", `/v1/memories/${encodeURIComponent(w.id)}/explain`);
  expect(r.status).toBe(200);
  expect(((await r.json()) as { id: string }).id).toBe(w.id);
});

test("a stale update is 409 with the current version", async () => {
  const w = (await (await call("ann-token", "POST", "/v1/memories", { scope, source: "svc", text: "Owner of the deploy key is Ann." })).json()) as { id: string };
  expect((await call("ann-token", "PATCH", `/v1/memories/${w.id}`, { text: "Owner of the deploy key is Ben.", source: "svc", expectedVersion: 1 })).status).toBe(200);
  const stale = await call("ann-token", "PATCH", `/v1/memories/${w.id}`, { text: "Owner is Cy.", source: "svc", expectedVersion: 1 });
  expect(stale.status).toBe(409);
  expect(await stale.json()).toMatchObject({ error: { code: "version_conflict", currentVersion: 2 } });
});
