import { readFileSync } from "node:fs";
import { Pool } from "pg";

import { createLocalEmbedder, createLocalReranker } from "./adapters/embedders.js";
import { createNeo4jGraph } from "./adapters/neo4j.js";
import { createPostgresStore } from "./adapters/postgres.js";
import { Hyperag } from "./core.js";

/**
 * Builds Hyperag from the environment. Every remote component is a URL, so local and remote
 * deployments differ only in configuration:
 *   HYPERAG_PG_URL        Postgres with pgvector (required)
 *   HYPERAG_NEO4J_URL     Neo4j HTTP endpoint; unset, no graph channel
 *   HYPERAG_NEO4J_USER / HYPERAG_NEO4J_PASSWORD
 *   HYPERAG_EMBEDDER      `local` (default) or `none`
 *   HYPERAG_RERANKER      `local` or `none` (default)
 *   HYPERAG_ADMINS        comma-separated principals that may grant and revoke
 */
export async function hyperagFromEnv(env = process.env): Promise<{ hyperag: Hyperag; pool: Pool }> {
  const url = env.HYPERAG_PG_URL;
  if (!url) throw new Error("HYPERAG_PG_URL is required");
  const choice = (name: string, allowed: readonly string[], fallback: string) => {
    const value = env[name] ?? fallback;
    if (!allowed.includes(value)) throw new Error(`${name} must be one of ${allowed.join(", ")}`);
    return value;
  };
  const embedder = choice("HYPERAG_EMBEDDER", ["local", "none"], "local");
  const reranker = choice("HYPERAG_RERANKER", ["local", "none"], "none");
  const pool = new Pool({ connectionString: url, max: 8 });
  const hyperag = new Hyperag({
    store: createPostgresStore(pool),
    admins: (env.HYPERAG_ADMINS ?? "").split(",").filter(Boolean),
    embedder: embedder === "none" ? null : createLocalEmbedder(),
    reranker: reranker === "local" ? createLocalReranker() : null,
    graph: env.HYPERAG_NEO4J_URL
      ? createNeo4jGraph({
          baseUrl: env.HYPERAG_NEO4J_URL,
          username: env.HYPERAG_NEO4J_USER ?? "neo4j",
          password: env.HYPERAG_NEO4J_PASSWORD ?? "",
        })
      : null,
  });
  await hyperag.init();
  return { hyperag, pool };
}

/** `HYPERAG_TOKENS_FILE`: JSON `{ "<sha256 of token>": "<principal>" }`. */
export function authFromEnv(env = process.env) {
  const file = env.HYPERAG_TOKENS_FILE;
  if (!file) throw new Error("HYPERAG_TOKENS_FILE is required");
  const tokens = JSON.parse(readFileSync(file, "utf8")) as unknown;
  const valid =
    tokens !== null &&
    typeof tokens === "object" &&
    Object.entries(tokens).every(([hash, who]) => /^[0-9a-f]{64}$/u.test(hash) && typeof who === "string" && who.length > 0);
  if (!valid) throw new Error("HYPERAG_TOKENS_FILE must map sha256 hex strings to principal names");
  return { tokens: tokens as Record<string, string> };
}
