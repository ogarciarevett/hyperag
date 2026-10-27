import { createHash } from "node:crypto";
import { identityHash, type PostgresStore, versionHash } from "./adapters/postgres.js";
import { spaceId } from "./adapters/postgres.js";
import { buildContext, type Context } from "./context.js";
import { fuse } from "./fusion.js";
import {
  type Embedder,
  type GraphStore,
  type Hit,
  HyperagError,
  type Memory,
  type MemoryUpdate,
  type NewMemory,
  type Relation,
  type Reranker,
  type WriteResult,
} from "./types.js";

export type RetrieveOptions = Readonly<{
  limit?: number;
  candidatesPerChannel?: number;
  /** Only facts valid at this instant (valid time), default: no valid-time filter. */
  asOf?: string;
  /** Turn single channels off to evaluate components one at a time. */
  channels?: Partial<Record<"bm25" | "vector" | "graph" | "rerank", boolean>>;
}>;

export type RetrievedItem = Readonly<{
  memory: Memory;
  score: number;
  /** Rank (1-based) of this memory in each channel that found it. */
  channels: Readonly<Partial<Record<"bm25" | "vector" | "graph", number>>>;
  /** For a graph hit: the seed memories it was reached from. */
  via?: readonly string[];
  rerankScore?: number;
}>;

export type Retrieval = Readonly<{
  items: readonly RetrievedItem[];
  /** True when no item clears the evidence bar: the caller should say it does not know. */
  insufficientEvidence: boolean;
  pathsUsed: readonly string[];
  limitations: readonly string[];
  embeddingSpace: string | null;
}>;

export type HyperagConfig = Readonly<{
  store: PostgresStore;
  embedder?: Embedder | null;
  graph?: GraphStore | null;
  reranker?: Reranker | null;
  rrfK?: number;
  /** Least cross-encoder score for evidence; below it the answer is `insufficientEvidence`. */
  minRerankScore?: number;
  /** Least best cosine similarity for evidence when no re-ranker runs (default 0.82, dev-set probe). */
  minVectorScore?: number;
  /** Principals allowed to grant and revoke. */
  admins?: readonly string[];
  /** Bounded LRU size for cross-encoder scores; default false (0, off). */
  rerankCache?: boolean | number | string;
}>;

export const DEFAULT_RERANK_CACHE_SIZE = 256;
export const MAX_RERANK_CACHE_BYTES = 8 * 1024 * 1024;

export function rerankCacheKey(
  modelId: string,
  query: string,
  candidates: readonly (readonly [string, number])[],
): string {
  return createHash("sha256").update(JSON.stringify([modelId, query, candidates])).digest("hex");
}

export function parseRerankCache(value?: boolean | number | string | null): number {
  if (value === undefined || value === null || value === false || value === 0) return 0;
  if (value === true) return DEFAULT_RERANK_CACHE_SIZE;
  if (typeof value === "number") {
    if (Number.isInteger(value) && value >= 1 && value <= 65_536) return value;
    throw new HyperagError("invalid_input", "HYPERAG_RERANK_CACHE must be false, true or an integer from 0 to 65536");
  }
  const trimmed = value.trim();
  if (trimmed === "" || ["0", "false", "off", "none"].includes(trimmed)) return 0;
  if (["true", "on"].includes(trimmed)) return DEFAULT_RERANK_CACHE_SIZE;
  const parsed = Number(trimmed);
  if (/^\d+$/u.test(trimmed) && Number.isInteger(parsed) && parsed >= 1 && parsed <= 65_536) return parsed;
  throw new HyperagError("invalid_input", "HYPERAG_RERANK_CACHE must be false, true or an integer from 0 to 65536");
}

export class RerankCache {
  readonly capacity: number;
  private readonly entries = new Map<string, { candidates: readonly (readonly [string, number])[]; scores: readonly number[]; bytes: number }>();
  constructor(capacity: boolean | number | string = 0) { this.capacity = parseRerankCache(capacity); }
  get size(): number { return this.entries.size; }
  get retainedBytes(): number {
    let bytes = 0;
    for (const entry of this.entries.values()) bytes += entry.bytes;
    return bytes;
  }

  invalidate(id: string, currentVersion?: number): void {
    if (this.capacity === 0) return;
    for (const [k, entry] of this.entries)
      if (entry.candidates.some(([cid, ver]) => cid === id && (currentVersion === undefined || ver !== currentVersion)))
        this.entries.delete(k);
  }

