// Checkout-only harness: isolated database, scoped identity and temporary HTTP service.
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Pool } from "pg";
import { Hyperag, createHandler, createNeo4jGraph, createPostgresStore, tokenHash, type HyperagConfig } from "../../src/index";
import { freshDatabase, NEO4J, uniq } from "../../test/helpers";

export async function demo(run: (env: {
  hyperag: Hyperag; store: HyperagConfig["store"]; scope: string;
  url: string; tokenFile: string;
  call<T>(path: string, body?: unknown): Promise<T>;
  serve(next: Hyperag): void;
}) => Promise<void>, config: Partial<HyperagConfig> = {}) {
  const db = await freshDatabase();
  const pool = new Pool({ connectionString: db.url, max: 2 });
  const dir = await mkdtemp(join(tmpdir(), "hyperag-readme-"));
  const tokenFile = join(dir, "agent.token");
  const token = crypto.randomUUID();
  const scope = uniq("readme");
  const store = createPostgresStore(pool);
  const graph = createNeo4jGraph(NEO4J);
  const hyperag = new Hyperag({ store, embedder: null, graph, admins: ["admin"], ...config });
  let server: ReturnType<typeof Bun.serve> | undefined;
  const auth = { tokens: { [tokenHash(token)]: "reader" } };
  try {
    await writeFile(tokenFile, token, { mode: 0o600 });
    await hyperag.init();
    for (const permission of ["read", "write"] as const)
      await hyperag.grant("admin", "reader", scope, permission);
    server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: createHandler(hyperag, auth) });
    assert(![7481, 7482].includes(server.port!));
    const call = async <T>(path: string, body?: unknown): Promise<T> => {
      const response = await fetch(new URL(path, server!.url), {
        method: body === undefined ? "GET" : "POST",
        headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      assert.equal(response.status, 200, `HTTP ${path}`);
      return await response.json() as T;
    };
    assert.deepEqual(await call("/v1/whoami"), { principal: "reader" });
    await run({ hyperag, store, scope, url: server.url.origin, tokenFile, call,
      serve: (next) => server!.reload({ fetch: createHandler(next, auth) }) });
  } finally {
    server?.stop(true);
    // Remove only this example's graph nodes before dropping its Postgres database.
    try {
      const rows = await pool.query<{ id: string }>("select distinct id from memories where scope = $1", [scope]);
      for (const { id } of rows.rows) await graph.deleteMemory(id);
    } finally {
      await pool.end();
      await db.drop();
      await rm(dir, { recursive: true, force: true });
    }
  }
}
