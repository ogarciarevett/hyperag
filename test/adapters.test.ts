import { afterAll, describe, expect, test } from "bun:test";

import {
  createCloudflareEmbedder,
  createLocalReranker,
  createOpenAICompatibleEmbedder,
} from "../src/adapters/embedders";
import { spaceId } from "../src/adapters/postgres";
import type { Embedder } from "../src/types";
import { embedder, freshDatabase, open, uniq } from "./helpers";

/** The contract every embedder adapter must keep. */
async function embedderContract(e: Embedder) {
  const space = e.describeModel();
  const docs = await e.embedDocuments(["alpha", "beta"]);
  expect(docs).toHaveLength(2);
  for (const v of docs) expect(v).toHaveLength(space.dimensions);
  expect(await e.embedQuery("alpha")).toHaveLength(space.dimensions);
  expect(space.model.length).toBeGreaterThan(0);
  expect(space.revision.length).toBeGreaterThan(0);
}

// A local fake of an OpenAI-compatible and a Workers AI endpoint: it checks request shape only.
const seen: unknown[] = [];
const fake = Bun.serve({
  port: 0,
  async fetch(req) {
    const body = (await req.json()) as { input?: string[]; text?: string[] };
    seen.push({ path: new URL(req.url).pathname, auth: req.headers.get("authorization"), body });
    const n = (body.input ?? body.text ?? []).length;
    const rows = Array.from({ length: n }, (_, i) => Array.from({ length: 4 }, (_, j) => i + j / 10));
    if (body.text) return Response.json({ success: true, result: { shape: [n, 4], data: rows } });
    // Out of order on purpose: the adapter must sort by index.
    return Response.json({ data: rows.map((embedding, index) => ({ index, embedding })).reverse() });
  },
});
afterAll(() => fake.stop());

describe("embedder contract", () => {
  test("local transformers.js embedder", async () => {
    await embedderContract(embedder());
    const [q] = [await embedder().embedQuery("hola")];
    expect(Math.hypot(...q)).toBeCloseTo(1, 3);
  }, 60_000);

  test("OpenAI-compatible endpoint (NVIDIA / TEI / custom shape)", async () => {
    const e = createOpenAICompatibleEmbedder({
      baseUrl: fake.url.origin,
      model: "nvidia/nv-embedqa-e5-v5",
      revision: "v5",
      dimensions: 4,
      normalized: false,
      apiKey: "test-key",
      queryBody: { input_type: "query" },
      documentBody: { input_type: "passage" },
    });
    await embedderContract(e);
    const [docs] = await e.embedDocuments(["a", "b"]);
    expect(docs).toEqual([0, 0.1, 0.2, 0.3]);
    expect(seen).toContainEqual(
      expect.objectContaining({ path: "/v1/embeddings", auth: "Bearer test-key", body: expect.objectContaining({ input_type: "query" }) }),
    );
  });

  test("Cloudflare Workers AI shape", async () => {
    await embedderContract(
      createCloudflareEmbedder({ accountId: "acct", apiToken: "t", model: "@cf/baai/bge-m3", dimensions: 4, baseUrl: fake.url.origin }),
    );
    expect(seen).toContainEqual(expect.objectContaining({ path: "/accounts/acct/ai/run/@cf/baai/bge-m3" }));
  });

  test("a wrong dimension is refused, never stored", async () => {
    const e = createOpenAICompatibleEmbedder({ baseUrl: fake.url.origin, model: "m", revision: "r", dimensions: 8, normalized: false });
    await expect(e.embedQuery("x")).rejects.toThrow("expected 8");
  });

  test("same dimension, different model: different space", () => {
    const a = { model: "a", revision: "1", dimensions: 384, normalized: true, preprocessing: "none" };
    expect(spaceId(a)).not.toBe(spaceId({ ...a, model: "b" }));
    expect(spaceId(a)).not.toBe(spaceId({ ...a, preprocessing: "query: " }));
  });
});

describe("reranker contract", () => {
  test("local cross-encoder scores the relevant document higher", async () => {
    const r = createLocalReranker();
    const [good, bad] = await r.rerank("which database stores memory?", ["Memory is stored in Postgres.", "The cat sat on the mat."]);
    expect(good).toBeGreaterThan(bad as number);
    expect(await r.rerank("q", [])).toEqual([]);
  }, 120_000);
});

describe("model switch", () => {
  test("a second space gets its own index; source, BM25 and the first index stay", async () => {
    const db = await freshDatabase();
    const scope = uniq("switch");
    const first = await open(db.url);
    await first.hyperag.grant("admin", "ana", scope, "read");
    await first.hyperag.grant("admin", "ana", scope, "write");
    await first.hyperag.ingest("ana", { scope, source: "doc", text: "Embeddings switch test document about kayaks." });
    const other = createOpenAICompatibleEmbedder({ baseUrl: fake.url.origin, model: "fake-4d", revision: "1", dimensions: 4, normalized: false });
    const re = await first.hyperag.reindex(other);
    expect(re.embedded).toBe(1);
    const tables = await first.pool.query("select count(*)::int as n from embedding_spaces");
    expect(tables.rows[0].n).toBe(2);
    await first.close();
    const second = await open(db.url, { embedder: other });
    const r = await second.hyperag.retrieve("ana", "kayaks");
    expect(r.items[0]?.memory.text).toContain("kayaks");
    expect(r.embeddingSpace).toBe(re.space);
    await second.close();
    await db.drop();
  }, 60_000);
});
