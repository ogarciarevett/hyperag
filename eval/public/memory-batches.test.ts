import { expect, test } from "bun:test";
import { memoryBatches } from "./memory-batches";

for (const text of ["x".repeat(15000), "界".repeat(15000), '"\\\n'.repeat(5000)]) {
  test(`serialized batches respect the service limit (${Buffer.byteLength(text)} text bytes)`, () => {
    const memories = Array.from({ length: 100 }, (_, id) => ({ scope: "lme-s/q", id: String(id), source: "speaker", text }));
    const batches = [...memoryBatches(memories)];
    expect(batches.length).toBeGreaterThan(1);
    expect(batches.flat()).toEqual(memories);
    for (const batch of batches) expect(Buffer.byteLength(JSON.stringify({ memories: batch }))).toBeLessThanOrEqual(1_048_576);
  });
}

test("small batches retain the count cap and order", () => {
  const memories = Array.from({ length: 513 }, (_, id) => ({ id }));
  const batches = [...memoryBatches(memories)];
  expect(batches.map((batch) => batch.length)).toEqual([256, 256, 1]);
  expect(batches.flat()).toEqual(memories);
  expect([...memoryBatches([])]).toEqual([]);
});

test("a single oversized memory fails explicitly", () => {
  expect(() => [...memoryBatches([{ text: "x".repeat(1_048_576) }])]).toThrow("exceeds the service JSON body limit");
});

test("the exact JSON envelope limit fits and one extra byte is refused", () => {
  const overhead = Buffer.byteLength(JSON.stringify({ memories: [{ text: "" }] }));
  const exact = { text: "x".repeat(1_048_576 - overhead) };
  expect(Buffer.byteLength(JSON.stringify({ memories: [...memoryBatches([exact])][0] }))).toBe(1_048_576);
  expect(() => [...memoryBatches([{ text: exact.text + "x" }])]).toThrow();
});