  get(modelId: string, query: string, candidates: readonly (readonly [string, number])[]): number[] | undefined {
    if (this.capacity === 0) return undefined;
    for (const [id, ver] of candidates) this.invalidate(id, ver);
    const key = rerankCacheKey(modelId, query, candidates);
    const hit = this.entries.get(key);
    if (hit === undefined) return undefined;
    this.entries.delete(key);
    this.entries.set(key, hit);
    return [...hit.scores];
  }

  put(modelId: string, query: string, candidates: readonly (readonly [string, number])[], scores: readonly number[]): void {
    if (this.capacity === 0) return;
    // Account for UTF-16 IDs, tuple/array slots, score doubles, and the fixed digest/entry.
    let bytes = 256 + scores.length * 8 + candidates.length * 64;
    for (const [id] of candidates) bytes += id.length * 2;
    if (bytes > MAX_RERANK_CACHE_BYTES) return;
    for (const [id, ver] of candidates) this.invalidate(id, ver);
    const key = rerankCacheKey(modelId, query, candidates);
    this.entries.delete(key);
    let retained = this.retainedBytes;
    while (this.entries.size >= this.capacity || retained + bytes > MAX_RERANK_CACHE_BYTES) {
      const oldest = this.entries.keys().next().value;
      if (oldest === undefined) break;
      retained -= this.entries.get(oldest)!.bytes;
      this.entries.delete(oldest);
    }
    this.entries.set(key, { candidates: candidates.map(([id, v]) => [id, v] as const), scores: [...scores], bytes });
  }
}

/**
 * THE HYPERAG CORE. Every call names the acting principal. Inside one process the principal is
 * trusted as given; the HTTP service (`server.ts`) is the trust boundary that verifies it. Scopes a
 * principal may read or write always come from the grants table, never from the caller.
 */
export class Hyperag {
  private space: string | null = null;
  readonly rerankCache: RerankCache;

  constructor(private readonly config: HyperagConfig) {
    this.rerankCache = new RerankCache(config.rerankCache);
  }

  /** Applies the schema, registers the embedding space and the graph constraint. Idempotent. */
  async init(): Promise<void> {
    await this.config.store.migrate();
    if (this.config.embedder) this.space = await this.config.store.ensureSpace(this.config.embedder.describeModel());
    await this.config.graph?.ensureSchema();
  }

  /** Grants and revocations need an admin named in `config.admins`; each change is logged. */
  async grant(admin: string, principal: string, scope: string, permission: "read" | "write") {
    this.mustAdmin(admin);
    await this.config.store.grant(principal, scope, permission, admin);
  }

  async revoke(admin: string, principal: string, scope: string, permission: "read" | "write") {
    this.mustAdmin(admin);
    await this.config.store.revoke(principal, scope, permission, admin);
  }

  async scopes(principal: string) {
    const [read, write] = await Promise.all([
      this.config.store.scopes(principal, "read"),
      this.config.store.scopes(principal, "write"),
    ]);
    return { read, write };
  }

  async ingest(principal: string, input: NewMemory): Promise<WriteResult> {
    validNew(input);
    await this.mustWrite(principal, input.scope);
    return this.ingestWith(principal, input, (await this.vectorsOf([input.text]))?.[0] ?? null);
  }

  /** Many memories with one embedding call per batch; same checks and result as `ingest` each. */
  async ingestMany(principal: string, inputs: readonly NewMemory[], batch = 32): Promise<WriteResult[]> {
    inputs.forEach(validNew);
    for (const scope of new Set(inputs.map((i) => i.scope))) await this.mustWrite(principal, scope);
    const results: WriteResult[] = [];
    for (let i = 0; i < inputs.length; i += batch) {
      const chunk = inputs.slice(i, i + batch);
      const vectors = await this.vectorsOf(chunk.map((c) => c.text));
      for (const [j, input] of chunk.entries()) results.push(await this.ingestWith(principal, input, vectors?.[j] ?? null));
    }
    return results;
  }

