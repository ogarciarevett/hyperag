import { Pool, type PoolClient } from "pg";

import { createLocalEmbedder, createLocalReranker, createOpenAICompatibleEmbedder } from "../../../src/adapters/embedders";
import { createNeo4jGraph } from "../../../src/adapters/neo4j";
import { createPostgresStore } from "../../../src/adapters/postgres";
import { Hyperag, type RetrieveOptions } from "../../../src/core";
import type { Embedder, EmbeddingSpace, Reranker } from "../../../src/types";
import { type Conversation, newConversation, sessionEdges } from "../conversation-graph";
import { containerBytes } from "../rss";
import type { SystemFactory } from "../system";
import { benchmarkDatabaseName, ensureBenchmarkDatabase } from "./postgres-db";
import { gpuRequest } from "./gpu-request";

/**
 * EmbeddingGemma 2 (onnx-community/embeddinggemma-2-ONNX, q8, pinned revision): the model returns
 * its pooled, projected `sentence_embedding` (768-d) itself, so it is read directly instead of
 * through the feature-extraction pipeline. Prompts are the model card's search prompts.
 */
function embeddingGemma2(): Embedder {
  const model = "onnx-community/embeddinggemma-2-ONNX";
  const revision = "daa72c51243991dfcaf9f9137d2c573d8f7790c0";
  const query = "task: search result | query: ";
  const document = "title: none | text: ";
  const space: EmbeddingSpace = { model: `${model}#q8`, revision, dimensions: 768, normalized: true, preprocessing: `sentence_embedding; max_length=512; prompts query=${JSON.stringify(query)} document=${JSON.stringify(document)}` };
  type Run = (texts: string[]) => Promise<number[][]>;
  let run: Promise<Run> | undefined;
  const load = () =>
    (run ??= import("@huggingface/transformers").then(async (t) => {
      const tokenizer = (await t.AutoTokenizer.from_pretrained(model, { revision })) as unknown as (x: string[], o: object) => object;
      const m = (await t.AutoModel.from_pretrained(model, { revision, dtype: "q8" })) as unknown as (i: object) => Promise<{ sentence_embedding: { tolist(): number[][] } }>;
      return async (texts: string[]) => (await m(tokenizer(texts, { padding: true, truncation: true, max_length: 512 }))).sentence_embedding.tolist();
    }));
  const embed = async (texts: string[]) => {
    const r = await load();
    const out: number[][] = [];
    for (let i = 0; i < texts.length; i += 8) {
      for (const v of await r(texts.slice(i, i + 8))) {
        const n = Math.hypot(...v) || 1;
        out.push(v.map((x) => x / n));
      }
    }
    return out;
  };
  return {
    embedDocuments: (texts) => embed(texts.map((t) => document + t)),
    embedQuery: async (q) => (await embed([query + q]))[0]!,
    describeModel: () => space,
  };
}

/**
 * NVIDIA Nemotron 3.5 on the shared GPU endpoint (vLLM, OpenAI-compatible): `HYPERAG_GPU_URL` and
 * `HYPERAG_GPU_KEY`. The embedder keeps the first 1,024 of 4,096 dimensions and re-normalises them,
 * as the model card allows (pgvector's HNSW index takes at most 2,000). Requests follow the model
 * cards: embeddings through `/v2/embed` with `input_type` (the checkpoint applies its own query or
 * document prompt), re-ranking through `/classify` with `query` and `document` messages.
 */
