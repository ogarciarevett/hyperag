# Hyperag: Evaluating Hybrid Retrieval for Agent Memory on Resource-Constrained Hardware

Technical report, version 2 (draft, updated 24 October 2026). Version 1 is the PDF technical report of 23 October 2026; its LaTeX source was not found, so this version is written in Markdown.

## What changed since version 1

Version 1 could only re-analyse two earlier reports. Its claim/evidence matrix had 60 claims: 40 supported, 7 partial, 8 pending, 5 removed. Version 2 adds new runs made with code in this repository:

- Hyperag now runs its full design: Postgres BM25 and pgvector with reciprocal rank fusion, a Neo4j relationship graph, a cross-encoder re-ranker, versions with valid time, grants per scope, deletion and provenance. Version 1 measured only the BM25 + vector path.
- A Rust build of the same design (`crates/hyperag`) is checked against the TypeScript reference with the same harness.
- Engram is run the way agents on the measuring machine use it, through its MCP server, next to the restricted profile that version 1 measured.
- A shared public-benchmark runner (`eval/public/`) runs BEIR with the official test split and scorer; the ongoing LoCoMo, LongMemEval and MSC evaluation is separate from the results archived in this report.

Reading conventions are those of version 1. Every value is labelled **measured** (an observed run), **recomputed** (a re-derivation of a reported value) or **exploratory** (not in a registered protocol). N/D means not observed; it is never zero. [Evidence and reproduction limits](evidence.md) distinguish archived records, unarchived observations and missing provenance.

## Setup

- Machine: Mac Studio (Apple silicon), OrbStack, Postgres 18.6 with pgvector, Neo4j 5.26.28 Community, Bun 1.4.3, Rust 1.99. All models on CPU.
- Models: BAAI/bge-small-en-v1.5 (fastembed 0.9.0 ONNX, as in version 1), multilingual-e5-small (Xenova q8, revision 761b726), bge-reranker-base (Xenova q8, revision 280bcc2).
- Synthetic cross-team battery: the frozen suite of version 1 (1,536 documents, 128 queries, 112 with evidence, four scopes, English and Spanish), scored with the baseline's own `evaluate.py`, unchanged. **It is an internal smoke test.** Results on it are not claims about general quality.
- Public: BEIR SciFact, NFCorpus, FiQA-2018, ArguAna, SciDocs and TREC-COVID downloaded from the BEIR distribution; official test qrels; pytrec_eval as in the BEIR toolkit.
- Protocol: `eval/PROTOCOL.md`, fixed before the first run, with its recorded deviations. Nothing was tuned on the held-out queries.

## Results

### Reproduction of the version 1 baseline (measured)

The Hyperag baseline profile (BM25 + bge-small + RRF, no graph, no re-ranker) reproduces the sealed baseline (agents-runtime v4) on the smoke suite within 0.001: nDCG@10 0.717, recall@10 0.787, all-evidence@10 0.723, cross-team handoff 6/16 (paired bootstrap difference 0.000, interval −0.001 to 0.000). Two adapter faults had to be fixed first: BGE needs CLS pooling, and pgvector's HNSW default `ef_search = 40` returned only 92% of the exact top 24 under a scope filter (now at least 100, which returns 100%, measured against exact search without labels).

Reproduce: `bun eval/run-suite.ts --variant V0 --data <suite>` then `python3 -I eval/score.py ...` (see `eval/RESULTS.md`).

### Components one at a time (measured, smoke suite)

These are historical TypeScript reference profiles L and V0 to V4, not the shipped default template. Model revisions and artifact links are in [the evidence index](evidence.md). Paired intervals use 10,000 query resamples, seed 20261021, and are not adjusted for the multiple ablations.

| Variant | nDCG@10 | all evidence@10 | handoff | Δ all evidence vs baseline, 95% CI |
| --- | --- | --- | --- | --- |
| BM25 only | 0.709 | 0.607 | 8/16 | −0.116 [−0.205, −0.036] |
| Baseline profile (BM25 + bge + RRF) | 0.717 | 0.723 | 6/16 | 0.000 |
| + graph | 0.700 | 0.812 | 9/16 | +0.089 [0.027, 0.161] |
| + re-ranker | 0.692 | 0.705 | 3/16 | −0.018 [−0.080, 0.045] |
| e5 instead of bge | 0.749 | 0.545 | 0/16 | −0.179 [−0.268, −0.089] |
| e5 + graph + re-ranker | 0.789 | 0.786 | 8/16 | +0.062 [−0.018, 0.152] |