  private async ingestWith(
    principal: string,
    input: NewMemory,
    vector: { space: string; values: number[] } | null,
  ): Promise<WriteResult> {
    const supersedes = input.supersedes ?? [];
    const oldScopes = new Map<string, string>();
    for (const old of supersedes) oldScopes.set(old, await this.writableScopeOf(principal, old));
    // An explicit id is namespaced by its scope, so a colliding id never reveals another scope's memory.
    const id = input.id === undefined ? `m_${identityHash(input.scope, input.source, input.text)}` : `${input.scope}/${input.id}`;
    const result = await this.config.store.create(
      principal,
      { ...input, id, kind: input.kind ?? "document", supersedes },
      versionHash(input.scope, input),
      vector,
    );
    for (const old of supersedes) this.rerankCache.invalidate(old);
    // Graph writes are MERGEs, so they also run on a replay: a retry repairs a graph write that failed.
    // A replay of a memory deleted since then writes nothing.
    const live = result.created || (await this.config.store.scopeOf([result.id])).has(result.id);
    if (this.config.graph && live) {
      await this.config.graph.upsertMemory(result.id, input.scope);
      for (const old of supersedes)
        await this.config.graph.relate(
          { from: result.id, to: old, type: "SUPERSEDES", createdBy: principal, createdAt: new Date().toISOString() },
          { from: input.scope, to: oldScopes.get(old) as string },
        );
    }
    return result;
  }

  async update(principal: string, id: string, input: MemoryUpdate): Promise<WriteResult> {
    validDates(input);
    const scope = await this.writableScopeOf(principal, id);
    const vector = (await this.vectorsOf([input.text]))?.[0] ?? null;
    const result = await this.config.store.update(principal, id, input, versionHash(scope, input), vector);
    this.rerankCache.invalidate(id, result.version);
    return result;
  }

  /** The fact stops ranking and its validity ends now; its history stays explainable. */
  async invalidate(principal: string, id: string, reason: string): Promise<void> {
    await this.writableScopeOf(principal, id);
    await this.config.store.invalidate(id, reason, principal);
    this.rerankCache.invalidate(id);
  }

  /**
   * Text, vectors and graph node go; a content-free tombstone stays. Deleting an already deleted
   * memory repeats only the graph step, so a retry finishes a graph delete that failed. Memories
   * this one had corrected stay superseded.
   */
  async delete(principal: string, id: string, reason: string): Promise<void> {
    const tomb = await this.config.store.history(id);
    if (tomb?.deleted && (await this.config.store.scopes(principal, "write")).includes(tomb.scope)) {
      await this.config.graph?.deleteMemory(id);
      return;
    }
    await this.writableScopeOf(principal, id);
    await this.config.store.delete(id, reason, principal);
    this.rerankCache.invalidate(id);
    await this.config.graph?.deleteMemory(id);
  }

  /** A typed edge between two memories: write on the source's scope, read on the target's. */
  async relate(principal: string, rel: Readonly<{ from: string; to: string; type: string }>): Promise<void> {
    const graph = this.requireGraph();
    if (!/^[A-Z][A-Z0-9_]{0,63}$/u.test(rel.type))
      throw new HyperagError("invalid_input", "relation type must be UPPER_SNAKE_CASE");
    const from = await this.writableScopeOf(principal, rel.from);
    const to = await this.readableScopeOf(principal, rel.to);
    await graph.relate(
      { ...rel, createdBy: principal, createdAt: new Date().toISOString() } satisfies Relation,
      { from, to },
    );
  }

  /** Edges whose both ends are readable and not deleted in Postgres, whatever the graph still holds. */
  async relations(principal: string, id: string): Promise<Relation[]> {
    const readable = await this.config.store.scopes(principal, "read");
    const rels = await this.requireGraph().relations(id, readable);
    const live = await this.config.store.scopeOf(rels.flatMap((r) => [r.from, r.to]));
    return rels.filter((r) => readable.includes(live.get(r.from) ?? "") && readable.includes(live.get(r.to) ?? ""));
  }

