import { expect, test } from "bun:test";
import { createServer } from "node:http";
import { gpuRequest } from "./gpu-request";

for (const mode of ["recover", "reset", "bad-request"] as const) {
  test(`GPU transport ${mode}`, async () => {
    let calls = 0;
    const bodies: string[] = [];
    const server = createServer(async (req, res) => {
      let body = "";
      for await (const chunk of req) body += chunk;
      bodies.push(body);
      calls++;
      if (mode === "reset" || (mode === "recover" && calls === 1)) {
        req.socket.destroy();
        return;
      }
      res.writeHead(mode === "bad-request" ? 400 : 200, { "content-type": "application/json" });
      res.end(JSON.stringify({ embeddings: [1, 2, 3] }));
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("missing test server address");
    try {
      const request = gpuRequest(`http://127.0.0.1:${address.port}`, "/v2/embed", { inputs: ["same input"] });
      if (mode === "recover") {
        expect(await request).toEqual({ embeddings: [1, 2, 3] });
        expect(calls).toBe(2);
        expect(bodies[0]).toBe(bodies[1]);
      } else if (mode === "reset") {
        await expect(request).rejects.toMatchObject({ code: "ECONNRESET" });
        expect(calls).toBe(3);
      } else {
        await expect(request).rejects.toThrow("gpu endpoint /v2/embed: 400");
        expect(calls).toBe(1);
      }
    } finally {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  }, 10_000);
}
