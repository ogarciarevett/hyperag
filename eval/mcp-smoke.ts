// End-to-end MCP check: starts a Hyperag service, then `hyperag mcp` on stdio for two agents, and
// runs ingest, relate, search with citations, explain, a refused cross-scope read, a model list and
// health through MCP tool calls only.
//   HYPERAG_SERVICE_CMD="target/release/hyperag serve" bun eval/mcp-smoke.ts
import { Pool } from "pg";

import { startService } from "./hyperag-http";
import { connectStdio } from "./mcp-client";

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}mcp_smoke_${Date.now()}`;
const admin = new Pool({ connectionString: PG, max: 1 });
await admin.query(`create database ${name}`);
const svc = await startService({
  pgUrl: PG.replace(/\/[^/]+$/u, `/${name}`),
  principals: ["admin", "alice", "bob"],
  admins: ["admin"],
  neo4j: true,
  embedding: process.env.HYPERAG_SMOKE_EMBEDDING ?? "none",
  reranker: "none",
});
const failures: string[] = [];
let checks = 0;
const check = (what: string, ok: boolean, detail = "") => {
  checks++;
  console.log(`${ok ? "pass" : "FAIL"}  ${what}${detail ? `  (${detail})` : ""}`);
  if (!ok) failures.push(what);
};
try {
  for (const [who, scope, perm] of [["alice", "team", "write"], ["alice", "team", "read"], ["bob", "team", "read"], ["alice", "secret", "write"], ["alice", "secret", "read"]])
    await svc.must("admin", "POST", "/v1/grants", { principal: who, scope, permission: perm });
  const bin = (process.env.HYPERAG_SERVICE_CMD ?? "target/release/hyperag serve").split(" ")[0]!;
  const agent = (who: string) => connectStdio(bin, ["mcp", "--url", svc.base], { ...process.env, HYPERAG_TOKEN: `token-${who}` });
  const alice = await agent("alice");
  const bob = await agent("bob");

  const tools = (await alice.listTools()).map((t) => t.name).sort();
  check("tools listed", ["delete", "explain", "health", "ingest", "invalidate", "models_list", "models_set", "relate", "search", "update"].every((t) => tools.includes(t)), tools.join(","));
  check("health names the caller", (await bob.call("health", {})).text.includes('"bob"'));

  const ev = JSON.parse((await alice.call("ingest", { scope: "team", source: "ci run 77", kind: "evidence", text: "CI run 77: the deploy failed because the migration lock timed out after 30 seconds." })).text);
  const fi = JSON.parse((await alice.call("ingest", { scope: "team", source: "alice analysis", kind: "finding", text: "Finding: raise the migration lock timeout to 120 seconds to fix the failing deploy." })).text);
  check("ingest returns ids", typeof ev.id === "string" && typeof fi.id === "string");
  check("relate", !(await alice.call("relate", { from: fi.id, to: ev.id, type: "SUPPORTED_BY" })).isError);
  const secret = await alice.call("ingest", { scope: "secret", source: "vault", text: "The deploy key passphrase is stored in vault path ops/deploy." });
  check("alice writes her private scope", !secret.isError && JSON.parse(secret.text).created === true);
  const own = JSON.parse((await alice.call("search", { query: "deploy key passphrase vault path" })).text);
  check("alice finds her private memory", JSON.stringify(own.citations).includes("vault"));

  const found = await bob.call("search", { query: "why did the deploy fail and how do we fix it?", maxTokens: 400 });
  const ctx = JSON.parse(found.text);
  check("bob's search cites both memories", [ev.id, fi.id].every((id) => ctx.citations.some((c: { memoryId: string }) => c.memoryId === id)), `${ctx.citations.length} citations`);
  check("context text is numbered", /^\[1\] /u.test(ctx.text));

  const leak = JSON.parse((await bob.call("search", { query: "deploy key passphrase vault path" })).text);
  check("bob cannot see alice's secret scope", !JSON.stringify(leak.citations).includes("vault"));
  const write = await bob.call("ingest", { scope: "team", source: "bob", text: "bob has no write grant" });
  check("bob's write is refused", write.isError && write.text.includes("forbidden"));

  const why = JSON.parse((await bob.call("explain", { id: fi.id })).text);
  check("explain shows version and relation", why.versions?.[0]?.author === "alice" && why.relations?.some((r: { type: string }) => r.type === "SUPPORTED_BY"));
  check("models_list answers", (await alice.call("models_list", {})).text.includes('"spaces"'));
  const t0 = performance.now();
  const refused = await alice.call("models_set", { embedding: "hf:BAAI/bge-large-en-v1.5" });
  const ms = performance.now() - t0;
  // Refused before any download: a large model would take many seconds to fetch and load.
  check("models_set is refused for a non-admin before loading anything", refused.isError && refused.text.includes("forbidden") && ms < 2_000, `${ms.toFixed(0)} ms`);

  // MCP over HTTP at /mcp: initialize, then tool calls with and without a bearer token.
  const rpc = async (body: unknown, headers: Record<string, string>) => {
    const r = await fetch(`${svc.base}/mcp`, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream", ...headers },
      body: JSON.stringify(body),
    });
    const text = await r.text();
    const data = text.split("\n").filter((l) => l.startsWith("data: ") && l.length > 6).map((l) => JSON.parse(l.slice(6)));
    return { session: r.headers.get("mcp-session-id"), msg: data.at(-1) as { result?: { content: Array<{ text: string }>; isError: boolean } } | undefined };
  };
  const auth = { authorization: "Bearer token-bob" };
  const init = await rpc({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "smoke", version: "0" } } }, auth);
  const sid = { "mcp-session-id": init.session ?? "" };
  await rpc({ jsonrpc: "2.0", method: "notifications/initialized" }, { ...auth, ...sid });
  const viaHttp = await rpc({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "health", arguments: {} } }, { ...auth, ...sid });
  check("HTTP MCP tool call carries the caller's token", viaHttp.msg?.result?.content[0]?.text.includes('"bob"') === true);
  const anon = await rpc({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "health", arguments: {} } }, sid);
  check("HTTP MCP tool call without a token is refused", anon.msg?.result?.isError === true && anon.msg.result.content[0]!.text.includes("unauthenticated"));

  alice.close();
  bob.close();
} finally {
  await svc.stop();
  await admin.query(`drop database ${name} with (force)`);
  await admin.end();
}
console.log(failures.length === 0 ? `MCP smoke: all ${checks} checks passed` : `MCP smoke: ${failures.length} of ${checks} failed`);
process.exit(failures.length === 0 ? 0 : 1);
