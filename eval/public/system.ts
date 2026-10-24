/**
 * THE ONE INTERFACE every system under test implements, in one file under `systems/`.
 * A namespace isolates one benchmark collection (BEIR corpus) or one conversation (memory sets).
 */
export type Doc = Readonly<{ id: string; text: string; title?: string; timestamp?: string; speaker?: string }>;
export type Hit = Readonly<{ id: string; score?: number; text?: string }>;

export type MemorySystem = {
  /** Name and version as recorded in every result file. */
  describe(): Promise<Readonly<{ system: string; version: string; profile: string; config: Record<string, unknown> }>>;
  /** Called once before ingesting a namespace; may be a no-op. */
  open(namespace: string): Promise<void>;
  ingest(namespace: string, docs: readonly Doc[]): Promise<void>;
  /** Best first. `k` is the depth the benchmark scores (BEIR: 100). */
  /** `asOf` (ISO date) is the question's own date, the anchor for relative dates ("last week"); a
   * system uses it only through a native search parameter and says so in describe().config.asOf. */
  search(namespace: string, query: string, k: number, opts?: Readonly<{ asOf?: string }>): Promise<readonly Hit[]>;
  close(): Promise<void>;
  /** RAM held by the system outside this process (servers, containers), sampled by the runners. */
  rssBytes?(): Promise<number>;
};

/** `systems/<name>.ts` default-exports a factory taking the profile string (`--profile`). */
export type SystemFactory = (profile: string) => Promise<MemorySystem>;
