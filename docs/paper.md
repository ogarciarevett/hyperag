# Hyperag: public-benchmark evaluation across two recorded windows

Technical note, 25 October 2026. Evidence baseline: [`e50125f`](https://github.com/ogarciarevett/hyperag/commit/e50125fee17bf6933a00b91e77537be56d31ce52), the merged second-window result snapshot. This note concerns the measured TypeScript reference adapter and the native comparison paths recorded with it.

The default-template configuration completed LoCoMo with **0.4913 F1 (W2)** and DMR with **411/500 = 0.8220 accuracy (W2)**. The archive contains 29 complete memory observations and 16 BEIR retrieval observations across the two windows. Unresolved rows remain N/D. These observations support a bounded account of the measured configurations, without confidence intervals or a general superiority claim.

## Cohorts and evidence

- **W1 — October 23–24:** the [first-window manifest](../eval/public/results/window-2026-10-24.json) and [report](../eval/public/notes/window-2026-10-24.md), together with the previously archived October 23 BEIR baseline rows in [the combined scorer output](../eval/public/results/beir-scores.json).
- **W2 — October 24–25:** the [second-window manifest](../eval/public/results/window-2026-10-25.json), [scorer output](../eval/public/results/window-2026-10-25-scorer.txt), [source/completion checks](../eval/public/results/window-2026-10-25-validation.txt) and [report](../eval/public/notes/window-2026-10-25.md).

Every memory score below names its cohort in the cell. Every BEIR row has a window column. W1 baseline rows were archived before W2; they were not rerun alongside its results. All competitor comparisons mean **“on these benchmarks, in this run”**. They compare the recorded native paths and settings, with incomplete coverage across datasets and configurations.

## Method

Hyperag stores scoped memory records with provenance and versions. The measured reference adapter retrieves through Postgres BM25 and pgvector, fuses ranked channels with reciprocal rank fusion, and optionally reranks or expands a Neo4j relationship graph. Scope grants control retrieval. These benchmark scores measure retrieval-supported answers or ranked retrieval; they do not independently establish all operational, security or concurrency properties of the installed service. [Architecture](architecture.md) and the [historical report](../paper/report-v2.md) document those separate aspects. Its older public-benchmark pending statuses are superseded by the result manifests used here.

The **default template** here is `egemma2+bgev2m3+graph`: EmbeddingGemma 2 q8, bge-reranker-v2-m3 q8 and graph expansion. `e5` selects multilingual-e5-small q8; `rerank` selects the historical bge-reranker-base, while `bgev2m3` selects the newer reranker. `nemotron` and `nvrerank` select the recorded GPU embedding and reranking checkpoints. Only profiles ending in `+graph` enable graph in these memory results. The exact [adapter](../eval/public/systems/hyperag.ts) and per-run metadata define the measured paths.

Conversation graph edges use two deterministic rules: link a turn to its previous turn in the session, and link successive exact entity mentions across the conversation. The implementation excludes speaker names and calendar words from that entity heuristic; expansion is limited to two hops. [The rules](../eval/public/conversation-graph.ts) are explicit. The reported BEIR rows are graph off; their documents provide no conversation relations.

### Tasks and scoring

- **LoCoMo:** all 1,986 questions are answered; the main score is Porter-stemmed token F1 over the 1,540 category 1–4 questions. Category-specific answer handling and the seeded adversarial-option order are preserved by [the runner](../eval/public/memory.ts) and [scorer](../eval/public/score_locomo.py). The benchmark is described by [Maharana et al.](https://arxiv.org/abs/2402.17753).
- **DMR:** the 500 MSC-Self-Instruct questions use the common answer/judge harness. The archive stores each model answer and its recorded boolean judgment. Offline accuracy recomputation aggregates those judgments; it does not independently rejudge them.
- **LongMemEval:** the oracle row searches the oracle haystack and retrieves the top 10; it is not upstream oracle-session retrieval. S100 is the frozen reporting subset of 100 questions. No full 500-question LME-S run completed. The upstream task is described by [Wu et al.](https://arxiv.org/abs/2410.10813v2); the repository's [split and temporal protocol](temporal.md) records its additional restrictions.
- **BEIR:** use all official test qrels, retrieve up to 100 documents, and score nDCG@10, Recall@100 and MAP@100 through [the existing pytrec_eval scorer](../eval/public/score_beir.py). Identical query/document IDs are excluded as in BEIR's evaluator. The [BEIR paper](https://arxiv.org/abs/2104.08663v4) and [official repository](https://github.com/beir-cellar/beir) provide the upstream benchmark and data distribution.

Missing or incomplete runs are N/D, never invented zero scores. Native failed searches within a completed run retain the common harness's scoring rule. In W1, Memvid has 5 such LoCoMo failures, 2 oracle failures and 25 NFCorpus query failures; none were selectively removed. Actual zero-valued Engram rows remain zero-valued observations.

## Setup and provenance

The operator-declared setup used shared H200 inference for the GPU services and local CPU embedding/reranking where the profile required it, with Postgres 18/pgvector and Neo4j 5.26.28 Community. This is a setup description, not a hardware-performance comparison. W2 Hyperag memory metadata records two workers. The memory harness uses the serving alias `llama-3.3-70b-instruct` for answers and judgments at temperature zero; LLM-using extraction paths also use the common endpoint. Retrieval configurations differ by the named subject profile.

W2 metadata identifies EmbeddingGemma 2 q8 revision `daa72c51243991dfcaf9f9137d2c573d8f7790c0`, multilingual-e5-small q8 revision `761b726dd34fb83930e26aab4e9ac3899aa1fa78`, Nemotron embedding revision `781e779ee4ca9c2d2a4f38321993556baeab49a1`, and Nemotron reranking revision `f88f8d9104726747791283ab156d2fcdb29e90a7`. Reranker identifiers and preprocessing are recorded in each metadata file. Preserve abbreviated historical identifiers as recorded rather than assigning missing provenance retroactively.

The corrected W2 rescore environment is Python 3.12, nltk 3.9.2, regex 2025.11.3, NumPy 2.5.4 and pytrec-eval-terrier 0.5.7. The initial missing-regex invocation produced no score; complete outputs were subsequently rescored in the corrected environment. Source IDs, questions, gold answers, categories, counts, official BEIR queries and corpus IDs were checked for the W2 results; output hashes are in its manifest.

The public quality metadata reports reference-adapter version 0.1.0. Installed Rust release validation is a separate evidence category. Current source, locks and CI cannot establish the exact historical binary or full dependency state of an unrecorded run. The published records do not supply a complete generator-weight digest, hardware attestation or every original dataset/runtime digest; those provenance gaps remain open.

Letta's BEIR entries use its archival-memory semantic search; its LLM is not called on that path. Engram and Memvid retain their declared lexical/native modes. These path choices matter when interpreting the competitor rows; they do not define a ranking of every feature offered by those products.

## Results

### Memory observations

This comparison is **on these benchmarks, in this run**; each numerical cell explicitly names W1 or W2. Profiles with only N/D cells are retained to make planned/blocked comparisons visible.

| System | Profile | LoCoMo F1 | DMR accuracy | LME oracle-haystack accuracy | LME-S100 accuracy |
| --- | --- | --- | --- | --- | --- |
| hyperag | `egemma2+bgev2m3+graph` | 0.4913 (W2) | 0.8220 (W2) | N/D | N/D |
| hyperag | `e5+bgev2m3` | 0.4635 (W2) | N/D | N/D | N/D |
| hyperag | `e5+rerank` | 0.4543 (W1) | 0.7700 (W1) | 0.8060 (W1) | 0.7700 (W1) |
| hyperag | `e5+rerank+graph` | 0.4655 (W1) | N/D | 0.8020 (W1) | N/D |
| hyperag | `egemma2+nvrerank` | N/D | N/D | N/D | N/D |
| hyperag | `egemma2+rerank` | 0.4764 (W1) | 0.7620 (W1) | N/D | N/D |
| hyperag | `nemotron+nvrerank` | 0.5101 (W1) | 0.8240 (W1) | 0.8520 (W1) | N/D |
| hyperag | `nemotron-int8` | N/D | N/D | N/D | N/D |
| cognee | `default` | 0.4111 (W1) | N/D | N/D | N/D |
| engram | `any` | 0.1066 (W1) | 0.0980 (W1) | 0.3860 (W1) | N/D |
| engram | `default` | 0.0149 (W1) | 0.0300 (W1) | 0.0760 (W1) | 0.0900 (W1) |
| graphiti | `default` | 0.1510 (W1) | N/D | N/D | N/D |
| hindsight | `default` | 0.3538 (W1) | N/D | N/D | N/D |
| langmem | `default` | N/D | N/D | N/D | N/D |
| letta | `default` | 0.4108 (W1) | N/D | N/D | N/D |
| mem0 | `default` | 0.3860 (W1) | N/D | N/D | N/D |
| memvid | `default` | 0.0174 (W1) | 0.0280 (W1) | 0.1080 (W1) | N/D |

The W2 default-template DMR result is 411 correct judgments from 500 questions. Its LoCoMo result and the W2 `e5+bgev2m3` result both complete the full question set with zero failed searches. Graph-on/default and graph-off variants also change model settings, and some belong to different windows; their differences are not an isolated graph ablation.

![LoCoMo observations](benchmarks/locomo.svg)

![DMR observations](benchmarks/dmr.svg)

### BEIR observations

These are all 16 completed retrieval observations in the combined score artifact. The comparison is **on these benchmarks, in this run**, with the recorded window named in every row.

| Window | Dataset | System / profile | Queries | nDCG@10 | Recall@100 | MAP@100 |
| --- | --- | --- | --- | --- | --- | --- |
| W1 | arguana | engram / `any` | 1406 | 0.0000 | 0.0000 | 0.0000 |
| W1 | arguana | engram / `default` | 1406 | 0.0000 | 0.0000 | 0.0000 |
| W2 | arguana | hyperag / `nemotron` | 1406 | 0.5004 | 0.9893 | 0.4225 |
| W1 | arguana | letta / `default` | 1406 | 0.5957 | 0.9836 | 0.5220 |
| W1 | nfcorpus | engram / `any` | 323 | 0.2384 | 0.1261 | 0.0987 |
| W1 | nfcorpus | engram / `default` | 323 | 0.1864 | 0.0897 | 0.0793 |
| W2 | nfcorpus | hyperag / `nemotron` | 323 | 0.3724 | 0.3441 | 0.1823 |
| W2 | nfcorpus | hyperag / `nemotron+nvrerank` | 323 | 0.4481 | 0.3441 | 0.2202 |
| W1 | nfcorpus | letta / `default` | 323 | 0.3371 | 0.3058 | 0.1568 |
| W1 | nfcorpus | memvid / `default` | 323 | 0.1389 | 0.0566 | 0.0520 |
| W1 | scifact | engram / `any` | 300 | 0.0679 | 0.0883 | 0.0623 |
| W1 | scifact | engram / `default` | 300 | 0.0067 | 0.0067 | 0.0067 |
| W1 | scifact | hyperag / `bm25` | 300 | 0.6596 | 0.8826 | 0.6182 |
| W1 | scifact | hyperag / `nemotron` | 300 | 0.7505 | 0.9767 | 0.7117 |
| W1 | scifact | hyperag / `nemotron+nvrerank` | 300 | 0.8388 | 0.9767 | 0.7998 |
| W1 | scifact | letta / `default` | 300 | 0.7201 | 0.9533 | 0.6817 |

On these benchmarks, in this run, NFCorpus has Hyperag `nemotron` **0.3724 (W2)** and `nemotron+nvrerank` **0.4481 (W2)**, alongside Letta's archival path **0.3371 (W1)**. ArguAna has Hyperag `nemotron` **0.5004 (W2)** and Letta's archival path **0.5957 (W1)**. Configuration and cohort differences prevent attributing these observations to one component or generalizing them to other workloads.

![NFCorpus observations](benchmarks/nfcorpus.svg)

![ArguAna observations](benchmarks/arguana.svg)

SciDocs, FiQA and TREC-COVID have no completed rows in this public score artifact. Their planned rows remain N/D in the status appendix.

## Limits

- These public-window observations have no repeated-run uncertainty estimates or confidence intervals. Incomplete coverage and different native paths limit cross-system comparisons.
- Memory concurrency, machine pauses and scheduling make recorded latency and wall time unsuitable for performance rankings. NFCorpus reranking reused its ingested corpus; that ingestion time is a warm reuse measurement.
- [Transport recovery changed within W1](../eval/public/notes/transport-retry.md); earlier rows were not retrospectively relabelled or rerun. SDK retries and common availability recovery are separate layers. W2 repaired local connection recovery and pause accounting, and graph-on dispatch waited for an HTTP listener and a successful query. These operational differences do not establish a causal quality improvement.
- Graphiti's W1 native output request was capped to the shared endpoint ceiling documented in the first-window report. Native competitor defaults, token budgets and retrieval modes must accompany their results.
- LoCoMo-style dialogue informed exploratory temporal rules. The full LME-S includes development questions, and oracle haystack is distinct from oracle-session retrieval. The public TypeScript rows are not evidence for a held-out Rust temporal-channel improvement; see [the temporal protocol](temporal.md).
- Failed and partial attempts remain archived and unscored. Complete default-template retries supersede earlier failed attempts; preempted or budget-skipped variants do not receive partial scores. This note adds no new inference, statistical reanalysis, product acceptance claim or submission claim.

## Reproduce without new inference

Use the frozen result snapshot at the baseline commit above. Original dataset acquisition references are the [LoCoMo repository](https://github.com/snap-research/locomo), [LongMemEval repository](https://github.com/xiaowu0162/LongMemEval) and [BEIR repository](https://github.com/beir-cellar/beir). Supply the official BEIR dataset root with `corpus.jsonl`, `queries.jsonl` and `qrels/test.tsv` for each used dataset. A newly downloaded archive is not proof of the unrecorded historical dataset bytes; check the recorded query IDs/counts and available hashes.

With Python 3.12 available as `python3.12`, create an isolated scoring environment using the repository's current trust-tooling policy. These are setup instructions; they do not claim that every historical run used today's downloader versions.

```sh
python3.12 -m venv .venv-paper
sh scripts/bootstrap-python-ca.sh .venv-paper/bin/python
.venv-paper/bin/uv pip install --python .venv-paper/bin/python --constraint eval/public/python-ca-constraints.txt \
  nltk==3.9.2 regex==2025.11.3 numpy==2.5.4 pytrec-eval-terrier==0.5.7
```

Run from the repository root. Set `BEIR_ROOT` to the supplied official dataset root, then copy the frozen outputs to a temporary directory: the scorers update metadata/output files, so rescoring should not rewrite the archive.

```sh
RESCORE_DIR="$(mktemp -d)"
cp -R eval/public/results "$RESCORE_DIR/results"
.venv-paper/bin/python -I eval/public/score_locomo.py "$RESCORE_DIR"/results/memory/locomo.*.jsonl
.venv-paper/bin/python -I eval/public/score_beir.py --data "$BEIR_ROOT" \
  "$RESCORE_DIR"/results/*.run "$RESCORE_DIR"/results/beir/*.run
python3 -I eval/public/benchmark_charts.py --results "$RESCORE_DIR/results" --out "$RESCORE_DIR/figures"
python3 -I eval/public/benchmark_charts.py --check
python3 -I eval/public/test_benchmark_charts.py
python3 -I eval/public/test_readme_table.py
```

Expected W2 LoCoMo scores are **0.4635 (`e5+bgev2m3`)** and **0.4913 (default)**. The BEIR scorer should reproduce all rows above; default W2 DMR accuracy recomputes as **411/500 = 0.8220** from the saved judgments. These are offline checks of recorded outputs. New retrieval/generation runs can differ and require their own run-specific provenance and authorization.

### Number-to-source map

Memory cells read `/memory/*/score` in the linked W1/W2 manifests, identified by benchmark, system and profile; each manifest links its raw question rows and output hash. BEIR cells read `ndcg@10`, `recall@100` and `map@100` in the combined scorer JSON, keyed by dataset, system and profile. W2 membership is established by its manifest; remaining BEIR rows are the W1 archive. Completion and failure counts come from the matching metadata and retained raw rows. The appendix below reads only non-complete entries from the final ledger. Printed metric values are rounded to four decimal places; source precision remains in the JSON artifacts.

## Status appendix: N/D observations

The final ledger has 49 tracked rows: 6 completed replacements/new results and **43 unresolved rows**, all shown below as N/D with their retained status. It is a tracked follow-up list, not a count of all historical completed benchmark observations. Exact runtime estimates, dispatch safety margin and admission reasons remain in the [machine-readable ledger](../eval/public/results/window-2026-10-25-ledger.json) and [full row-status report](../eval/public/notes/next-window.md).

| Benchmark | System / profile | Result | Retained status and reason |
| --- | --- | --- | --- |
| dmr | letta / `default` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-oracle | letta / `default` | N/D | Unrun: outside the W2 selected subset. |
| dmr | mem0 / `default` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| dmr | cognee / `default` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| dmr | hindsight / `default` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| dmr | graphiti / `default` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-oracle | mem0 / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-oracle | cognee / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-oracle | hindsight / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-oracle | graphiti / `default` | N/D | Unrun: outside the W2 selected subset. |
| dmr | hyperag / `e5+bgev2m3` | N/D | Incomplete: retained partial attempt; no completed replacement. |
| lme-oracle | hyperag / `e5+bgev2m3` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| locomo | hyperag / `egemma2+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| dmr | hyperag / `egemma2+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-oracle | hyperag / `egemma2+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-s100 | mem0 / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-s100 | cognee / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-s100 | graphiti / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-s100 | hindsight / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-s100 | hyperag / `egemma2+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-s | hyperag / `e5+rerank` | N/D | Unrun: outside the W2 selected subset. |
| lme-s | hyperag / `e5+bgev2m3` | N/D | Unrun: outside the W2 selected subset. |
| lme-s | hyperag / `egemma2+rerank` | N/D | Unrun: outside the W2 selected subset. |
| lme-s | letta / `default` | N/D | Unrun: outside the W2 selected subset. |
| locomo | hyperag / `nemotron-int8` | N/D | Blocked: no served INT8 embedding checkpoint. |
| dmr | hyperag / `nemotron-int8` | N/D | Blocked: no served INT8 embedding checkpoint. |
| lme-oracle | hyperag / `nemotron-int8` | N/D | Blocked: no served INT8 embedding checkpoint. |
| lme-s100 | hyperag / `nemotron-int8` | N/D | Blocked: no served INT8 embedding checkpoint. |
| lme-s100 | hyperag / `nemotron+nvrerank` | N/D | Failed earlier attempt; no completed replacement after W2. |
| beir:arguana | hyperag / `nemotron+nvrerank` | N/D | Failed earlier attempt; no completed replacement after W2. |
| beir:scidocs | hyperag / `nemotron` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| beir:scidocs | hyperag / `nemotron+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| beir:fiqa | hyperag / `nemotron` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| beir:fiqa | hyperag / `nemotron+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| beir:trec-covid | hyperag / `nemotron` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| beir:trec-covid | hyperag / `nemotron+nvrerank` | N/D | Unrun: runtime plus dispatch margin did not fit the W2 cutoff. |
| lme-s | engram / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-s | memvid / `default` | N/D | Unrun: outside the W2 selected subset. |
| lme-oracle | hyperag / `egemma2+rerank` | N/D | Incomplete: retained partial attempt; no completed replacement. |
| locomo | langmem / `default` | N/D | Blocked: native automatic-tool readiness was not established. |
| lme-oracle | langmem / `default` | N/D | Blocked: native automatic-tool readiness was not established. |
| dmr | langmem / `default` | N/D | Blocked: native automatic-tool readiness was not established. |
| lme-s100 | langmem / `default` | N/D | Blocked: native automatic-tool readiness was not established. |
