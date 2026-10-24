# Harness arm: native meta-harness vs harness2, memory fixed to Hyperag

This arm keeps the memory backend constant and changes only the harness around the executor, so a
harness effect is not confused with the Hyperag vs Engram comparison in `eval/`.

## Setup

- **Tasks.** Hyperag's evaluation set has no coding tasks yet, and its cross-team suite is held out
  (nothing is tuned on it). `build_dev.py` therefore runs the same cross-team generator templates
  with new project names and a new seed. Asserts check that no document id or project overlaps the
  held-out suite. Two packs (`packs.py`), each split 40/40/20 train/search/sealed by id hash:
  - **handoff** (72 items): cross-team handoff, multi-evidence and abstention questions. Output: the
    memory ids to cite, or `[]` to abstain. Quality: F1 of the cited set; abstention items score 1
    only for `[]`. Hard failure: citing a memory outside the retrieved hits.
  - **engineering** (64 items, the coding task): code that turns memory hits into a release record
    (owner, rollback image, root cause, current launch date). Quality: share of fields exactly right.
    Hard failure: a value that appears in no retrieved hit.
- **Memory, fixed.** `retrieve.ts` ingests the 1,536 dev documents into Hyperag once and saves each
  query's top 10 (`data/hits.json`). Config: multilingual-e5-small, no re-ranker, no graph,
  `limit = 10`; each query is asked by a principal that reads only its scope. Both sides read the
  same saved hits, so the memory output is identical by construction.
- **Sides.** The runner is the harness2 pilot's `round2.py`, imported unchanged; `run.py` only
  registers the two packs and seeds.
  - A: native meta-harness, one Codex proposal plus one native search round.
  - B: harness2 at `1cfe254e8d25b15f37048c3730134686abe60335`, parallel mode, k=1: one Propose,
    Probe, Compose step, with automatic singleton selection.
  - Same starting harness, same visible data (8 labelled train items, 8 unlabelled search inputs),
    1200 active seconds per task and side.
  - Executor: Codex 0.164.0 (real binary), `gpt-6.1-sol`, medium effort, `service_tier="standard"`,
    sandbox on, command network off.
  - Sealed labels never reach a model, and sealed scoring happens only after the final selection.
- **Host stops.** A run pauses on load over 20, free memory under 40 percent or swap over 8 GB. It
  resumes with `--resume` and keeps only the unspent cap. Stopped time counts against the cap.

## Results (one trial per side)

| Task | Side | Status | Sealed score | Pass | Active s | Phases reached | Compose changed harness | B own baseline | B delta |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| handoff | A | complete | 0.7359 | fail | 75.2 | n/a (native loop) | n/a | n/a | n/a |
| handoff | B | complete | 0.7359 | fail | 793.1 | baseline, propose, probe, compose | no (no-op) | 0.7359 | 0.0000 |
| engineering | A | complete | 0.7949 | fail | 94.7 | n/a (native loop) | n/a | n/a | n/a |
| engineering | B | complete | 0.7949 | fail | 503.6 | baseline, propose, probe, compose | no (no-op) | 0.7949 | 0.0000 |

Sealed items: 13 per task.

Pass means no hard failure and a sealed score of at least 0.95, the pilot's rule.

Starting plugins on the same sealed items: handoff 0.4103, engineering 0.5641.

**Verdict.**
- **handoff:** tie.
- **engineering:** tie.

Neither verdict shows a harness effect:

- **Every final plugin hits the retrieval ceiling.** The ceiling is the best score possible from the
  saved hits: cite exactly the required ids that were retrieved, or fill exactly the fields whose
  value appears in a hit. On the sealed items it is 0.7359 (handoff) and 0.7949 (engineering), and
  all four final plugins reach it. The two sides wrote different plugins (different SHA-256), and
  both reach the limit. The remaining loss comes from evidence Hyperag did not return in the top
  10: on the dev suite, all evidence is in the top 10 for 70 of 112 answerable questions. Hyperag
  also flags insufficient evidence on 0 of 16 probes, so all abstention comes from the plugins.
- **Compose was a no-op on both tasks.** The composed harness is byte-identical to harness2's own
  materialized base. Both proposals on handoff and one on engineering edited the harness (AGENTS.md and a new skill), but Compose kept none of it.
  B's final equals its own baseline (delta 0.0000).
- **Grounding check is a substring test.** An empty or one-letter engineering value does not hard-fail; it scores 0 instead. This did not affect these runs (all four plugins sit at the ceiling) and the grader was not changed after the runs.
- **One trial, 13 sealed items per task.** A difference this small could not be read as a winner
  anyway.

**Cost and calls.** These are standard-price API-equivalent estimates from the Codex usage receipts,
not an invoice:

| Task | Side | Model calls | USD | Input / cached / output tokens |
| --- | --- | --- | --- | --- |
| handoff | A | 1 | 0.0815 | 174,658 / 154,240 / 2,519 |
| handoff | B | 10 | 0.8701 | 1,859,280 / 1,610,496 / 21,144 |
| engineering | A | 1 | 0.1142 | 250,193 / 219,520 / 3,093 |
| engineering | B | 7 | 0.9102 | 1,591,480 / 1,289,856 / 17,801 |

Load-stopped calls can lack final usage, so B's costs are lower bounds.

**Incidents.**
- **Host stops on handoff B.** Three calls were load-stopped. The first was a memory stop at 35
  percent free.
- **One crash on resume.** A stop before a model call left an empty editor folder, and the next
  resume failed when it tried to create the same folder. No model call ran in that folder. The saved
  result was reset to its load-stopped state, with all 235.6 consumed seconds kept. `run.py` now
  removes such an empty, reply-less folder before a retry.
- **Engineering ran without a stop.**

**What this shows.** With the memory backend fixed and recall capped, harness2's one-step recursion
cost 6 to 11 times the tokens and 5 to 11 times the active time of the native loop, for the same
sealed score. A harness comparison that can separate the two methods needs tasks where the retrieved
evidence is not the bottleneck: a larger `limit`, the graph or re-ranker variant, or harder
selection among hits.

## Reproduce

```sh
python3 eval/harness2/build_dev.py <packaged benchmark>/data      # dev suite, no held-out overlap
docker compose -p hyperag-h2 up -d postgres && bun eval/harness2/retrieve.ts && docker compose -p hyperag-h2 stop
PILOT=<harness2 pilot> $PILOT/.venv/bin/python eval/harness2/run.py --pack handoff      # then --pack engineering
python3 eval/harness2/report.py                                    # writes results.json, prints the table
```

`runs/` is not committed: it holds per-call Codex logs and workspaces. `results.json` keeps both
sides' result records and side B's phases and no-op check.

```
BENCH model=gpt-6.1-sol effort=medium service_tier=standard executor=codex-0.164.0 sandbox=on network=off
BENCH harness A=meta-harness@ac0aba4e native (1 proposal + 1 search round) B=harness2@1cfe254e parallel k=1
BENCH budget=1200 active s per task-side, serialized calls; memory=Hyperag multilingual-e5-small, no reranker, no graph, limit 10
```
