# Evaluation results (2026-10-23)

Protocol: [PROTOCOL.md](PROTOCOL.md), fixed before the first run. Machine: Mac Studio (Apple silicon), OrbStack, Postgres 18.6 + pgvector, Neo4j 5.26.28 Community, Bun 1.4.3, all models on CPU. Engram 3.3.2 (Homebrew binary on the same machine).

## Cross-team suite (internal smoke test, synthetic; not a headline claim)

Frozen, 128 queries, 112 answerable.

Scored with the baseline's own `evaluate.py`, unchanged. Baseline rows are re-scored from the baseline's sealed request records (`packaged-20261021-v4`, `-quick`) and match their published numbers.

| Run | nDCG@10 | recall@10 | all evidence@10 | handoff all evidence (16) | probe abstention (16) | false abstention (112) | cross-scope hits | p50 ms | p95 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Baseline (agents-runtime v4, 2026-10-21) | 0.717 | 0.787 | 0.723 | 0.375 (6/16) | n/a | n/a | 0 | 17.6 | 22.1 |
| Baseline Cognee | 0.705 | 0.766 | 0.652 | 0.062 | n/a | n/a | 0 | 43.5 | 51.1 |
| Baseline Memvid hybrid | 0.589 | 0.682 | 0.554 | 0.000 | n/a | n/a | 0 | 12.1 | 17.7 |
| Baseline Graphiti | 0.567 | 0.704 | 0.571 | 0.438 | n/a | n/a | 0 | 15.2 | 20.4 |
| Baseline Mem0 | 0.555 | 0.621 | 0.482 | 0.062 | n/a | n/a | 0 | 16.5 | 20.9 |
| Baseline Engram (packaged) | 0.170 | 0.201 | 0.188 | 0.500 | n/a | n/a | 0 | 4.1 | 6.6 |
| **Engram 3.3.2 here, default (all words)** | 0.000 | 0.000 | 0.000 | 0.000 | n/a | n/a | 0 | 0.6 | 0.9 |
| **Engram 3.3.2 here, `match_mode=any`** | 0.163 | 0.188 | 0.179 | 0.500 | n/a | n/a | 0 | 1.9 | 4.0 |
| Hyperag L (BM25 only) | 0.709 | 0.756 | 0.607 | 0.500 | 0.000 | 0.000 | 0 | 3.0 | 4.1 |
| Hyperag V0 (baseline profile) | 0.717 | 0.787 | 0.723 | 0.375 | 1.000 | 0.768 | 0 | 11.3 | 15.3 |
| Hyperag V1 (+ graph) | 0.700 | 0.857 | **0.812** | **0.562** | 1.000 | 0.768 | 0 | 13.4 | 18.6 |
| Hyperag V2 (+ re-ranker) | 0.692 | 0.789 | 0.705 | 0.188 | 0.750 | 0.348 | 0 | 206 | 246 |
| Hyperag V3 (e5 multilingual) | 0.749 | 0.750 | 0.545 | 0.000 | 0.000 | 0.000 | 0 | 8.7 | 10.3 |
| Hyperag V4 (e5 + graph + re-ranker) | **0.789** | **0.900** | 0.786 | 0.500 | 0.688 | 0.321 | 0 | 217 | 283 |

Paired bootstrap against the baseline (agents-runtime v4) (10,000 resamples by query, 95% interval):

| Variant | Δ nDCG@10 | Δ all evidence@10 |
| --- | --- | --- |
| V0 | 0.000 [−0.001, 0.000] | 0.000 [0.000, 0.000] |
| V1 graph | −0.017 [−0.051, 0.018] | **+0.089 [0.027, 0.161]** |
| V2 re-ranker | −0.025 [−0.079, 0.032] | −0.018 [−0.080, 0.045] |
| V3 e5 | +0.032 [−0.024, 0.087] | **−0.179 [−0.268, −0.089]** |
| V4 full | +0.072 [−0.003, 0.150] | +0.062 [−0.018, 0.152] |
| L BM25 only | −0.008 [−0.051, 0.037] | **−0.116 [−0.205, −0.036]** |
| Engram any | **−0.554 [−0.643, −0.467]** | **−0.545 [−0.652, −0.429]** |

