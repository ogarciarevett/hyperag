# Rust rerank candidate budget

`HYPERAG_RERANK_CANDIDATES=8` limits the cross-encoder to the first eight fused candidates. Positive integer values from 1 to150 are accepted and clamped to the available pool. Omit the setting to preserve full-pool reranking. Invalid values return a configuration error when reranking is requested.

This budget applies after channels have retrieved their usual candidates and fusion has ordered them. Keep `candidatesPerChannel=24` for lexical, vector and graph recall. The loaded pool remains unchanged. Only the scored prefix is reordered; the unscored tail retains its fused order and has no `rerankScore`. With a result limit above the cap, that tail can also be returned. This avoids promoting unscored rows above negative cross-encoder logits.

Set the environment value and restart the service to change the budget without rebuilding. Measure quality and latency on a fixed question set before retaining a cap. The flag does not switch embedding spaces or models.
