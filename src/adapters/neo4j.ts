import type { GraphStore, Relation } from "../types.js";

export type Neo4jConfig = Readonly<{
  /** The HTTP listener, local (`http://127.0.0.1:57474`) or remote; the Query API v2 path is added. */
  baseUrl: string;
  database?: string;
  username: string;
  password: string;
  timeoutMs?: number;
  fetch?: (request: Request) => Promise<Response>;
}>;

type Run = (statement: string, parameters: Record<string, unknown>) => Promise<unknown[][]>;

/**
 * One statement through Neo4j's Query API over HTTP, so no driver is needed. From the original
 * agents-runtime graph store. Any failure is `graph_unavailable`, and the caller degrades.
 */
export function createNeo4jQueryRun(config: Neo4jConfig): Run {
  const url = `${config.baseUrl.replace(/\/+$/u, "")}/db/${config.database ?? "neo4j"}/query/v2`;
  const authorization = `Basic ${Buffer.from(`${config.username}:${config.password}`).toString("base64")}`;
  const doFetch = config.fetch ?? fetch;
  return async (statement, parameters) => {
    const response = await doFetch(
      new Request(url, {
        method: "POST",
        headers: { authorization, "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify({ statement, parameters }),
        signal: AbortSignal.timeout(config.timeoutMs ?? 5_000),
      }),
    ).catch(() => unavailable("unreachable"));
    const body = (await response.json().catch(() => null)) as {
      data?: { values?: unknown[][] };
      errors?: Array<{ code?: string }>;
    } | null;
    if ((response.status !== 200 && response.status !== 202) || !Array.isArray(body?.data?.values))
      return unavailable(body?.errors?.[0]?.code ?? `http_${response.status}`);
    return body.data.values;
  };
}

/**
 * THE RELATIONSHIP GRAPH IN NEO4J. Nodes are memory ids with their scope; edges are typed and carry
 * who made them. No text is stored here. Every read passes the caller's readable scopes and keeps
 * only paths whose every node is readable, so a revoked scope also closes every path through it.
 */
export function createNeo4jGraph(config: Neo4jConfig): GraphStore {
  const run = createNeo4jQueryRun(config);
  return {
    async ensureSchema() {
      await run("CREATE CONSTRAINT hyperag_memory_id IF NOT EXISTS FOR (m:Memory) REQUIRE m.id IS UNIQUE", {});
    },
    async upsertMemory(id, scope) {
      await run("MERGE (m:Memory {id: $id}) SET m.scope = $scope", { id, scope });
    },
    async relate(rel, scopes) {
      // MERGE on (from, to, type): relating twice is a no-op, so the write is idempotent.
      await run(
        `MERGE (a:Memory {id: $from}) SET a.scope = $fromScope
         MERGE (b:Memory {id: $to}) SET b.scope = $toScope
         MERGE (a)-[r:REL {type: $type}]->(b)
         ON CREATE SET r.createdBy = $createdBy, r.createdAt = $createdAt`,
        { ...rel, fromScope: scopes.from, toScope: scopes.to },
      );
    },
    async expand(seeds, readable, hops, limit) {
      if (seeds.length === 0 || readable.length === 0) return [];
      const depth = Math.max(1, Math.min(3, Math.trunc(hops)));
      const rows = await run(
        `MATCH p = (s:Memory)-[:REL*1..${depth}]-(n:Memory)
         WHERE s.id IN $seeds AND NOT n.id IN $seeds AND all(x IN nodes(p) WHERE x.scope IN $readable)
         WITH n, count(p) AS paths, collect(DISTINCT s.id) AS via
         RETURN n.id, paths, via[0..5] ORDER BY paths DESC, n.id LIMIT $limit`,
        { seeds, readable, limit },
      );
      return rows.map(([id, paths, via]) => ({
        id: String(id),
        paths: Number(paths),
        via: (via as unknown[]).map(String),
      }));
    },
    async relations(id, readable) {
      const rows = await run(
        `MATCH (a:Memory {id: $id})-[r:REL]-(b:Memory)
         WHERE a.scope IN $readable AND b.scope IN $readable
         RETURN startNode(r).id, endNode(r).id, r.type, r.createdBy, r.createdAt
         ORDER BY r.createdAt, startNode(r).id, endNode(r).id`,
        { id, readable },
      );
      return rows.map(
        ([from, to, type, createdBy, createdAt]): Relation => ({
          from: String(from),
          to: String(to),
          type: String(type),
          createdBy: String(createdBy),
          createdAt: String(createdAt),
        }),
      );
    },
    async deleteMemory(id) {
      await run("MATCH (m:Memory {id: $id}) DETACH DELETE m", { id });
    },
  };
}

function unavailable(reason: string): never {
  throw new Error(`graph_unavailable: ${reason}`);
}