nDCG@10 of the full profile is +0.072 over the baseline with interval [−0.003, 0.150]: not significant at 95%.

### Behaviour scenarios (measured)

Thirteen tasks from the spec, the same task list for every system (`eval/scenarios.ts`): retention across a restart in English and Spanish, a handoff with its evidence, a correction and the correction after a restart, a concurrent update, history of a correction, a restricted scope, revocation, deletion, abstention, valid time, citations.

| System | Passed |
| --- | --- |
| Hyperag, TypeScript library | 13/13 |
| Hyperag, TypeScript service over HTTP | 13/13 |
| Hyperag, Rust service over HTTP (also in CI on Linux) | 13/13 |
| Engram 3.3.2 through its MCP server, default search | 3/13 |
| Engram 3.3.2 through its MCP server, `match_mode=any` | 8/13 |

Engram's failures with `match_mode=any`: no conflict on concurrent updates, any caller can read any project, no revocation, no abstention signal, no valid time. With its default search it also finds nothing for natural-language questions, because every word must match.

### Engram, restricted versus real (measured)

On the smoke suite, Engram 3.3.2 through its HTTP search reached nDCG@10 0.000 with its default search and 0.163 with `match_mode=any`, close to version 1's restricted profile (0.170). Version 1's number therefore holds for the same binary and search modes through its HTTP API, not only for the restricted profile; the scenario table above is the more relevant comparison for agent memory.

### Rust build against the TypeScript reference (measured)

Rust is measured over HTTP. The TypeScript comparison uses HTTP for BM25, e5 and the full profile, but the in-process library for bge profiles because that service loads only e5. Quality uses the same query records; transport is therefore not uniform across every quality pair. Each pair below is Rust then TypeScript. Every variant is equal or within noise: identical with BM25 only, the baseline profile and the graph; re-ranker alone Δ nDCG +0.001 [0.000, 0.002]; e5 +0.006 [−0.003, 0.019]; e5 + graph + re-ranker Δ nDCG −0.005 [−0.026, 0.016], all evidence +0.027 [−0.018, 0.071]. Scenarios 13/13 on both. Latency p50: BM25 only 3.2 vs 3.3 ms; e5 10.1 vs 9.7 ms (Rust slightly slower); full profile 210 vs 236 ms (8 model threads). Recorded peak process RSS is 15.9 vs 71.3 MB without models, 986.0 vs 800.1 MB with e5, and 1812.3 vs 1510.0 MB for the full profile (decimal MB). These are sampled run peaks, not physical-footprint measurements or proof that Rust always uses less memory. The earlier 643 vs 630 MB physical-footprint observation has no committed log and is not used as an archived memory result.

An earlier re-ranker gap (+0.071 all evidence) was a measurement fault: the measured Rust build carried a temporary `session.disable_prepacking` setting, which changes the quantised integer kernels. Without it, both builds give the same q8 scores (max difference 4.6e−7 over 96 pairs), and both differ from the full-precision model by the same amount (mean 0.21 logits). The pairs and scores are in `eval/results/rerank-parity/`.

### Container stack (measured once by hand; no committed log)

`docker compose --profile service up -d` on OrbStack (Apple silicon): the first start downloaded the default template's models (EmbeddingGemma 2 q8 and bge-reranker-v2-m3 q8, 891 MB, each large file checked against the hub's SHA-256) into a named volume and was healthy in about 25 s; the service used 1.85 GiB. The README quick start (token, grants, ingest, cited context through BM25, vector, graph and re-ranker; MCP at `/mcp`) worked as written.

### Harness arm (measured, by the competitors evaluation)

One trial per side, 13 sealed items per task. With memory fixed to Hyperag's saved top 10 (e5 only, no graph, no re-ranker) and only the agent harness changed, both harnesses tied on a handoff task (sealed score 0.736) and an engineering task (0.795). Every final version reached the ceiling of what the retrieved evidence allows, so the remaining loss is evidence the memory did not return: for that memory configuration, retrieval, not the harness, is the bottleneck on these tasks; the full profile was not tried in this arm (`eval/harness2/README.md`).

### Public benchmarks (measured, in progress)

| Benchmark | Profile | nDCG@10 | Published reference |
| --- | --- | --- | --- |
| BEIR SciFact (300 test queries, no query sampling) | Hyperag reference 0.1.0, BM25 only, graph off | 0.660 | BM25 0.665 (BEIR paper) |

