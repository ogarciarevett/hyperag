import { Pool } from "pg";

import { createLocalEmbedder } from "../src/adapters/embedders";
import { createNeo4jGraph } from "../src/adapters/neo4j";
import { createPostgresStore } from "../src/adapters/postgres";
import { Hyperag, type HyperagConfig } from "../src/core";
import type { Embedder } from "../src/types";

export const PG_ADMIN_URL = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";
export const NEO4J = {
  baseUrl: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474",
  username: "neo4j",
  password: process.env.HYPERAG_NEO4J_PASSWORD ?? "fake-test-only",
};

let shared: Embedder | undefined;
/** One model load per test process: the local embedder takes seconds to load. */
export const embedder = () => (shared ??= createLocalEmbedder());

/** A fresh database per test file, so files never see each other's rows. */
export async function freshDatabase(): Promise<{ url: string; drop(): Promise<void> }> {
  const name = `${process.env.HYPERAG_DB_PREFIX ?? ""}t_${Date.now()}_${Math.floor(Math.random() * 1e6)}`;
  const admin = new Pool({ connectionString: PG_ADMIN_URL, max: 1 });
  await admin.query(`create database ${name}`);
  const url = PG_ADMIN_URL.replace(/\/[^/]+$/u, `/${name}`);
  return {
    url,
    async drop() {
      await admin.query(`drop database if exists ${name} with (force)`);
      await admin.end();
    },
  };
}

/** A Hyperag on its own pool, as a fresh process would build it; `close` ends the pool. */
export async function open(url: string, config: Partial<HyperagConfig> = {}) {
  const pool = new Pool({ connectionString: url, max: 4 });
  // Dropping the test database ends idle connections; that error is expected at teardown.
  pool.on("error", () => undefined);
  const hyperag = new Hyperag({
    store: createPostgresStore(pool),
    embedder: embedder(),
    graph: createNeo4jGraph(NEO4J),
    admins: ["admin"],
    ...config,
  });
  await hyperag.init();
  return { hyperag, pool, close: () => pool.end() };
}

/** Graph node ids must not collide across test files sharing one Neo4j. */
export const uniq = (label: string) => `${label}_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 7)}`;