By language (nDCG@10, en / es): baseline (agents-runtime v4) 0.802 / 0.632; V0 0.801 / 0.632; V1 0.793 / 0.607; V2 0.723 / 0.662; V3 0.779 / 0.719; V4 0.793 / 0.785; Engram any 0.316 / 0.009.

### Hypotheses

- **H1 holds.** V0 reproduces the baseline (agents-runtime v4) exactly (every metric within 0.001). Two adapter faults had to be fixed first: BGE needs CLS pooling, and pgvector's HNSW default `ef_search = 40` returned only 92% of the exact top 24 under the scope filter (now at least 100, which returns 100%). The baseline's embedding runtime (fastembed 0.9.0, Qdrant ONNX build of bge-small-en-v1.5) is served by `fastembed_server.py` through Hyperag's OpenAI-compatible adapter.
- **H2 holds for evidence, not ranking.** The graph raises all-evidence@10 by 0.089 (significant) and cross-team handoff from 6/16 to 9/16; nDCG does not change significantly.
- **H3 does not hold.** The re-ranker alone (V2) lowers nDCG and handoff on English; it helps Spanish (0.632 → 0.662). Its value here is abstention (see below).
- **H4 holds.** Multilingual embeddings raise Spanish nDCG from 0.632 to 0.719, but they lose multi-evidence recall badly (all evidence −0.179); only combined with the graph and the re-ranker (V4) is all evidence above baseline.
- **H5 holds.** Zero cross-scope hits in every variant.
- **H6 holds.** See scenarios below.

### Abstention

The cosine-threshold rule (no re-ranker, `minVectorScore = 0.82`, fixed before the run) does not work on this corpus: with bge it abstains on 77% of answerable queries, with e5 it never abstains. The re-ranker rule (`minRerankScore = 0`) abstains on 69–75% of the 16 probes at the cost of 32–35% false abstention. Neither is good enough to ship as a default; this is the first improvement round.

## Scenarios (13 tasks, same task list for every system; internal smoke test)

Engram runs as agents on this machine run it: its MCP server (`engram mcp`) and the mem_save, mem_search, mem_update, mem_delete and mem_get_observation tools; a correction reuses the corrected memory's `topic_key`, which is how Engram replaces a fact. Its default search and its `match_mode=any` option are both reported.

| Task | Hyperag | Engram 3.3.2 MCP, default | Engram 3.3.2 MCP, `match_mode=any` |
| --- | --- | --- | --- |
| retention across restart (en) | pass | FAIL (no result) | pass |
| retention across restart (es) | pass | FAIL (no result) | pass |
| handoff: finding and its evidence reach the second agent | pass | FAIL (no result) | pass |
| correction supersedes an outdated fact | pass | FAIL (no result) | pass |
| a correction still holds after a restart | pass | FAIL (no result) | pass |
| conflicting concurrent update is detected | pass | FAIL (last writer wins) | FAIL (last writer wins) |
| history explains a correction | pass | pass | pass |
| restricted scope is not reachable | pass | FAIL (any caller can read any project) | FAIL |
| revoked access stops reads | pass | FAIL (no per-principal permissions) | FAIL |
| deleted memory is not returned | pass | pass | pass |
| insufficient evidence is reported | pass | pass (empty result counted as abstaining) | FAIL (results returned, no signal) |
| valid time: an expired fact is not current | pass | FAIL (no valid-time fields) | FAIL |
| answer carries its source | pass | FAIL (no result) | pass |
| **Total** | **13/13** | **3/13** | **8/13** |

Hyperag ran with e5, graph and re-ranker. Raw outcomes: `results/scenarios.json`.

## Performance (cross-team, 1,536 documents)

| Profile | Ingest (embed + write) | Graph edges | Warm p50 / p95 | Throughput c=1 / 4 / 8 (q/s) | RSS peak | Storage |
| --- | --- | --- | --- | --- | --- | --- |
| Hyperag L | 14.8 s | 1,248 in 2.5 s | 3.0 / 4.1 ms | 353 / n/a / n/a | 27 MB | 11 MB Postgres |
| Hyperag V0 | 31.1 s | 1,248 in 2.3 s | 11.3 / 15.3 ms | 96 / 202 / 207 | 25 MB (+ the separate fastembed process) | 18 MB Postgres |
| Hyperag V4 | 25.1 s | 1,248 in 2.0 s | 217 / 283 ms | 4.6 / 4.8 / 4.9 | 1.67 GB | 18 MB Postgres + Neo4j |
| Engram 3.3.2 | 1.3 s | n/a | 1.9 / 4.0 ms | n/a | not sampled | 8 MB |