The SciFact run and metadata are [archived](evidence.md), and a rescore with pytrec_eval-terrier 0.5.7 reproduces its three committed quality metrics. The metadata does not record the original scorer version or source commit. The dense and re-ranker results from the completed October 23–24 window are archived separately in [the verified window report](../eval/public/notes/window-2026-10-24.md), together with the competitor memory rows and scoring limits. The memory runners cover LoCoMo (1,986 questions), LongMemEval S (500), oracle (500) and MSC/DMR (500), with one open model as generator and judge for every system (`eval/public/ITEMS.md`).

Graph rows on the memory benchmarks are exploratory. The window completed e5 + re-ranker + graph on LoCoMo (F1 0.4655, versus 0.4543 without graph) and oracle-haystack top-10 retrieval (accuracy 0.802, versus 0.806 without graph). These are descriptive results without a significance claim; the default-template graph profile remains unmeasured. These sets carry no relations, so the `+graph` profiles build edges by two exact rules, with no model: a turn links to the previous turn of its session (`NEXT_TURN`) and to the latest earlier turn of the same conversation that names the same entity (`SAME_ENTITY`; a capitalised word of three or more letters that follows a lowercase letter and a space, with an optional comma between them, compared exactly, minus speaker names, months and weekdays). Both rules link successive mentions, so a common name does not form a star; retrieval expands two hops. The rule was fixed before any run and is checked on a stub (`eval/public/graph-stub-check.ts`). A graph gain on these sets would measure these rules, not the value of a graph built by extraction. BEIR documents carry no relations; enabling graph on a corpus adds no edges or relationship contribution.

## Answers to the research questions

- **RQ1 (hybrid versus components): partially answered.** On the smoke suite, fusion beats BM25 alone on evidence (+0.116 all-evidence) and ties on nDCG. Vector-only is pending.
- **RQ2 (graph and re-ranker): answered on the smoke suite.** The graph raises all-evidence by 0.089 (significant) and handoffs from 6/16 to 9/16, at about 2 ms per query. The re-ranker alone does not help ranking (−0.025 nDCG, not significant), costs about 200 ms per query on CPU, and is the only working abstention signal (69 to 75% of probes, with 32 to 35% false abstention).
- **RQ3 (evidence and agent tasks): unanswered.** No answer-generation benchmark result is incorporated in this report yet. The harness arm answers a different question: the harness effect with memory fixed.
- **RQ4 (individual versus shared memory): pending.** The condition is implemented (`--memory individual|shared` in `eval/run-suite-http.ts`) and waits for a run.
- **RQ5 (embeddings): partially answered.** Multilingual e5 raises Spanish nDCG from 0.632 to 0.719 but loses multi-evidence recall; with the graph and re-ranker it is above the baseline on both. EmbeddingGemma 300m (768-d) is implemented and waits for a run.
- **RQ6 (corrections, permissions, provenance): answered as behaviour.** Corrections, corrections after a restart, concurrent-update conflicts, revocation, deletion and provenance all pass as scenario tasks, and the Rust service passes them in CI.

## Claim/evidence matrix: changes

Only claims whose status changes are listed; the rest keep their version 1 status.

| ID | Claim | v1 | v2 | New evidence |
| --- | --- | --- | --- | --- |
| C22 | Hyperag reaches state-of-the-art quality on BEIR/SciFact | removed | removed | SciFact now observed (BM25 0.660), but at the published BM25 level, not above it |
| C24 | Hyperag nDCG@10 0.802 EN / 0.632 ES | partial | supported (measured) | reproduced within 0.001 (0.801 / 0.632); multilingual embeddings change ES to 0.719 (measured) |
| C44 | Fusion adds quality over BM25-only and vector-only | pending | partial (smoke suite only) | over BM25 only: +0.116 all-evidence, significant; vector-only pending |
| C45 | Graph and re-ranker improve quality, and at what cost | pending | partial (smoke suite only): graph helps evidence; re-ranker shows no ranking gain | +0.089 all-evidence [0.027, 0.161]; re-ranker −0.025 nDCG n.s., ~200 ms/query |
| C46 | Shared memory outperforms individual memory | pending | pending | condition implemented, not yet run |
| C47 | Effect of the embedding model | pending | partial (smoke suite only) | bge vs e5 measured (ES +0.087 nDCG, all-evidence −0.179); EmbeddingGemma pending |
| C48 | Corrections and temporal invalidation work across sessions | pending | supported (behaviour) | scenario tasks pass on three builds, including after a restart |
| C49 | Permissions hold across identities and agents | pending | supported (behaviour) | token-verified principals, scope grants, revocation; Engram fails the same tasks |
| C50 | Provenance of results | partial | supported (behaviour) | `explain` and citations with memory id, version, source, author |
| C55 | Report A's Memvid hybrid equals report B's integration v2 | pending | pending | not addressed |
| C42 | Retrieved evidence improves agent task success | pending | pending | needs the open-model endpoint |

