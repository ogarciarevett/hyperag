# Final row ledger after two GPU windows

Window 2 ended at 00:57:48Z on October 25. This ledger covers all 49 previously unresolved or second-window planned rows. Completed replacements supersede failed attempts; partial answers remain unscored. Historical window-1 results are unchanged.

The admission rule was **runtime estimate + 1.0 minute dispatch safety margin < time remaining before 01:33:43Z**. For the borderline skips, **35.0 + 1.0 = 36.0 minutes required**, versus about **35.9 minutes remaining**. Runtime estimates already included their stated contingency; the extra minute was an additional dispatch reserve. Displayed values are rounded to 0.1 minute; admission used full precision.

The earlier 21:59Z stop-latch messages were superseded scheduling events, not final budget skips. The clock remained October 24 19:41:43Z + USD 63.60 prior spend at USD 4.50/hour. See the [machine-readable ledger](../results/window-2026-10-25-ledger.json) and [window report](window-2026-10-25.md).

| Benchmark | System | Profile | Final status | Reason |
| --- | --- | --- | --- | --- |
| dmr | letta | `default` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-oracle | letta | `default` | unrun | Not selected for the second-window subset; no new score. |
| dmr | mem0 | `default` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| dmr | cognee | `default` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| dmr | hindsight | `default` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| dmr | graphiti | `default` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-oracle | mem0 | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-oracle | cognee | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-oracle | hindsight | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-oracle | graphiti | `default` | unrun | Not selected for the second-window subset; no new score. |
| locomo | hyperag | `e5+bgev2m3` | complete | Full 1986-question/query run completed; zero search/query failures. |
| dmr | hyperag | `e5+bgev2m3` | unfinished | Lower-priority attempt preempted after 114 answers; no score. Runtime estimate 55.0 min + 1.0 min dispatch safety margin = 56.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-oracle | hyperag | `e5+bgev2m3` | unrun | Runtime estimate 75.0 min + 1.0 min dispatch safety margin = 76.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| locomo | hyperag | `egemma2+nvrerank` | unrun | Runtime estimate 50.0 min + 1.0 min dispatch safety margin = 51.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| dmr | hyperag | `egemma2+nvrerank` | unrun | Runtime estimate 35.0 min + 1.0 min dispatch safety margin = 36.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-oracle | hyperag | `egemma2+nvrerank` | unrun | Runtime estimate 75.0 min + 1.0 min dispatch safety margin = 76.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-s100 | mem0 | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-s100 | cognee | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-s100 | graphiti | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-s100 | hindsight | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-s100 | hyperag | `egemma2+nvrerank` | unrun | Runtime estimate 120.0 min + 1.0 min dispatch safety margin = 121.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-s | hyperag | `e5+rerank` | unrun | Not selected for the second-window subset; no new score. |
| lme-s | hyperag | `e5+bgev2m3` | unrun | Not selected for the second-window subset; no new score. |
| lme-s | hyperag | `egemma2+rerank` | unrun | Not selected for the second-window subset; no new score. |
| lme-s | letta | `default` | unrun | Not selected for the second-window subset; no new score. |
| locomo | hyperag | `nemotron-int8` | blocked | No INT8 embedding checkpoint was served; no second-window attempt or score. |
| dmr | hyperag | `nemotron-int8` | blocked | No INT8 embedding checkpoint was served; no second-window attempt or score. |
| lme-oracle | hyperag | `nemotron-int8` | blocked | No INT8 embedding checkpoint was served; no second-window attempt or score. |
| lme-s100 | hyperag | `nemotron-int8` | blocked | No INT8 embedding checkpoint was served; no second-window attempt or score. |
| lme-s100 | hyperag | `nemotron+nvrerank` | failed | The first-window attempt remains unscored. Runtime estimate 120.0 min + 1.0 min dispatch safety margin = 121.0 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:nfcorpus | hyperag | `nemotron` | complete | Full 323-question/query run completed; zero search/query failures. |
| beir:nfcorpus | hyperag | `nemotron+nvrerank` | complete | Full 323-question/query run completed; zero search/query failures. |
| beir:arguana | hyperag | `nemotron` | complete | Full 1406-question/query run completed; zero search/query failures. |
| beir:arguana | hyperag | `nemotron+nvrerank` | failed | The first-window attempt remains unscored. Runtime estimate 60.6 min + 1.0 min dispatch safety margin = 61.6 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:scidocs | hyperag | `nemotron` | unrun | Runtime estimate 49.8 min + 1.0 min dispatch safety margin = 50.8 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:scidocs | hyperag | `nemotron+nvrerank` | unrun | Runtime estimate 79.6 min + 1.0 min dispatch safety margin = 80.6 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:fiqa | hyperag | `nemotron` | unrun | Runtime estimate 90.3 min + 1.0 min dispatch safety margin = 91.3 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:fiqa | hyperag | `nemotron+nvrerank` | unrun | Runtime estimate 109.7 min + 1.0 min dispatch safety margin = 110.7 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:trec-covid | hyperag | `nemotron` | unrun | Runtime estimate 242.3 min + 1.0 min dispatch safety margin = 243.3 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| beir:trec-covid | hyperag | `nemotron+nvrerank` | unrun | Runtime estimate 243.8 min + 1.0 min dispatch safety margin = 244.8 min required; about 35.9 min remained before the no-new-start cutoff. Full retry was not admitted. |
| lme-s | engram | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-s | memvid | `default` | unrun | Not selected for the second-window subset; no new score. |
| lme-oracle | hyperag | `egemma2+rerank` | unfinished | First-window evidence: 384/500 answers; local compute was paused; window ended; no score. Not selected for the second-window subset; no new score. |
| locomo | langmem | `default` | blocked | Native automatic tool selection requires an enabled parser. Readiness was not established in window 2; no second-window attempt or score. |
| lme-oracle | langmem | `default` | blocked | Native automatic tool selection requires an enabled parser. Readiness was not established in window 2; no second-window attempt or score. |
| dmr | langmem | `default` | blocked | Native automatic tool selection requires an enabled parser. Readiness was not established in window 2; no second-window attempt or score. |
| lme-s100 | langmem | `default` | blocked | Native automatic tool selection requires an enabled parser. Readiness was not established in window 2; no second-window attempt or score. |
| locomo | hyperag | `egemma2+bgev2m3+graph` | complete | Full 1986-question/query run completed; zero search/query failures. |
| dmr | hyperag | `egemma2+bgev2m3+graph` | complete | Full 500-question/query run completed; zero search/query failures. |

LangMem retains the native automatic-tool-selection requirement; the serving endpoint was not reconfigured by the evaluation worker. INT8 remains blocked by the absent served checkpoint. All full LME-S rows remain unrun. Earlier failed and partial evidence stays under `../results/memory/attempts/` and `../results/beir/attempts/`.
