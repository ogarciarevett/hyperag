import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { z } from "zod";

import type { Embedder, EmbeddingSpace, Reranker } from "../types.js";

const LocalOptions = z
  .object({
    profile: z.enum(["gemma2", "e5-small"]).optional(),
    model: z.string().min(1).optional(),
    revision: z.string().min(1).optional(),
    dimensions: z.number().int().positive().optional(),
    dtype: z.enum(["fp32", "q8"]).optional(),
    pooling: z.enum(["mean", "cls", "none"]).optional(),
    queryPrefix: z.string().optional(),
    documentPrefix: z.string().optional(),
    threads: z.number().int().min(1).max(64).optional(),
  })
  .strict()
  .refine((o) => {
    if (isGemma(o))
      return (
        (o.model === undefined || o.model === GEMMA2) &&
        (o.dimensions === undefined || o.dimensions === 768) &&
        (o.pooling === undefined || o.pooling === "none")
      );
    return (
      o.pooling !== "none" &&
      (o.profile !== "e5-small" || o.model === undefined || o.model === E5)
    );
  });

const GEMMA2 = "onnx-community/embeddinggemma-2-ONNX";
const GEMMA2_REVISION = "daa72c51243991dfcaf9f9137d2c573d8f7790c0";
const E5 = "Xenova/multilingual-e5-small";
const E5_REVISION = "761b726dd34fb83930e26aab4e9ac3899aa1fa78";
const isGemma = (o: { profile?: string; model?: string }) =>
  o.profile === "gemma2" || (o.profile === undefined && o.model === GEMMA2);

/** External inference boundary; injection allows offline contract tests and preloaded runtimes. */
export type LocalEmbeddingLoader = (
  profile: Readonly<{
    model: string;
    revision: string;
    dtype: "fp32" | "q8";
    pooling: "mean" | "cls" | "none";
    output: "sentence_embedding" | null;
    threads?: number;
  }>,
) => Promise<(texts: string[]) => Promise<number[][]>>;

/** Local pinned ONNX profiles. Legacy selection stays default until measured promotion. */
export function createLocalEmbedder(
  options: z.input<typeof LocalOptions> = {},
  loadModel: LocalEmbeddingLoader = loadLocalModel,
): Embedder {
  const parsed = LocalOptions.safeParse(options);
  if (!parsed.success) throw new Error("embedder_invalid_config");
  const o = parsed.data;
  const gemma = isGemma(o);
  const model = o.model ?? (gemma ? GEMMA2 : E5);
  const revision = o.revision ?? (gemma ? GEMMA2_REVISION : E5_REVISION);
  const dtype = o.dtype ?? "q8";
  const queryPrefix =
    o.queryPrefix ?? (gemma ? "task: search result | query: " : "query: ");
  const documentPrefix =
    o.documentPrefix ?? (gemma ? "title: none | text: " : "passage: ");
  const pooling = o.pooling ?? (gemma ? "none" : "mean");
  const output = gemma ? "sentence_embedding" : null;
  const space: EmbeddingSpace = {
    model: `${model}#${dtype}`,
    revision,
    dimensions: o.dimensions ?? (gemma ? 768 : 384),
    normalized: true,
    preprocessing: gemma
      ? `model output ${output}; prompts query=${JSON.stringify(queryPrefix)} document=${JSON.stringify(documentPrefix)}`
      : `${pooling} pooling; prefixes query=${JSON.stringify(queryPrefix)} document=${JSON.stringify(documentPrefix)}`,
  };
  let extractor: ReturnType<LocalEmbeddingLoader> | undefined;
  async function embed(texts: string[]): Promise<number[][]> {
    const vectors: number[][] = [];
    try {
      extractor ??= loadModel({
        model,
        revision,
        dtype,
        pooling,
        output,
        threads: o.threads ?? (gemma ? 4 : undefined),
      });
      const fe = await extractor;
      for (let i = 0; i < texts.length; i += 16)
        vectors.push(...(await fe(texts.slice(i, i + 16))));
    } catch {
      throw new Error("embedder_failed: local inference");
    }
    const checkedVectors = checked(space, vectors, texts.length);
    if (checkedVectors.some((v) => Math.abs(Math.hypot(...v) - 1) > 0.001))
      throw new Error("embedder_failed: local output is not normalized");
    return checkedVectors;
  }
  return {
    embedDocuments: (texts) => embed(texts.map((t) => documentPrefix + t)),
    embedQuery: async (q) => (await embed([queryPrefix + q]))[0] as number[],
    describeModel: () => space,
  };
}

