# Temporal channel

Agent memory benchmarks ask questions about time: "When did Priya renew her passport?", "How long has Tomas been learning the cello?", "Which did I buy first?". A first LoCoMo run of Hyperag (graph off, not the default template, from the competitors evaluation) gave temporal F1 0.168. This page describes a temporal channel and the rules for measuring it. The first retrieval-only dev observations are below; answer-quality and default-template runs remain pending.

## Method

1. **Dates in memories.** `crates/hyperag/src/temporal.rs` finds calendar dates and periods in a memory's text with fixed rules (no model). It handles absolute dates (`8 May, 2023`, `2023/05/20`, `July 20th, 2022`, `March 2021`, `in 2019`) and relative ones (`yesterday`, `two days ago`, `a couple of weeks ago`, `last week`, `last Friday`, `last June`). "may" and "march" count as months only with an ordinal day, "of" or a year next to them, and counts above 1,000 are ignored. Relative dates resolve against the memory's own time: its source's timestamp when there is one (`metadata.timestamp`, ISO or a written date), else `validFrom`, else `recordedAt`. Each span is a half-open day range. The spans are computed at query time from the text of the loaded candidates; nothing new is stored.
2. **Time in the question, at query time.** The same rules find spans in the question, resolved against the question's date: the `anchor` retrieve option, which filters nothing (else `asOf`, else today). `intent` labels the question: `When` (the answer is a date), `Duration` (how long, how many days), `Order` (first, last, before, after) or none.
3. **The channel.** It runs only when the question has a span or an intent; otherwise retrieval is unchanged.
   - Question with a span: the candidates that mention a time overlapping it, in their fused order (one more list for reciprocal rank fusion, next to BM25, vectors and the graph).
   - Any time intent with no span: the candidates that mention a time other than the day they were said, in their fused order. Dated evidence is what lets the answering model state a date or an order. (A source that writes its own date into each line, as LoCoMo and LongMemEval do, does not make every line count as dated.)
4. **Fusion.** The temporal list enters reciprocal rank fusion with k = 60 and weight 1, like the other lists. There is no weight fitted to any benchmark.
5. **Context.** Each citation shows its resolved date next to its text, so the answering model does not have to work out "last Friday" itself.

The graph channel and the re-ranker run after fusion as before.

## Measurement rules

- The benchmark runner passes each question's date. Hyperag uses it only as `anchor`, never as a time filter, because the stored times of benchmark memories are load times.
- **Dev split, frozen before any tuning:** `eval/public/splits/lme-s-dev.json`, written by `bun eval/public/dev_split.ts`.
  - Test is the seeded n=100 LongMemEval-S subset that the benchmark tables report (seed 42).
  - Dev is the other 400 questions minus 2 whose evidence sessions overlap the test questions' evidence sessions: 398 questions.
  - The file records the source file's SHA-256.
- Every change is decided on dev only. Reported numbers are the n=100 subset and LoCoMo, with the same generator and judge for every system. The full LME-S 500 row includes dev questions, so it is labelled "not held out".
- LoCoMo is never used for tuning. The rules were written with LoCoMo-style dialogue in view (session dates written into each turn, relative dates such as "yesterday"), so LoCoMo is not a fully held-out set for this channel. The main claim rests on the LongMemEval-S n=100 subset.
- Each change is reported as an ablation on the graph-on default template: without the temporal channel, with it, and with it but without the dates added to each passage (`+temporal+nodates`).
- The benchmark profile drives the Rust service over HTTP. The TypeScript library stays frozen as the reference.

## First dev retrieval check (exploratory)

On 24 October 2026, the Rust service was checked on all 398 frozen dev questions with BM25 only, no embedder, no re-ranker and graph off (not the default template). The metric is the fraction of questions whose top 10 contains at least one turn from an answer session. It does not measure answer accuracy or whether all evidence sessions were retrieved. The question date was passed as `anchor` for the temporal profile.

| Question type | Questions | BM25 hits | BM25 + temporal hits |
| --- | --- | --- | --- |
| knowledge-update | 60 | 60 | 60 |
| multi-session | 107 | 106 | 105 |
| single-session-assistant | 45 | 45 | 45 |
| single-session-preference | 25 | 21 | 21 |
| single-session-user | 55 | 55 | 55 |
| temporal-reasoning | 106 | 99 | 99 |
| All | 398 | 386 (0.970) | 385 (0.967) |

Paired comparison: no questions gained a hit; one multi-session question lost its hit. This check shows no retrieval gain from the channel. It cannot establish whether adding resolved dates helps an answering model. A stricter dev metric (recall at 3 or retrieval of all answer sessions) and the graph-on default-template runs are pending.

The counts and artifact hashes are [recorded here](../eval/results/temporal-dev.json), recomputed from the original per-question outputs retained outside this repository. The original run did not record its source commit, so these observations are exploratory and do not certify a particular checkout. The runner is `eval/public/dev_retrieval.ts` on the Rust adapter branch, pending its merge; the split uses seed 42 and records the dataset SHA-256. No held-out quality claim rests on this check.

## Status

- Date and intent rules: done, with unit tests (`cargo test temporal`).
- Dev split: frozen.
- Channel in retrieval (`channels.temporal`, off by default) and dates in citations: done, with unit tests for the date rules. It ranks the fused candidates only, not the whole scope; spans are stored at ingest only if dev shows misses.
- Known limits, to decide on dev: `intent` matches single words, so "my last name" counts as an order question; a day and month with no year take the anchor's year, even when that is in the future; question dates must come from the benchmark (passed as `anchor`), or relative dates in questions resolve against today.
- Pending: merging the Rust benchmark adapter, stricter dev checks, default-template ablations and answer-quality runs.