// HYPERAG_GPU_EMBED_URL / HYPERAG_GPU_RERANK_URL when the two models sit on their own ports;
// HYPERAG_GPU_EMBED_MODEL / HYPERAG_GPU_RERANK_MODEL when the server's model ids differ from the
// Hugging Face ids (results still record the Hugging Face id and revision).
const gpu = (service: "EMBED" | "RERANK") => {
  const url = process.env[`HYPERAG_GPU_${service}_URL`] ?? process.env.HYPERAG_GPU_URL;
  if (!url) throw new Error(`HYPERAG_GPU_${service}_URL or HYPERAG_GPU_URL is not set`);
  const key = process.env.HYPERAG_GPU_KEY;
  return (path: string, body: unknown) => gpuRequest(url, path, body, key);
};
// Full revisions: the vLLM launch must pass the same ones (--revision), so the recorded one is true.
const NEMOTRON_EMBED = { model: "nvidia/Nemotron-3.5-Embed-8B-BF16", revision: "781e779ee4ca9c2d2a4f38321993556baeab49a1", keep: 1024 };
const NEMOTRON_RERANK = { model: "nvidia/Nemotron-3.5-Rerank-8B-BF16", revision: "f88f8d9104726747791283ab156d2fcdb29e90a7" };
function nemotronEmbedder(build: { model: string; revision: string; label: string } = { model: NEMOTRON_EMBED.model, revision: NEMOTRON_EMBED.revision, label: "" }): Embedder {
  const call = gpu("EMBED");
  const space: EmbeddingSpace = {
    model: `${build.model}${build.label}#first-${NEMOTRON_EMBED.keep}-dims`,
    revision: build.revision,
    dimensions: NEMOTRON_EMBED.keep,
    normalized: true,
    preprocessing: `server /v2/embed, input_type query|document, truncate END; mean pooling; first ${NEMOTRON_EMBED.keep} of 4096 dims, re-normalised`,
  };
  const embed = async (texts: string[], inputType: "query" | "document") => {
    const out: number[][] = [];
    for (let i = 0; i < texts.length; i += 64) {
      const r = (await call("/v2/embed", {
        model: process.env.HYPERAG_GPU_EMBED_MODEL ?? build.model,
        input_type: inputType,
        inputs: texts.slice(i, i + 64).map((text) => ({ content: [{ type: "text", text }] })),
        embedding_types: ["float"],
        truncate: "END",
      })) as { embeddings: { float: number[][] } };
      for (const row of r.embeddings.float) {
        const v = row.slice(0, NEMOTRON_EMBED.keep);
        const n = Math.hypot(...v) || 1;
        out.push(v.map((x) => x / n));
      }
    }
    if (out.length !== texts.length) throw new Error("gpu endpoint: wrong number of vectors");
    return out;
  };
  return {
    embedDocuments: (texts) => embed([...texts], "document"),
    embedQuery: async (q) => (await embed([q], "query"))[0]!,
    describeModel: () => space,
  };
}
function nemotronReranker(): Reranker {
  const call = gpu("RERANK");
  return {
    name: `${NEMOTRON_RERANK.model}@${NEMOTRON_RERANK.revision}`,
    async rerank(query, documents) {
      if (documents.length === 0) return [];
      const scores = new Array<number>(documents.length).fill(Number.NEGATIVE_INFINITY);
      // /classify is the path the model card documents; /v1/rerank only after one pair agrees with it.
      if ((process.env.HYPERAG_GPU_RERANK_API ?? "classify") === "rerank") {
        // vLLM's rerank API: all pairs of one query in one request, one score per document index.
        const r = (await call("/v1/rerank", { model: process.env.HYPERAG_GPU_RERANK_MODEL ?? NEMOTRON_RERANK.model, query, documents })) as { results: Array<{ index: number; relevance_score: number }> };
        for (const x of r.results) scores[x.index] = x.relevance_score;
        return scores;
      }
      // vLLM's classify API: one pair per request (the server applies the model's chat template to
      // the messages), at most HYPERAG_GPU_CONCURRENCY (default 4) in flight.
      const limit = Number(process.env.HYPERAG_GPU_CONCURRENCY ?? 4);
      let next = 0;
      await Promise.all(
        Array.from({ length: Math.min(limit, documents.length) }, async () => {
          while (next < documents.length) {
            const i = next++;
            const r = (await call("/classify", {
              model: process.env.HYPERAG_GPU_RERANK_MODEL ?? NEMOTRON_RERANK.model,
              messages: [
                { role: "query", content: [{ type: "text", text: query }] },
                { role: "document", content: [{ type: "text", text: documents[i]! }] },
              ],
              add_special_tokens: false,
              use_activation: false,
            })) as { data: Array<{ probs?: number[]; logits?: number[] }> };
            const row = r.data[0]!;
            // With use_activation false the card reads probs[0] as the raw logit.
            scores[i] = (row.probs ?? row.logits ?? [Number.NEGATIVE_INFINITY])[0]!;
          }
        }),
      );
      return scores;
    },
  };
}

const PG = process.env.HYPERAG_PG_URL ?? "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag";

/**
 * Profiles: `egemma2` (BM25 + EmbeddingGemma 2 q8 + RRF, the planned default), `nemotron` (BM25 +
 * Nemotron 3.5 Embed 8B on the GPU endpoint, first 1,024 dims), `nemotron-int8` (the community
 * INT8 W8A8 build of it), `bm25`, `bge` (BM25 + bge-small via the fastembed server + RRF: the baseline profile),
 * `e5` (BM25 + multilingual-e5-small + RRF), and any of them with `+rerank` (bge-reranker-base over
 * the fused top 24) `+bgev2m3` (BAAI/bge-reranker-v2-m3, local q8) or `+nvrerank` (Nemotron 3.5 Rerank 8B on the GPU endpoint), and `+graph` for conversation sets only:
 * edges by the exact rules in `conversation-graph.ts` (next turn, same named entity), no model. BEIR
 * documents carry no relations, so on a corpus `+graph` keeps the graph channel on and writes no edges
 * (the default template has the graph on; there is nothing to expand).
 */