const loadLocalModel: LocalEmbeddingLoader = async (profile) => {
  const t = await import("@huggingface/transformers");
  const options = {
    revision: profile.revision,
    dtype: profile.dtype,
    ...sessionOptions(profile.threads),
  };
  if (profile.output === "sentence_embedding") {
    const config = await t.AutoConfig.from_pretrained(profile.model, {
      revision: profile.revision,
    });
    // The generic pipeline loads all modalities and does not read sentence_embedding in 4.3.1.
    const modalities = config as unknown as Record<string, unknown>;
    modalities.vision_config = null;
    modalities.audio_config = null;
    // 4.3.1 discovers tokenizer metadata without forwarding revision. Prefer the
    // exact existing snapshot; this changes loading location, never model identity.
    const snapshot = pinnedTokenizerSnapshot(t.env?.cacheDir, profile);
    const tokenizer = await t.AutoTokenizer.from_pretrained(
      snapshot ?? profile.model,
      {
        revision: profile.revision,
        ...(snapshot ? { local_files_only: true } : {}),
      },
    );
    const encoder = await t.AutoModel.from_pretrained(profile.model, {
      ...options,
      config,
    });
    return async (texts) => {
      const result = await encoder(
        tokenizer(texts, { padding: true, truncation: true, max_length: 8192 }),
      );
      const embedding = result.sentence_embedding;
      if (!embedding)
        throw new Error("embedder_failed: missing sentence_embedding");
      return embedding.normalize(2, -1).tolist() as number[][];
    };
  }
  const pipeline = await t.pipeline(
    "feature-extraction",
    profile.model,
    options,
  );
  return async (texts) =>
    (
      await pipeline(texts, { pooling: profile.pooling, normalize: true })
    ).tolist() as number[][];
};

function pinnedTokenizerSnapshot(
  cacheDir: string | null | undefined,
  profile: Readonly<{ model: string; revision: string }>,
): string | undefined {
  if (
    !cacheDir ||
    profile.model !== GEMMA2 ||
    profile.revision !== GEMMA2_REVISION
  )
    return undefined;
  const directory = resolve(cacheDir, profile.model, profile.revision);
  return ["tokenizer.json", "tokenizer_config.json"].every((file) =>
    existsSync(resolve(directory, file)),
  )
    ? directory
    : undefined;
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
  async function embed(
    input: readonly string[],
    extra: Record<string, unknown> | undefined,
  ) {
    const response = await doFetch(
      `${options.baseUrl.replace(/\/+$/u, "")}/v1/embeddings`,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          ...(options.apiKey === undefined
            ? {}
            : { authorization: `Bearer ${options.apiKey}` }),
        },
        body: JSON.stringify({ model: options.model, input, ...extra }),
        signal: AbortSignal.timeout(30_000),
      },
    );
    if (!response.ok)
      throw new Error(`embedder_failed: http_${response.status}`);
    const body = (await response.json()) as {
      data?: Array<{ index: number; embedding: number[] }>;
    };
    const rows = [...(body.data ?? [])].sort((a, b) => a.index - b.index);
    return checked(
      space,
      rows.map((r) => r.embedding),
      input.length,
    );
  }
  return {
    embedDocuments: (texts) => embed(texts, options.documentBody),
    embedQuery: async (q) =>
      (await embed([q], options.queryBody))[0] as number[],
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
    const response = await doFetch(
      `${base}/accounts/${options.accountId}/ai/run/${options.model}`,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${options.apiToken}`,
        },
        body: JSON.stringify({ text }),
        signal: AbortSignal.timeout(30_000),
      },
    );
    if (!response.ok)
      throw new Error(`embedder_failed: http_${response.status}`);
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
function checked(
  space: EmbeddingSpace,
  vectors: number[][],
  count: number,
): number[][] {
  if (vectors.length !== count)
    throw new Error("embedder_failed: wrong vector count");
  for (const v of vectors)
    if (v.length !== space.dimensions || !v.every(Number.isFinite))
      throw new Error(
        `embedder_failed: expected ${space.dimensions} finite dimensions`,
      );
  return vectors;
}

const sessionOptions = (threads?: number) =>
  threads
    ? { session_options: { intraOpNumThreads: threads, interOpNumThreads: 1 } }
    : {};

/** A LOCAL CROSS-ENCODER through transformers.js; the default is bge-reranker-base. */
export function createLocalReranker(
  options: Readonly<{
    model?: string;
    revision?: string;
    dtype?: "fp32" | "q8";
    threads?: number;
  }> = {},
): Reranker {
  const model = options.model ?? "Xenova/bge-reranker-base";
  const revision =
    options.revision ?? "280bcc27a84e0b898c251e06fddb25171bd9b101";
  const dtype = options.dtype ?? "q8";
  type Loaded = {
    tokenizer: (a: string[], o: object) => unknown;
    model: (inputs: unknown) => Promise<{ logits: { tolist(): number[][] } }>;
  };
  let loaded: Promise<Loaded> | undefined;
  const load = () => {
    loaded ??= import("@huggingface/transformers").then(async (t) => ({
      tokenizer: (await t.AutoTokenizer.from_pretrained(model, {
        revision,
      })) as unknown as Loaded["tokenizer"],
      model: (await t.AutoModelForSequenceClassification.from_pretrained(
        model,
        {
          revision,
          dtype,
          ...sessionOptions(options.threads),
        },
      )) as unknown as Loaded["model"],
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