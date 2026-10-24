/** The memory record a caller sees. One stable id, many versions; only the current one ranks. */
export type Memory = Readonly<{
  id: string;
  scope: string;
  kind: string;
  version: number;
  text: string;
  source: string;
  author: string;
  validFrom: string | null;
  validTo: string | null;
  recordedAt: string;
  status: "active" | "superseded" | "invalidated";
  statusReason: string | null;
  supersededBy: string | null;
  metadata: Readonly<Record<string, unknown>>;
}>;

export type NewMemory = Readonly<{
  scope: string;
  text: string;
  source: string;
  kind?: string;
  /** Stable identity; omitted, it is derived from scope, source and text, so a retry is a no-op. */
  id?: string;
  validFrom?: string;
  validTo?: string;
  metadata?: Record<string, unknown>;
  /** Memories this one corrects: each one's current version becomes `superseded`. */
  supersedes?: readonly string[];
  idempotencyKey?: string;
}>;

export type MemoryUpdate = Readonly<{
  text: string;
  source: string;
  /** The version the writer read. Another writer got there first: `version_conflict`. */
  expectedVersion: number;
  validFrom?: string;
  validTo?: string;
  metadata?: Record<string, unknown>;
  idempotencyKey?: string;
}>;

export type WriteResult = Readonly<{ id: string; version: number; created: boolean }>;

/** What a store returns per channel: ids and versions in that channel's best-first order. */
/** `at` is the version's recorded time (epoch seconds), used to break ties newest first. */
export type Hit = Readonly<{ id: string; version: number; score: number; at: number }>;

/** Provider-independent embeddings. The space describes everything that makes vectors comparable. */
export type EmbeddingSpace = Readonly<{
  model: string;
  revision: string;
  dimensions: number;
  normalized: boolean;
  /** Text preprocessing and query/document mode, for example `e5 prefixes "query: "/"passage: "`. */
  preprocessing: string;
}>;

export type Embedder = {
  embedDocuments(texts: readonly string[]): Promise<number[][]>;
  embedQuery(question: string): Promise<number[]>;
  describeModel(): EmbeddingSpace;
};

/** A cross-encoder or any scorer: one score per document, in order, higher is more relevant. */
export type Reranker = {
  rerank(query: string, documents: readonly string[]): Promise<number[]>;
  name: string;
};

export type Relation = Readonly<{
  from: string;
  to: string;
  type: string;
  createdBy: string;
  createdAt: string;
}>;

/** A graph store holds memory ids and typed edges only; it never holds text. */
export type GraphStore = {
  ensureSchema(): Promise<void>;
  upsertMemory(id: string, scope: string): Promise<void>;
  relate(rel: Relation, scopes: { from: string; to: string }): Promise<void>;
  /** Memories within `hops` of the seeds whose every node on the path is in a readable scope. */
  expand(
    seeds: readonly string[],
    readable: readonly string[],
    hops: number,
    limit: number,
  ): Promise<Array<{ id: string; paths: number; via: string[] }>>;
  relations(id: string, readable: readonly string[]): Promise<Relation[]>;
  deleteMemory(id: string): Promise<void>;
};

export class HyperagError extends Error {
  constructor(
    readonly code:
      | "forbidden"
      | "not_found"
      | "version_conflict"
      | "already_exists"
      | "invalid_input"
      | "not_active"
      | "embedding_space_mismatch",
    message: string,
    readonly detail?: Record<string, unknown>,
  ) {
    super(message);
  }
}
