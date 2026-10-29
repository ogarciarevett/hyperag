// Separate process: replacing the external library cannot affect other test files.
import { expect, mock } from "bun:test";

const queries: string[][] = [];
let encoderLoads = 0;
let pipelineLoads = 0;
mock.module("@huggingface/transformers", () => ({
  AutoConfig: {
    from_pretrained: async (_model: string, options: object) => {
      expect(options).toEqual({
        revision: "daa72c51243991dfcaf9f9137d2c573d8f7790c0",
      });
      return { vision_config: {}, audio_config: {} };
    },
  },
  AutoTokenizer: {
    from_pretrained: async () => (texts: string[]) => ({ texts }),
  },
  AutoModel: {
    from_pretrained: async (
      _model: string,
      options: { config: object; dtype: string },
    ) => {
      encoderLoads++;
      expect(options.config).toEqual({
        vision_config: null,
        audio_config: null,
      });
      expect(options.dtype).toBe("q8");
      return async (input: { texts: string[] }) => {
        queries.push(input.texts);
        return {
          sentence_embedding: {
            normalize: (p: number, axis: number) => {
              expect([p, axis]).toEqual([2, -1]);
              return {
                tolist: () =>
                  input.texts.map(() => [
                    0.6,
                    0.8,
                    ...Array<number>(766).fill(0),
                  ]),
              };
            },
          },
        };
      };
    },
  },
  pipeline: async (_task: string, model: string) => {
    pipelineLoads++;
    expect(model).toBe("Xenova/multilingual-e5-small");
    return async (_texts: string[], options: object) => {
      expect(options).toEqual({ pooling: "mean", normalize: true });
      return { tolist: () => [[1, ...Array<number>(383).fill(0)]] };
    };
  },
}));

const { createLocalEmbedder } = await import("../../src/adapters/embedders");
const gemma = createLocalEmbedder({ profile: "gemma2" });
const vector = await gemma.embedQuery("question");
expect(vector).toHaveLength(768);
expect(Math.hypot(...vector)).toBeCloseTo(1);
await gemma.embedDocuments(["document"]);
expect(queries).toEqual([
  ["task: search result | query: question"],
  ["title: none | text: document"],
]);
expect(encoderLoads).toBe(1);
expect(pipelineLoads).toBe(0);
expect(
  await createLocalEmbedder({ profile: "e5-small" }).embedQuery("legacy"),
).toHaveLength(384);
expect(pipelineLoads).toBe(1);
console.log("text-only runtime contract PASS");
