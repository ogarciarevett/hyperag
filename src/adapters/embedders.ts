import type { Embedder, EmbeddingSpace, Reranker } from "../types.js";

/**
 * LOCAL EMBEDDINGS through transformers.js (ONNX Runtime, CPU). No network call after the model is
 * downloaded once from the Hugging Face hub into the local cache. The default is
 * multilingual-e5-small (English and Spanish), which expects `query: ` and `passage: ` prefixes.
 */
export function createLocalEmbedder(
  options: Readonly<{
    model?: string;
    revision?: string;
    dimensions?: number;
    dtype?: "fp32" | "q8";
    /** `mean` for e5 models, `cls` for BGE models, as each model card specifies. */
    pooling?: "mean" | "cls";
    queryPrefix?: string;
    documentPrefix?: string;
    /** ONNX intra-op threads; unset leaves the runtime default (all cores). */
    threads?: number;
  }> = {},
): Embedder {
  const model = options.model ?? "Xenova/multilingual-e5-small";
  const revision = options.revision ?? "761b726dd34fb83930e26aab4e9ac3899aa1fa78";
  const dtype = options.dtype ?? "q8";
  const queryPrefix = options.queryPrefix ?? "query: ";
  const documentPrefix = options.documentPrefix ?? "passage: ";
  const pooling = options.pooling ?? "mean";
  const space: EmbeddingSpace = {
    model: `${model}#${dtype}`,
    revision,
    dimensions: options.dimensions ?? 384,
    normalized: true,
    preprocessing: `${pooling} pooling; prefixes query=${JSON.stringify(queryPrefix)} document=${JSON.stringify(documentPrefix)}`,
  };
  let extractor: Promise<(texts: string[], o: object) => Promise<{ tolist(): number[][] }>> | undefined;
  const load = () => {
    extractor ??= import("@huggingface/transformers").then(
      ({ pipeline }) =>
        pipeline("feature-extraction", model, { revision, dtype, ...sessionOptions(options.threads) }) as unknown as Promise<
          (texts: string[], o: object) => Promise<{ tolist(): number[][] }>
        >,
    );
    return extractor;
  };
  async function embed(texts: string[]): Promise<number[][]> {
    const fe = await load();
    const vectors: number[][] = [];
    // Batches of 16 keep peak memory flat on long corpora.
    for (let i = 0; i < texts.length; i += 16) {
      const out = await fe(texts.slice(i, i + 16), { pooling, normalize: true });
      vectors.push(...out.tolist());
    }
    return checked(space, vectors, texts.length);
  }
  return {
    embedDocuments: (texts) => embed(texts.map((t) => documentPrefix + t)),
    embedQuery: async (q) => (await embed([queryPrefix + q]))[0] as number[],
    describeModel: () => space,
  };
}

/**
 * AN OPENAI-COMPATIBLE `/v1/embeddings` ENDPOINT: NVIDIA NIM / API catalog, Hugging Face Text
 * Embeddings Inference, vLLM, llama.cpp server, or a custom service. `extraBody` carries
 * provider-specific fields, for example NVIDIA's `input_type` (`query` or `passage`).
 * Contract-tested against a local fake only; no live provider has been called.
 */
export function createOpenAICompatibleEmbedder(
  options: Readonly<{
    baseUrl: string;
    model: string;
    revision: string;
    dimensions: number;
    normalized: boolean;
    apiKey?: string;
    queryBody?: Record<string, unknown>;
    documentBody?: Record<string, unknown>;
    fetch?: typeof fetch;
  }>,
): Embedder {
  const space: EmbeddingSpace = {
    model: `${options.baseUrl}|${options.model}`,
    revision: options.revision,
    dimensions: options.dimensions,
    normalized: options.normalized,
    preprocessing: `query=${JSON.stringify(options.queryBody ?? {})} document=${JSON.stringify(options.documentBody ?? {})}`,
  };
  const doFetch = options.fetch ?? fetch;
  async function embed(input: readonly string[], extra: Record<string, unknown> | undefined) {
    const response = await doFetch(`${options.baseUrl.replace(/\/+$/u, "")}/v1/embeddings`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...(options.apiKey === undefined ? {} : { authorization: `Bearer ${options.apiKey}` }),
      },
      body: JSON.stringify({ model: options.model, input, ...extra }),
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) throw new Error(`embedder_failed: http_${response.status}`);
    const body = (await response.json()) as { data?: Array<{ index: number; embedding: number[] }> };
    const rows = [...(body.data ?? [])].sort((a, b) => a.index - b.index);
    return checked(space, rows.map((r) => r.embedding), input.length);
  }
  return {
    embedDocuments: (texts) => embed(texts, options.documentBody),
    embedQuery: async (q) => (await embed([q], options.queryBody))[0] as number[],
    describeModel: () => space,
  };
}