## Temporal channel dev check (exploratory)

A fixed-rule temporal channel is implemented in Rust and disabled by default. It resolves dates in candidates against their source timestamps and question dates against a separate `anchor` option, which does not filter memories. A temporal candidate list enters reciprocal rank fusion with k = 60 and weight 1; resolved dates are added to citations. The method and evaluation boundary are in [docs/temporal.md](../docs/temporal.md).

The frozen LongMemEval-S split uses seed 42: the existing n=100 subset is reserved for reporting, and the remaining questions minus two with overlapping answer sessions form dev (n=398). The dataset hash is committed with the split. The full 500-question row includes dev and must be labelled not held out; LoCoMo informed the date-rule design and is not fully held out for this channel.

A retrieval-only dev check with BM25, no models and graph off (not the default template) returned at least one answer-session turn in the top 10 for 386/398 questions (0.970), versus 385/398 (0.967) with the temporal channel. There were zero gained hits and one lost hit; temporal-reasoning questions were unchanged at 99/106. These observations show no retrieval gain, and say nothing about answer accuracy. [Recomputed counts and artifact hashes](../eval/results/temporal-dev.json) are committed; the original run did not record its source commit. Stricter dev metrics and graph-on default-template ablations remain pending. No held-out quality claim rests on this check.

## Defaults (chosen, not yet measured on public benchmarks)

The default template is EmbeddingGemma 2 (q8, 768 dimensions, Apache-2.0) with bge-reranker-v2-m3 (q8, Apache-2.0), Postgres with pgvector, and the Neo4j graph on. The graph store is a slot that another graph database can fill; running without it is possible but is not the default template. The default template's rows (graph on) run in the next GPU window. Graph-off rows from the completed window are labelled "graph off (not the default template)"; the exploratory e5 graph rows use the rule above; on BEIR, which carries no relations, the graph has no edges to expand.  `hyperag models recommend` picks models per slot from the free resources, using parameters x bytes per parameter x 1.2 plus the batch working set.

## Limitations

- The smoke suite is synthetic and templated; no claim about general quality rests on it.
- One machine, CPU only; latency includes in-process or loopback HTTP, never a remote network. Quality comparisons do not all use the same transport, and recorded model-loaded Rust RSS peaks are higher than their TypeScript counterparts.
- Query-bootstrap intervals describe this fixed smoke suite, not variation across datasets, machines or training runs. Historical source commits and some scorer/runtime digests were not recorded; current source files are reproduction guidance rather than proof of those old checkouts.
- The 13 scenario tasks come from Hyperag's own design, so 13/13 against 3/13 or 8/13 measures coverage of that design (several Engram failures are features Engram does not have by design), not a general ranking.
- The version 1 matrix counts and the sealed baseline artifacts are outside this repository; a reader cannot check them from it alone.
- The full competitors evaluation is archived in the separate window report linked above; only selected graph-rule observations are summarized here, labelled exploratory.

## Reproduce

Start with [the evidence index](evidence.md) for artifact links, model revisions, seeds, exact rescore commands and remaining provenance gaps. The baseline artifacts (sealed runs and scorer) are read from a folder outside the repository. The index separates a rescore of saved predictions from a new retrieval run.

## Benchmark references

- Thakur et al. (2021), [BEIR: A Heterogenous Benchmark for Zero-shot Evaluation of Information Retrieval Models](https://arxiv.org/abs/2104.08663v4). The published BM25 figures are reference values from that evaluation, not runs on this machine.
- Wu et al. (2025), [LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory](https://arxiv.org/abs/2410.10813v2).
- Maharana et al. (2024), [Evaluating Very Long-Term Conversational Memory of LLM Agents](https://arxiv.org/abs/2402.17753), introducing LoCoMo.
