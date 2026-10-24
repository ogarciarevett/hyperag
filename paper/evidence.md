# Evidence and reproduction index

This index describes the artifacts used by [report v2](report-v2.md). A measured value is an observation; a committed prediction file permits rescoring, but does not by itself identify the historical binary, dependency set or original dataset snapshot. Missing provenance stays missing rather than being assigned the current main commit.

## Archived evidence

| Result | Committed evidence | Scope |
| --- | --- | --- |
| Historical reference ablations L, V0 to V4 | [Scores and paired intervals](../eval/results/cross-team-scores.json), [protocol](../eval/PROTOCOL.md), `eval/results/cross-team-{L,V0,V1,V2,V3,V4}.jsonl` | Synthetic smoke suite, 128 queries, 112 answerable; not general retrieval quality |
| Behaviour scenarios | [Library and Engram](../eval/results/scenarios.json), [Rust HTTP](../eval/results/scenarios-rust.json), [TypeScript HTTP](../eval/results/scenarios-ts-http.json) | Thirteen design-specific tasks; no sampled question split; Engram 3.3.2 |
| Rust versus TypeScript over HTTP | [Rust full-profile requests](../eval/results/http-rust-V4-t8.jsonl), [TypeScript requests](../eval/results/http-ts-V4.jsonl), [Rust run metadata](../eval/results/http-rust-V4-t8.jsonl.perf.json), [TypeScript metadata](../eval/results/http-ts-V4.jsonl.perf.json) | Full profile uses e5 plus graph plus bge-reranker-base, not the shipped default. Other L/V3 HTTP files use the same naming convention. The bge quality comparisons use the TypeScript library instead. |
| Re-ranker kernel parity | [Input pairs](../eval/results/rerank-parity/pairs.json), [Rust q8](../eval/results/rerank-parity/rust-q8.json), [TypeScript q8](../eval/results/rerank-parity/ts-q8.json) | 96 fixed pairs; agreement between implementations is not benchmark answer accuracy |
| SciFact BM25 | [TREC predictions](../eval/public/results/scifact.hyperag.bm25.run), [run metadata](../eval/public/results/scifact.hyperag.bm25.meta.json), [scores](../eval/public/results/beir-scores.json) | Reference adapter version 0.1.0; 300 official test queries, no sampling; graph off |
| Temporal dev check | [Aggregate and artifact hashes](../eval/results/temporal-dev.json), [frozen split](../eval/public/splits/lme-s-dev.json), [method](../docs/temporal.md) | Exploratory, graph off, retrieval-only; seed 42; original source commit unknown |
| Portable round trip | [Result and source commit](../eval/results/roundtrip.json), [fixture](../eval/roundtrip.ts), [format](../docs/export.md) | Operational behaviour, not retrieval-quality evidence: 52 same-model responses, 12 histories after switching to BM25/no graph, re-import and vector-population checks |

The container start-time and 1.85 GiB observation has no committed log. Earlier physical-footprint numbers are also unarchived and must not be substituted for the committed sampled RSS peaks.

## Model and statistical settings

| Item | Recorded or declared setting | Provenance limit |
| --- | --- | --- |
| bge-small reference path | fastembed 0.9.0; Qdrant/bge-small-en-v1.5-onnx-Q revision `aa8f8b060edb00e03bfdd08813a2949946c8ba55`; 384 dimensions | Declared by [the reference runner](../eval/run-suite.ts); ingestion metadata does not independently archive the model files. The Rust HTTP runner's bge spec is not revision-pinned. |
| e5 | Xenova/multilingual-e5-small revision `761b726dd34fb83930e26aab4e9ac3899aa1fa78`, q8, mean pooling, normalized vectors | Declared by [the embedder](../src/adapters/embedders.ts) and HTTP runner; not an exact historical source-commit receipt |
| Historical re-ranker | Xenova/bge-reranker-base revision `280bcc27a84e0b898c251e06fddb25171bd9b101`, q8 | Distinct from the shipped bge-reranker-v2-m3 default |
| Fusion and thresholds | RRF k=60; 24 candidates per channel; rerank threshold 0, vector threshold 0.82 | Fixed three-document probe described in the protocol; no claim of calibrated abstention |
| Cross-team paired intervals | Seed 20261021, 10,000 resamples, query as resampling unit, 95% intervals, first recorded quality attempt | Stored in every paired entry of cross-team-scores.json; unadjusted for multiple comparisons |
| Dev/reporting split | Seed 42, 100 reporting questions, 398 dev questions, two overlap exclusions; source SHA-256 stored in split | Full 500 includes dev; LoCoMo informed date-rule design and is not fully held out for the temporal channel |
| Historical software | Setup reports Bun 1.4.3, Rust 1.99, Postgres 18.6, Neo4j 5.26.28, and fastembed 0.9.0 where used | Package locks describe this checkout; they cannot prove the complete dependency state of an unrecorded historical checkout |

## Recompute without new retrieval

The SciFact result can be rescored from the committed predictions with the official BEIR test qrels. Install pytrec_eval-terrier 0.5.7 in a separate environment, set `BEIR_DIR` to the downloaded dataset root, then run:

```sh
python3 -I eval/public/score_beir.py --data "$BEIR_DIR" eval/public/results/scifact.hyperag.bm25.run
```

This reproduces nDCG@10 0.6595936919744444, recall@100 0.8825555555555555 and MAP@100 0.6182035068815604. The original scorer version was not recorded; 0.5.7 is the rescore environment, not a retrospective claim about it. The command rewrites beir-scores.json in the run directory. To retain scores for multiple BEIR sets, pass every run file in one invocation.

The cross-team scorer requires the original external artifacts and sealed baseline results:

```sh
python3 -I eval/score.py --artifacts "$BASELINE_ARTIFACTS" --baseline-results "$BASELINE_RESULTS" --suite cross-team --runs eval/results/cross-team-L.jsonl eval/results/cross-team-V0.jsonl eval/results/cross-team-V1.jsonl eval/results/cross-team-V2.jsonl eval/results/cross-team-V3.jsonl eval/results/cross-team-V4.jsonl --json-out /tmp/cross-team-rescore.json
```

The external scorer's source digest was not archived, so this is a recipe for a rescore with supplied artifacts, not a guarantee that arbitrary current scorer code reproduces the historical intervals. A new retrieval run additionally needs the original corpus and models; [run-suite.ts](../eval/run-suite.ts) requires `--variant`, `--data` and `--out`.

README rendering checks need only the standard Python library:

```sh
python3 -I eval/public/test_readme_table.py
python3 -I eval/public/readme_table.py
```

The renderer admits memory scores only when positive question counts match the expected count and the score is finite in [0,1]. Missing or partial observations remain N/D. LoCoMo reports token F1 for categories 1 to 4; other memory columns report judge accuracy. Generator/judge model revisions must accompany any future answer-quality table.

## Before treating new rows as publication evidence

Archive the exact source commit and dirty-state check, lockfiles and runtime versions, corpus/query/qrels hashes, model revisions and file digests, generator/judge prompts and revisions where used, question IDs and per-question outcomes, seeds, exclusions, and failed-call counts. Preserve unsuccessful and partial runs as such. Do not populate a default-template row with graph-off measurements. The ongoing competitors evaluation is incorporated only after those run-specific records are checked.
