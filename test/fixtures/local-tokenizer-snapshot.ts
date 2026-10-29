// External-library boundary stays in an isolated process, with no model load.
import { expect, mock } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const cacheDir = mkdtempSync(join(tmpdir(), "hyperag-tokenizer-"));
const model = "onnx-community/embeddinggemma-2-ONNX";
const revision = "daa72c51243991dfcaf9f9137d2c573d8f7790c0";
const snapshot = join(cacheDir, model, revision);
const environment = {
  cacheDir,
  localModelPath: "unchanged",
  allowRemoteModels: false,
};
const calls: { path: string; options: object }[] = [];
mock.module("@huggingface/transformers", () => ({
  env: environment,
  AutoConfig: { from_pretrained: async () => ({}) },
  AutoTokenizer: {
    from_pretrained: async (path: string, options: object) => {
      calls.push({ path, options });
      return (texts: string[]) => texts;
    },
  },
  AutoModel: {
    from_pretrained: async (path: string) => {
      expect(path).toBe(model);
      return async (texts: string[]) => ({
        sentence_embedding: {
          normalize: () => ({
            tolist: () => texts.map(() => [1, ...Array(767).fill(0)]),
          }),
        },
      });
    },
  },
}));
try {
  const { createLocalEmbedder } = await import("../../src/adapters/embedders");
  mkdirSync(snapshot, { recursive: true });
  for (const name of ["tokenizer.json", "tokenizer_config.json"])
    writeFileSync(join(snapshot, name), "{}");
  const pinned = createLocalEmbedder({ profile: "gemma2" });
  await pinned.embedQuery("fixture");
  expect(calls.pop()).toEqual({
    path: snapshot,
    options: { revision, local_files_only: true },
  });
  expect(pinned.describeModel().model).toBe(`${model}#q8`);
  expect(pinned.describeModel().revision).toBe(revision);
  rmSync(join(snapshot, "tokenizer.json"));
  await createLocalEmbedder({ profile: "gemma2" }).embedQuery("fixture");
  expect(calls.pop()).toEqual({ path: model, options: { revision } });
  await createLocalEmbedder({
    profile: "gemma2",
    revision: "different",
  }).embedQuery("fixture");
  expect(calls.pop()).toEqual({
    path: model,
    options: { revision: "different" },
  });
  expect(environment).toEqual({
    cacheDir,
    localModelPath: "unchanged",
    allowRemoteModels: false,
  });
  console.log("pinned tokenizer snapshot contract PASS");
} finally {
  rmSync(cacheDir, { recursive: true, force: true });
}
