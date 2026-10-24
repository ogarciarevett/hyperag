# Evaluation protocol (fixed before the first run, 2026-10-23)

## Baseline

The baseline is the frozen packaged benchmark of 2026-10-21 (`packaged-20261021-v4` and its `-quick` supplement), run on the original machine. Its corpus, queries, scorer and results are kept outside this repository and are read only. This evaluation does not re-run the baseline engines; it quotes their sealed numbers and runs Hyperag and Engram here.

Baseline numbers quoted, cross-team suite, 128 queries (112 answerable):

| Engine (baseline run) | nDCG@10 | recall@10 | all evidence@10 |
| --- | --- | --- | --- |
| baseline (agents-runtime v4: BM25 + pgvector + RRF) | 0.717 | 0.787 | 0.723 |
| Cognee | 0.705 | 0.766 | 0.652 |
| Memvid hybrid | 0.589 | 0.682 | 0.554 |
| Graphiti | 0.567 | 0.704 | 0.571 |
| Mem0 | 0.555 | 0.621 | 0.482 |
| Engram (as packaged then) | 0.170 | 0.201 | 0.188 |

The baseline (agents-runtime v4) SciFact run did not complete in the baseline (adapter ingestion timeout).

## Suites

1. **Cross-team (frozen).** `benchmarks/packaged/data/cross-team`: 1,536 short documents in four scopes (engineering, security, finance, support), 128 queries, half English and half Spanish, categories lookup, exact identifier, multi-evidence (2 and 3), temporal evidence, cross-team handoff, unanswerable (8) and unauthorized probes (8). Every query is asked by a principal that can read only the query's scope. This set is held out: nothing is tuned on it.
2. **Scenarios (new, small, reviewable).** `eval/scenarios.ts`: the behaviours the spec lists, each with a pass/fail check: retention across sessions, agent-to-agent handoff, correction of an outdated fact, conflicting concurrent update, permission revocation, deletion, restricted scope, insufficient evidence, English and Spanish. Run on Hyperag and on Engram 3.3.2 through the same task list.
3. **SciFact (optional, if resources allow).** 5,183 documents, 300 queries, same scorer.

## Scoring

The baseline's own scorer (`benchmarks/packaged/evaluate.py`, `score_query` and `aggregate`) is imported unchanged: first ten hits, chunks collapsed to parents, linear nDCG, cross-scope hits counted as leaks and given zero gain. Additions measured here: abstention on the 16 probes (`insufficientEvidence`), false abstention on the 112 answerable queries, and per-category all-evidence (cross-team handoff is the 6/16 case in the baseline).

## Variants (one component at a time)

All variants use the same corpus, scopes, `limit = 10`, 24 candidates per channel, RRF k = 60. No generator model is used, so none can differ.

| Variant | BM25 | Vector | Graph | Re-ranker | Embedding model |
| --- | --- | --- | --- | --- | --- |
| V0 baseline profile | yes | yes | no | no | BAAI/bge-small-en-v1.5 (as in the baseline) |
| V1 + graph | yes | yes | yes | no | bge-small-en-v1.5 |
| V2 + re-ranker | yes | yes | no | yes | bge-small-en-v1.5 |
| V3 embeddings | yes | yes | no | no | multilingual-e5-small |
| V4 full | yes | yes | yes | yes | multilingual-e5-small |
| L lexical only | yes | no | no | no | none |

Graph edges are built at ingestion from identifiers found in the document text (`[A-Z][a-z]+-\d+` project codes and `[A-Z]+-[A-Z0-9]+` ticket ids): two documents in the same scope that share an identifier are linked `SHARES_ID`. No label, gold id or query is used. Re-ranker: bge-reranker-base, default `minRerankScore = 0`. Without a re-ranker, `minVectorScore = 0.82`. Both thresholds were fixed before this run from a three-document probe unrelated to the suite.

## Hypotheses

- H1: V0 reproduces the baseline (agents-runtime v4) profile within ±0.03 nDCG@10 (same algorithm, same weights).
- H2: the graph (V1) raises all-evidence@10 on cross-team handoff and multi-evidence queries.
- H3: the re-ranker (V2) raises nDCG@10; it may hurt Spanish (an English-centred model).
- H4: multilingual embeddings (V3) raise Spanish quality over V0.
- H5: Hyperag returns zero cross-scope hits in every variant.
- H6: Hyperag passes more scenario checks than Engram, because Engram has no versions-with-validity, permission model or abstention signal; this is a capability comparison, not a quality one.

## Performance

Measured for V0 and V4: ingestion time (including embedding), warm sequential latency p50/p95 over the 128 queries (first pass discarded as warm-up, then one timed pass; repeated runs are not new quality observations), throughput at 1, 4 and 8 concurrent in-process clients sharing one 8-connection pool (genuine concurrency up to the pool size; local CPU models serialise inside one process, which is reported), process RSS peak, Postgres database size. Startup time of a fresh process to first answer is measured once.

## Deviations recorded after the first run

- The bge variants use the baseline's runtime (fastembed 0.9.0) through `eval/fastembed_server.py` instead of the transformers.js export, after the first V0 run (0.664) differed from the baseline. Changing the model build did not change the score.
- Two adapter faults found while checking H1 were fixed in the core and apply to every variant: BGE CLS pooling, and `hnsw.ef_search` raised from 40 to at least 100 (HNSW returned 92% of the exact top 24 under the scope filter; measured against exact search, not against labels). A newest-first tie-break from the original code was also restored.
- Engram's default search requires every query word and returned nothing on this suite, so it is also run with its `match_mode=any` option. Both are reported.
