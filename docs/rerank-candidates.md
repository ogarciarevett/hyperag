# Rust rerank candidate budget

A request with `"rerankCandidates": 8` limits the cross-encoder to the first eight fused candidates and overrides `HYPERAG_RERANK_CANDIDATES`. The environment variable supplies the default when the request omits the field. Positive integer values from 1 to150 are accepted and clamped to the available pool. Omit both values to preserve full-pool reranking. HTTP request values use the same 1–150 bounds as the environment default; invalid request bounds are rejected before retrieval. A valid request override also takes precedence over an invalid environment default.

This budget applies after channels have retrieved their usual candidates and fusion has ordered them. Keep `candidatesPerChannel=24` for lexical, vector and graph recall. The loaded pool remains unchanged. Only the scored prefix is reordered; the unscored tail retains its fused order and has no `rerankScore`. With a result limit above the cap, that tail can also be returned. This avoids promoting unscored rows above negative cross-encoder logits.

Set the environment value and restart the service to change the budget without rebuilding. Measure quality and latency on a fixed question set before retaining a cap. The flag does not switch embedding spaces or models.
