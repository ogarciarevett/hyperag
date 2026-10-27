# Rerank result cache

Cross-encoder reranking is deterministic for a fixed model, query text, and ordered candidate list with content versions. Set `HYPERAG_RERANK_CACHE` (or `Config::rerank_cache` in Rust / `rerankCache` in TypeScript) to cache rerank score vectors in memory.

- Default: off (`0`, `false`, `off`, or unset).
- Enable with default capacity (`256` entries): `true` or `on`.
- Enable with explicit LRU bound: any positive integer (for example `HYPERAG_RERANK_CACHE=512`).
- Cache key: SHA-256 of `(model_id, query_text, [(candidate_id, content_version), ...])` in the exact candidate order sent to the reranker.
- Cache value: the raw cross-encoder score vector (`Vec<f32>` in Rust, `number[]` in TypeScript).
- Invalidation: any write that advances or retires a candidate's content version changes the `(candidate_id, version)` pair in future keys and evicts cached entries referencing stale versions of that candidate.
