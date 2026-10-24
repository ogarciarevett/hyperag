import { withResetRetry } from "./systems/gpu-request";

/** One reset policy for every system and all three shared GPU services. */
export function serveGpuProxy(upstreams: Record<string, string>, port: number) {
  return Bun.serve({
    hostname: "127.0.0.1", port,
    idleTimeout: 0,
    async fetch(req) {
      const incoming = new URL(req.url);
      if (incoming.pathname === "/health") return new Response("ok");
      const route = incoming.pathname.split("/")[1]!;
      const upstream = upstreams[route];
      if (!upstream) return new Response("unknown GPU service", { status: 404 });
      const headers = new Headers(req.headers);
      headers.delete("host");
      headers.delete("content-length");
      const body = req.method === "GET" || req.method === "HEAD" ? undefined : await req.arrayBuffer();
      try {
        return await withResetRetry(async () => {
          const r = await fetch(`${upstream}${incoming.pathname.slice(route.length + 1)}${incoming.search}`, {
            method: req.method, headers, body, signal: req.signal, keepalive: false,
          });
          // Buffer before replying so a reset while reading cannot leave a half-response.
          const bytes = await r.arrayBuffer();
          const responseHeaders = new Headers(r.headers);
          for (const name of ["content-length", "content-encoding", "transfer-encoding"]) responseHeaders.delete(name);
          return new Response(bytes, { status: r.status, headers: responseHeaders });
        });
      } catch {
        // Never log bodies, credentials, upstream addresses or private error text.
        return new Response("GPU transport failed after bounded recovery", { status: 502 });
      }
    },
  });
}

if (import.meta.main) {
  const host = process.env.GPU_UPSTREAM_HOST;
  if (!host) throw new Error("GPU_UPSTREAM_HOST is required");
  serveGpuProxy({ llm: `http://${host}:8000`, embed: `http://${host}:8001`, rerank: `http://${host}:8002` }, 58400);
}