// HYPERAG_THREADS caps the local models' CPU threads so a run fits the machine's load limit
const threads = Number(process.env.HYPERAG_THREADS) || undefined;

const factory: SystemFactory = async (profile) => {
  const [base, ...extras] = profile.split("+");
  const embedders: Record<string, () => Embedder | null> = {
    bm25: () => null,
    bge: () =>
      createOpenAICompatibleEmbedder({
        baseUrl: process.env.HYPERAG_FASTEMBED_URL ?? "http://127.0.0.1:58321",
        model: "BAAI/bge-small-en-v1.5",
        revision: "fastembed-0.9.0 Qdrant/bge-small-en-v1.5-onnx-Q@aa8f8b060edb00e03bfdd08813a2949946c8ba55",
        dimensions: 384,
        normalized: true,
      }),
    e5: () => createLocalEmbedder({ threads }),
    egemma2: embeddingGemma2,
    nemotron: () => nemotronEmbedder(),
    // A community INT8 (W8A8) quantisation of the same model, not an NVIDIA build; OpenMDW-1.1.
    "nemotron-int8": () => nemotronEmbedder({ model: "kmouratidis/Nemotron-3.5-Embed-8B-INT8-W8A8", revision: "afc56ee78934e0d911995f588c8f9d11972e5e22", label: " (community INT8 W8A8)" }),
  };
  if (!base || !embedders[base]) throw new Error(`unknown hyperag profile ${profile}`);
  const rerank = extras.some((x) => ["rerank", "nvrerank", "bgev2m3"].includes(x));
  const embedder = embedders[base]!();
  const reranker = extras.includes("nvrerank")
    ? nemotronReranker()
    : extras.includes("bgev2m3")
      ? // The owner's re-ranker: BAAI/bge-reranker-v2-m3 (568M, multilingual), ONNX q8, pinned.
        createLocalReranker({ model: "onnx-community/bge-reranker-v2-m3-ONNX", revision: "6f5ff65298512715a1e669753bc754d2bc8f367b", dtype: "q8" })
      : rerank
        ? createLocalReranker({ threads })
        : null;
  // One Neo4j for every database: memory ids are the same in each, and the rules depend on the text only.
  const graph = extras.includes("graph")
    ? createNeo4jGraph({
        baseUrl: process.env.HYPERAG_NEO4J_URL ?? "http://127.0.0.1:57474",
        username: process.env.HYPERAG_NEO4J_USER ?? "neo4j",
        password: process.env.HYPERAG_NEO4J_PASSWORD ?? "fake-test-only",
        timeoutMs: 30_000,
      })
    : null;
  const conversations = new Map<string, Conversation>();
  const pools = new Map<string, Promise<{ pool: Pool; h: Hyperag }>>();
  const granted = new Set<string>();
  // A namespace `<set>/<item>` (one conversation, one LongMemEval question) is one scope in the set's
  // database, read by its own principal only; a plain namespace (a BEIR corpus) is scope `corpus`.
  const split = (ns: string) => {
    const at = ns.indexOf("/");
    return at < 0 ? { group: ns, scope: "corpus" } : { group: ns.slice(0, at), scope: ns };
  };
  const reader = (scope: string) => (scope === "corpus" ? "bench" : `reader:${scope}`);
  const database = (group: string) => {
    const found = pools.get(group);
    if (found) return found;
    const made = connect(group);
    pools.set(group, made);
    return made;
  };
  const opened = async (ns: string) => {
    const { group, scope } = split(ns);
    const entry = await database(group);
    if (!granted.has(scope)) {
      await entry.h.grant("bench", "bench", scope, "write");
      await entry.h.grant("bench", reader(scope), scope, "read");
      // relate() reads both ends, so the writer also needs read on a graph profile.
      if (graph) await entry.h.grant("bench", "bench", scope, "read");
      granted.add(scope);
    }
    return { ...entry, scope };
  };
  const connect = async (group: string) => {
    // One database per set and embedding family, reused across runs (ingest is idempotent).
    const db = benchmarkDatabaseName(group, base);
    const admin = new Pool({ connectionString: PG, max: 1 });
    let client: PoolClient | undefined;
    let pool: Pool | undefined;
    try {
      client = await admin.connect();
      // IF NOT EXISTS still races on extension and table creation across adapter instances.
      await client.query("select pg_advisory_lock(hashtextextended($1, 0))", [`hyperag-public-init:${db}`]);
      await ensureBenchmarkDatabase(client, db);
      pool = new Pool({ connectionString: PG.replace(/\/[^/]+$/u, `/${db}`), max: 4 });
      const h = new Hyperag({ store: createPostgresStore(pool), embedder, reranker, graph, admins: ["bench"] });
      await h.init();
      return { pool, h };
    } catch (error) {
      await pool?.end();
      throw error;
    } finally {
      client?.release();
      // Closing this dedicated admin session releases its initialization lock.
      await admin.end();
    }
  };
  const options = (k: number): RetrieveOptions => ({
    limit: k,
    // Each channel reads at least as deep as the scored depth.
    candidatesPerChannel: Math.max(24, k),
    channels: { bm25: true, vector: base !== "bm25", graph: graph !== null, rerank },
  });
  return {
    // the store runs in its own container, counted as the competitors' containers are
    rssBytes: async () => containerBytes([process.env.HYPERAG_PG_CONTAINER ?? "hyperag-postgres-1"]),
    describe: async () => ({
      system: "hyperag",
      version: "0.1.0",
      profile,
      config: { embedding: embedder?.describeModel() ?? null, reranker: reranker?.name ?? null, rrfK: 60, graph: graph ? "conversations: NEXT_TURN, SAME_ENTITY (exact match), 2 hops; BEIR: graph enabled, no edges or relationship contribution" : null,
        // Benchmark ingestion leaves validFrom and validTo null, so asOf would not hide memories.
        // The anchor-only rule leaves validity filters unused: they do not anchor relative dates.
        asOf: "none (asOf filters validity, not relative-date anchoring; validFrom and validTo are null)",
        ...((base.startsWith("nemotron") || extras.includes("nvrerank")) ? { gpuRequestDeadline: process.env.GPU_RETRY_PROXY === "1" ? "No extra adapter wall-clock deadline through the shared proxy; window budget and VM cutoff remain" : "120 seconds" } : {}) },
    }),
    open: async (ns) => void (await opened(ns)),
    ingest: async (ns, docs) => {
      const { pool, h, scope } = await opened(ns);
      const ids = docs.map((d) => `${scope}/${d.id.replaceAll("/", "%2F")}`);
      // Resumable: documents already stored by an earlier run are not embedded again.
      const have = new Set((await pool.query<{ id: string }>("select id from memories where id = any($1)", [ids])).rows.map((r) => r.id));
      const missing = docs.filter((_, i) => !have.has(ids[i]!));
      await h.ingestMany(
        "bench",
        missing.map((d) => ({
          scope,
          id: d.id.replaceAll("/", "%2F"),
          source: d.speaker ?? "corpus",
          // memory.ts already writes date and speaker into a turn's text, the same text every system gets
          text: d.speaker ? d.text : `${d.title ? `${d.title}\n` : ""}${d.text}`,
          ...(d.timestamp ? { metadata: { timestamp: d.timestamp } } : {}),
        })),
      );
      if (graph && scope !== "corpus") {
        // All turns, stored before or not: MERGE makes a replayed edge a no-op, and a resumed run rebuilds the state.
        const c = conversations.get(ns) ?? newConversation();
        conversations.set(ns, c);
        const local = (id: string) => `${scope}/${id.replaceAll("/", "%2F")}`;
        for (const e of sessionEdges(c, docs)) await h.relate("bench", { from: local(e.from), to: local(e.to), type: e.type });
      }
    },
    search: async (ns, query, k) => {
      const { h, scope } = await opened(ns);
      const r = await h.retrieve(reader(scope), query, options(k));
      // retrieve() degrades a failed graph channel to a limitation; under +graph that must be an error.
      const failed = r.limitations.find((l) => l.startsWith("graph channel failed"));
      if (failed) throw new Error(failed);
      return r.items.map((i, rank) => ({
        id: decodeURIComponent(i.memory.id.slice(scope.length + 1)),
        score: i.rerankScore ?? i.score ?? 1 / (rank + 1),
        text: i.memory.text,
      }));
    },
    close: async () => {
      for (const entry of pools.values()) await (await entry).pool.end();
    },
  };
};
export default factory;