  async retrieve(principal: string, query: string, options: RetrieveOptions = {}): Promise<Retrieval> {
    const readable = await this.config.store.scopes(principal, "read");
    const limit = options.limit ?? 8;
    const candidates = options.candidatesPerChannel ?? 24;
    const asOf = options.asOf ?? null;
    const on = { bm25: true, vector: true, graph: true, rerank: true, ...options.channels };
    const limitations: string[] = [];
    const paths: string[] = [];
    const lists: Record<string, Hit[]> = {};

    if (on.bm25) {
      lists.bm25 = await this.config.store.bm25(readable, query, candidates, asOf);
      paths.push("bm25");
    }
    if (on.vector) {
      if (this.config.embedder && this.space) {
        try {
          const q = await this.config.embedder.embedQuery(query);
          lists.vector = await this.config.store.vector(this.space, readable, q, candidates, asOf);
          paths.push("vector");
        } catch (error) {
          limitations.push(`vector channel failed: ${String(error)}`);
        }
      } else limitations.push("no embedder configured: lexical retrieval only");
    }
    const seeds = fuse(Object.values(lists), this.config.rrfK ?? 60).slice(0, 8).map((h) => h.id);
    const via = new Map<string, string[]>();
    if (on.graph) {
      if (this.config.graph) {
        try {
          const expanded = await this.config.graph.expand(seeds, readable, 2, candidates);
          lists.graph = expanded.map((e) => ({ id: e.id, version: 0, score: e.paths, at: 0 }));
          for (const e of expanded) via.set(e.id, e.via);
          paths.push("graph");
        } catch (error) {
          limitations.push(`graph channel failed: ${String(error)}`);
        }
      } else limitations.push("no graph configured: no relationship expansion");
    }

    const fused = fuse(Object.values(lists), this.config.rrfK ?? 60);
    const pool = on.rerank && this.config.reranker ? Math.max(limit, candidates) : limit;
    // Graph hits and stale candidates are re-checked against the store with the same filters.
    const memories = await this.config.store.load(
      fused.slice(0, pool).map((h) => h.id),
      readable,
      asOf,
    );
    const ranksOf = (id: string) => {
      const ranks: Partial<Record<"bm25" | "vector" | "graph", number>> = {};
      for (const [name, list] of Object.entries(lists)) {
        const at = list.findIndex((h) => h.id === id);
        if (at >= 0) ranks[name as "bm25"] = at + 1;
      }
      return ranks;
    };
    const fusedScore = new Map(fused.map((h) => [h.id, h.score]));
    let items: RetrievedItem[] = memories.map((memory) => ({
      memory,
      score: fusedScore.get(memory.id) ?? 0,
      channels: ranksOf(memory.id),
      ...(via.has(memory.id) ? { via: via.get(memory.id) } : {}),
    }));

    let reranked = false;
    if (on.rerank && this.config.reranker && items.length > 0) {
      try {
        const candidates = items.map((i) => [i.memory.id, i.memory.version] as const);
        let scores = this.rerankCache.get(this.config.reranker.name, query, candidates);
        if (scores === undefined) {
          scores = await this.config.reranker.rerank(query, items.map((i) => i.memory.text));
          this.rerankCache.put(this.config.reranker.name, query, candidates, scores);
        }
        items = items
          .map((item, index) => ({ ...item, rerankScore: scores[index] ?? Number.NEGATIVE_INFINITY }))
          .sort((a, b) => (b.rerankScore ?? 0) - (a.rerankScore ?? 0));
        reranked = true;
        paths.push(`rerank:${this.config.reranker.name}`);
      } catch (error) {
        limitations.push(`re-ranker failed, fused order kept: ${String(error)}`);
      }
    }
    items = items.slice(0, limit);

    const insufficientEvidence = !this.hasEvidence(items, lists, reranked);
    if (insufficientEvidence) limitations.push("insufficient evidence: no result clears the evidence threshold");
    if (readable.length === 0) limitations.push("principal has no readable scope");
    return { items, insufficientEvidence, pathsUsed: paths, limitations, embeddingSpace: this.space };
  }

  /** Retrieval packed into a token budget, each passage numbered and cited. */
  async context(
    principal: string,
    query: string,
    options: RetrieveOptions & Readonly<{ maxTokens?: number }> = {},
  ): Promise<Context & { retrieval: Retrieval }> {
    const retrieval = await this.retrieve(principal, query, options);
    return { ...buildContext(retrieval, options.maxTokens ?? 1_000), retrieval };
  }