- Latency is end-to-end in-process time for warm calls (eight warm-up queries discarded), one pass of 128 queries; repeated passes are not new quality observations.
- c=4 and c=8 share one process and an 8-connection pool. V0's gain stops at about 200 q/s because the single embedding server serialises queries. V4 does not scale at all: the re-ranker runs on the CPU inside the one process, so the concurrency there is queueing, not parallel work.
- Startup to first answer: under 70 ms after models are cached; model load on first use adds about 5 s (e5) and 9 s (re-ranker).

## Public benchmarks

Running through `eval/public/` (BEIR with the official test split and pytrec_eval). First check: SciFact, Hyperag BM25 only, nDCG@10 0.660 (300 queries), against the published BM25 baseline of 0.665 for SciFact in the BEIR paper. Full tables follow in the next report.

## Rust build against the TypeScript reference (2026-10-23)

Same harness for both (`eval/run-suite-http.ts`, `eval/scenarios.ts --hyperag http`), smoke suite. The TypeScript service loads only e5, so the TypeScript bge rows come from the in-process `eval/run-suite.ts` runs with the same records.

| Variant | TypeScript nDCG / all evidence / handoff | Rust nDCG / all evidence / handoff | Δ Rust − TS, 95% CI |
| --- | --- | --- | --- |
| BM25 only | 0.709 / 0.607 / 8/16 | 0.709 / 0.607 / 8/16 | identical |
| baseline profile (bge) | 0.717 / 0.723 / 6/16 | 0.717 / 0.723 / 6/16 | identical |
| + graph | 0.700 / 0.812 / 9/16 | 0.700 / 0.812 / 9/16 | identical |
| + re-ranker (bge-reranker-base q8) | 0.692 / 0.705 / 3/16 | 0.693 / 0.705 / 3/16 | nDCG +0.001 [0.000, 0.002] |
| e5 | 0.749 / 0.545 / 0/16 | 0.754 / 0.554 / 0/16 | nDCG +0.006 [−0.003, 0.019] |
| e5 + graph + re-ranker | 0.789 / 0.786 / 8/16 | 0.784 / 0.812 / 8/16 | nDCG −0.005 [−0.026, 0.016], all evidence +0.027 [−0.018, 0.071] |

Scenarios: 13/13 on both. Latency over HTTP, p50: BM25 only 3.2 vs 3.3 ms; e5 10.1 vs 9.7 ms (Rust slightly slower); e5 + graph + re-ranker 210 vs 236 ms (8 model threads). Memory: 16 vs 71 MB without models; with a model loaded both are bound by ONNX Runtime and the model (macOS footprint 643 vs 630 MB with e5).

**The earlier re-ranker gap was a measurement fault, now explained.** A first Rust re-ranker run differed (all evidence +0.071) because a session setting added during a memory investigation, `session.disable_prepacking`, was still in that build (it was never committed): it changes the integer matrix kernels of the quantised model and slows them. Without it, Rust and TypeScript give the same q8 scores (largest difference 4.6e−7 over 96 pairs, fp32 the same). Against the full-precision model both q8 builds differ by the same amount (mean 0.21 logits, same order in 18 of 24 queries). The pairs and the four score files are in `eval/results/rerank-parity/`; `eval/rerank-parity.ts` and the `reranker_parity` test reproduce them. The remaining small e5 differences come from the tokenizer libraries. A second fault was in the harness: after the team split for the shared/individual condition, graph links no longer crossed a scope's team sub-scopes; they do again. `http-rust-V4-prelinkfix.jsonl` and `evalhttp_rust_bge_teams-prelinkfix.ingest.json` are from before that fix (960 links, 1248 after); no table row uses them.

Re-ranker speed on this Mac, 24 pairs: Rust q8 239 ms at 4 threads, 192 ms at 8; TypeScript q8 198 ms. The Rust default is now half the cores, at most 8.