/**
 * CLOUDFLARE WORKERS AI embeddings (`/accounts/{id}/ai/run/@cf/...`, reply `result.data`).
 * Contract-tested against a local fake only; no live account has been called.
 */
export function createCloudflareEmbedder(
  options: Readonly<{
    accountId: string;
    apiToken: string;
    model: string;
    dimensions: number;
    baseUrl?: string;
    fetch?: typeof fetch;
  }>,
): Embedder {
  const base = options.baseUrl ?? "https://api.cloudflare.com/client/v4";
  const space: EmbeddingSpace = {
    model: `cloudflare|${options.model}`,
    revision: "provider-managed",
    dimensions: options.dimensions,
    normalized: false,
    preprocessing: "none",
  };
  const doFetch = options.fetch ?? fetch;
  async function embed(text: readonly string[]) {
    const response = await doFetch(`${base}/accounts/${options.accountId}/ai/run/${options.model}`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${options.apiToken}` },
      body: JSON.stringify({ text }),
      signal: AbortSignal.timeout(30_000),
    });
    if (!response.ok) throw new Error(`embedder_failed: http_${response.status}`);
    const body = (await response.json()) as { result?: { data?: number[][] } };
    return checked(space, body.result?.data ?? [], text.length);
  }
  return {
    embedDocuments: (texts) => embed(texts),
    embedQuery: async (q) => (await embed([q]))[0] as number[],
    describeModel: () => space,
  };
}

/** One vector per input, each of the declared dimension, or the call failed. */
function checked(space: EmbeddingSpace, vectors: number[][], count: number): number[][] {
  if (vectors.length !== count) throw new Error("embedder_failed: wrong vector count");
  for (const v of vectors)
    if (v.length !== space.dimensions || !v.every(Number.isFinite))
      throw new Error(`embedder_failed: expected ${space.dimensions} finite dimensions`);
  return vectors;
}

const sessionOptions = (threads?: number) =>
  threads ? { session_options: { intraOpNumThreads: threads, interOpNumThreads: 1 } } : {};

/** A LOCAL CROSS-ENCODER through transformers.js; the default is bge-reranker-base. */
export function createLocalReranker(
  options: Readonly<{ model?: string; revision?: string; dtype?: "fp32" | "q8"; threads?: number }> = {},
): Reranker {
  const model = options.model ?? "Xenova/bge-reranker-base";
  const revision = options.revision ?? "280bcc27a84e0b898c251e06fddb25171bd9b101";
  const dtype = options.dtype ?? "q8";
  type Loaded = {
    tokenizer: (a: string[], o: object) => unknown;
    model: (inputs: unknown) => Promise<{ logits: { tolist(): number[][] } }>;
  };
  let loaded: Promise<Loaded> | undefined;
  const load = () => {
    loaded ??= import("@huggingface/transformers").then(async (t) => ({
      tokenizer: (await t.AutoTokenizer.from_pretrained(model, { revision })) as unknown as Loaded["tokenizer"],
      model: (await t.AutoModelForSequenceClassification.from_pretrained(model, {
        revision,
        dtype,
        ...sessionOptions(options.threads),
      })) as unknown as Loaded["model"],
    }));
    return loaded;
  };
  return {
    name: `${model}@${revision.slice(0, 7)}#${dtype}`,
    async rerank(query, documents) {
      if (documents.length === 0) return [];
      const { tokenizer, model: m } = await load();
      const scores: number[] = [];
      for (let i = 0; i < documents.length; i += 8) {
        const docs = documents.slice(i, i + 8);
        const inputs = tokenizer(
          docs.map(() => query),
          { text_pair: docs, padding: true, truncation: true, max_length: 512 },
        );
        const { logits } = await m(inputs);
        scores.push(...logits.tolist().map((row) => row[0] as number));
      }
      return scores;
    },
  };
}