  /** Where a memory came from: every version, who wrote it, what corrected it, its relations. */
  async explain(principal: string, id: string) {
    const readable = await this.config.store.scopes(principal, "read");
    const history = await this.config.store.history(id);
    if (history === null || !readable.includes(history.scope))
      throw new HyperagError("not_found", `memory ${id} not found`);
    const relations = this.config.graph && history.deleted === null
      ? await this.relations(principal, id).catch(() => null)
      : [];
    // Ids of memories the reader cannot read are hidden, so explain is no cross-scope oracle.
    const linked = await this.config.store.scopeOf([
      ...history.corrects,
      ...history.versions.flatMap((v) => (v.supersededBy === null ? [] : [v.supersededBy])),
    ]);
    const visible = (other: string) => readable.includes(linked.get(other) ?? "");
    return {
      ...history,
      corrects: history.corrects.filter(visible),
      versions: history.versions.map((v) =>
        v.supersededBy === null || visible(v.supersededBy) ? v : { ...v, supersededBy: "[not readable]" },
      ),
      relations,
    };
  }

  /**
   * Embeds every current memory into `embedder`'s space, in its own vector table. Source text,
   * BM25 and the graph are untouched; switch by constructing Hyperag with the new embedder.
   */
  async reindex(embedder: Embedder, batch = 64): Promise<{ space: string; embedded: number }> {
    const space = await this.config.store.ensureSpace(embedder.describeModel());
    let embedded = 0;
    for (;;) {
      const rows = await this.config.store.missingVectors(space, batch);
      if (rows.length === 0) return { space, embedded };
      const vectors = await embedder.embedDocuments(rows.map((r) => r.text));
      await this.config.store.putVectors(
        space,
        rows.map((r, i) => ({ id: r.id, version: r.version, values: vectors[i] as number[] })),
      );
      embedded += rows.length;
    }
  }

  private hasEvidence(items: readonly RetrievedItem[], lists: Record<string, Hit[]>, reranked: boolean): boolean {
    const top = items[0];
    if (top === undefined) return false;
    if (reranked) return (top.rerankScore ?? Number.NEGATIVE_INFINITY) >= (this.config.minRerankScore ?? 0);
    // Lexical-only retrieval cannot abstain: any shared word is a match.
    if (lists.vector === undefined) return true;
    const best = Math.max(0, ...lists.vector.map((h) => h.score));
    return best >= (this.config.minVectorScore ?? 0.82);
  }

  private async vectorsOf(texts: string[]) {
    if (!this.config.embedder || !this.space) return null;
    if (spaceId(this.config.embedder.describeModel()) !== this.space)
      throw new HyperagError("embedding_space_mismatch", "embedder changed after init");
    const space = this.space;
    return (await this.config.embedder.embedDocuments(texts)).map((values) => ({ space, values }));
  }

  private mustAdmin(actor: string) {
    if (!(this.config.admins ?? []).includes(actor))
      throw new HyperagError("forbidden", "only an admin manages grants");
  }

  /** Missing and unreadable are the same `not_found`, so a write never confirms another scope's id. */
  private async readableScopeOf(principal: string, id: string): Promise<string> {
    const scope = (await this.config.store.scopeOf([id])).get(id);
    if (scope === undefined || !(await this.config.store.scopes(principal, "read")).includes(scope))
      throw new HyperagError("not_found", `memory ${id} not found`);
    return scope;
  }

  private async writableScopeOf(principal: string, id: string): Promise<string> {
    const scope = await this.readableScopeOf(principal, id);
    await this.mustWrite(principal, scope);
    return scope;
  }

  private async mustWrite(principal: string, scope: string) {
    if (!(await this.config.store.scopes(principal, "write")).includes(scope))
      throw new HyperagError("forbidden", `no write access to scope ${scope}`);
  }

  private requireGraph(): GraphStore {
    if (!this.config.graph) throw new HyperagError("invalid_input", "no graph store configured");
    return this.config.graph;
  }
}

function validDates(input: Readonly<{ validFrom?: string; validTo?: string }>) {
  for (const t of [input.validFrom, input.validTo])
    if (t !== undefined && !Number.isFinite(Date.parse(t)))
      throw new HyperagError("invalid_input", `not a date: ${t}`);
}

function validNew(input: NewMemory) {
  if (input.text.trim() === "") throw new HyperagError("invalid_input", "text is empty");
  if (input.id !== undefined && (input.id === "" || input.id.includes("/")))
    throw new HyperagError("invalid_input", "an explicit id is non-empty and has no '/'");
  validDates(input);
}
