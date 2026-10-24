// The Rust service accepts at most 1 MiB of JSON per request.
export function* memoryBatches<T>(memories: readonly T[]): Generator<T[]> {
  const limit = 1_048_576;
  const envelope = Buffer.byteLength('{"memories":[]}');
  let batch: T[] = [];
  let bytes = envelope;
  for (const memory of memories) {
    const size = Buffer.byteLength(JSON.stringify(memory));
    if (envelope + size > limit) throw new Error("A memory exceeds the service JSON body limit");
    if (batch.length === 256 || bytes + size + (batch.length ? 1 : 0) > limit) {
      yield batch;
      batch = [];
      bytes = envelope;
    }
    bytes += size + (batch.length ? 1 : 0);
    batch.push(memory);
  }
  if (batch.length) yield batch;
}
