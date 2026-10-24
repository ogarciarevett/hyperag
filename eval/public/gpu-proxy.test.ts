import { expect, test } from "bun:test";
import { createServer } from "node:http";
import { serveGpuProxy } from "./gpu-proxy";

test("all shared GPU services preserve requests and use the same bounded reset policy", async () => {
  const calls = new Map<string, number>();
  const upstream = createServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk;
    const key = req.url!;
    const n = (calls.get(key) ?? 0) + 1;
    calls.set(key, n);
    if (key.includes("always-reset") || (!key.includes("bad-request") && n === 1)) {
      req.socket.destroy();
      return;
    }
    res.writeHead(key.includes("bad-request") ? 400 : 200, { "content-type": "application/json" });
    res.end(JSON.stringify({ path: key, body, auth: req.headers.authorization }));
  });
  await new Promise<void>((resolve) => upstream.listen(0, "127.0.0.1", resolve));
  const address = upstream.address();
  if (!address || typeof address === "string") throw new Error("missing upstream address");
  const target = `http://127.0.0.1:${address.port}`;
  const proxy = serveGpuProxy({ llm: target, embed: target, rerank: target }, 0);
  try {
    for (const route of ["llm", "embed", "rerank"]) {
      const path = `/${route}/v1/${route}`;
      const response = await fetch(`http://127.0.0.1:${proxy.port}${path}?q=1`, {
        method: "POST", body: "same input", headers: { authorization: "Bearer test-only" },
      });
      expect(await response.json()).toEqual({ path: `/v1/${route}?q=1`, body: "same input", auth: "Bearer test-only" });
      expect(calls.get(`/v1/${route}?q=1`)).toBe(2);
    }
    const bad = await fetch(`http://127.0.0.1:${proxy.port}/llm/bad-request`);
    expect(bad.status).toBe(400);
    expect(calls.get("/bad-request")).toBe(1);
    const reset = await fetch(`http://127.0.0.1:${proxy.port}/llm/always-reset`);
    expect(reset.status).toBe(502);
    expect(calls.get("/always-reset")).toBe(3);
  } finally {
    proxy.stop(true);
    upstream.closeAllConnections();
    await new Promise<void>((resolve) => upstream.close(() => resolve()));
  }
}, 10_000);
