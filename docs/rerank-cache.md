# Rerank result cache

Cross-encoder reranking is deterministic for a fixed model, query text, and ordered candidate list with content versions. Set `HYPERAG_RERANK_CACHE` (or `Config::rerank_cache` in Rust / `rerankCache` in TypeScript) to cache rerank score vectors in memory.

- Default: off (`0`, `false`, `off`, or unset).
- Enable with default capacity (`256` entries): `true` or `on`.
- Enable with explicit LRU bound: any positive integer (for example `HYPERAG_RERANK_CACHE=512`).
- Cache key: SHA-256 of `(model_id, query_text, [(candidate_id, content_version), ...])` in the exact candidate order sent to the reranker.
- Cache value: the raw cross-encoder score vector (`Vec<f32>` in Rust, `number[]` in TypeScript).
- Invalidation: any write that advances or retires a candidate's content version changes the `(candidate_id, version)` pair in future keys and evicts cached entries referencing stale versions of that candidate.

Both caches also enforce an 8 MiB total retained-storage budget, regardless of entry capacity. An entry larger than the budget is skipped; otherwise least recently used entries are evicted until both bounds hold. Rust accounts for allocated capacities (including spare capacity) of IDs, candidate tuples, score vectors and digest keys. TypeScript accounts for UTF-16 ID bytes, candidate slots, score doubles and a fixed per-entry allowance. Runtime allocator/Map overhead and transient hashing/inference allocations are outside this payload budget.

Rust `set_models` clears the rerank cache under the models write lock on every reranker replacement or removal, including replacements using the same model spec. Retrieval keeps the read lock through scoring and insertion, so an old scorer cannot repopulate the cache after a switch.
