import { expect, test } from "bun:test";

import { createLocalEmbedder } from "../src/adapters/embedders";
import { spaceId } from "../src/adapters/postgres";
import type { PostgresStore } from "../src/adapters/postgres";
import { Hyperag } from "../src/core";
import type { EmbeddingSpace } from "../src/types";

test("the explicit Gemma 2 profile describes the pinned text-only space", () => {
  expect(createLocalEmbedder({ profile: "gemma2" }).describeModel()).toEqual({
    model: "onnx-community/embeddinggemma-2-ONNX#q8",
    revision: "daa72c51243991dfcaf9f9137d2c573d8f7790c0",
    dimensions: 768,
    normalized: true,
    preprocessing:
      'model output sentence_embedding; prompts query="task: search result | query: " document="title: none | text: "',
  });
});

test("inference failures do not disclose input text", async () => {
  const embedder = createLocalEmbedder(
    { profile: "gemma2" },
    async () => async () => {
      throw new Error("fixture-sensitive-input");
    },
  );
  await expect(embedder.embedQuery("fixture-sensitive-input")).rejects.toThrow(
    "embedder_failed: local inference",
  );
});

test("the legacy profile keeps its original vector-space identity", () => {
  const legacy = createLocalEmbedder({ profile: "e5-small" }).describeModel();
  expect(legacy).toEqual({
    model: "Xenova/multilingual-e5-small#q8",
    revision: "761b726dd34fb83930e26aab4e9ac3899aa1fa78",
    dimensions: 384,
    normalized: true,
    preprocessing:
      'mean pooling; prefixes query="query: " document="passage: "',
  });
  expect(createLocalEmbedder().describeModel()).toEqual(legacy);
  expect(
    spaceId(createLocalEmbedder({ profile: "gemma2" }).describeModel()),
  ).not.toBe(spaceId(legacy));
});

test("unchanged initialization keeps an existing legacy vector space", async () => {
  const legacy = spaceId(
    createLocalEmbedder({ profile: "e5-small" }).describeModel(),
  );
  // Database boundary fixture refuses any new space: a default swap must fail this regression.
  const store = {
    migrate: async () => {},
    ensureSpace: async (descriptor: EmbeddingSpace) => {
      expect(spaceId(descriptor)).toBe(legacy);
      return legacy;
    },
    scopes: async () => [],
    load: async () => [],
  } as unknown as PostgresStore;
  const hyperag = new Hyperag({ store, embedder: createLocalEmbedder() });
  await hyperag.init();
  const result = await hyperag.retrieve("fixture", "question", {
    channels: { bm25: false, vector: false, graph: false, rerank: false },
  });
  expect(result.embeddingSpace).toBe(legacy);
});

test("options are validated before loading a model", () => {
  for (const options of [
    { dimensions: 0 },
    { profile: "gemma2", dimensions: 384 },
    { profile: "gemma2", pooling: "mean" },
    { revision: "" },
    { threads: -1 },
    { profile: "e5-small", model: "other" },
  ]) {
    expect(() => createLocalEmbedder(options as never)).toThrow(
      "embedder_invalid_config",
    );
  }
});

test("prefixes apply once, batching is bounded and one model load is reused", async () => {
  const seen: string[][] = [];
  let loads = 0;
  const embedder = createLocalEmbedder(
    { profile: "gemma2" },
    async (profile) => {
      loads++;
      expect(profile.output).toBe("sentence_embedding");
      expect(profile.pooling).toBe("none");
      return async (texts) => {
        seen.push(texts);
        return texts.map(() => [1, ...Array<number>(767).fill(0)]);
      };
    },
  );
  expect(await embedder.embedQuery("question")).toHaveLength(768);
  expect(
    await embedder.embedDocuments(Array<string>(17).fill("document")),
  ).toHaveLength(17);
  expect(seen).toEqual([
    ["task: search result | query: question"],
    Array<string>(16).fill("title: none | text: document"),
    ["title: none | text: document"],
  ]);
  expect(loads).toBe(1);
});

test("malformed inference output is refused before it can be stored", async () => {
  for (const vectors of [
    [],
    [[0]],
    [Array<number>(768).fill(0)],
    [[Number.NaN, ...Array<number>(767).fill(0)]],
  ]) {
    const embedder = createLocalEmbedder(
      { profile: "gemma2" },
      async () => async () => vectors,
    );
    await expect(embedder.embedQuery("question")).rejects.toThrow(
      "embedder_failed",
    );
  }
});

test("the default external runtime uses text-only projected output without downloads", () => {
  const result = Bun.spawnSync([
    process.execPath,
    "run",
    new URL("./fixtures/local-embedding-runtime.ts", import.meta.url).pathname,
  ]);
  expect(result.exitCode).toBe(0);
  expect(result.stdout.toString()).toContain("text-only runtime contract PASS");
});
