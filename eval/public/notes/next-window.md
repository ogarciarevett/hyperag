# Next-window retries

- **LangMem default, LoCoMo:** blocked during ingestion in the 2026-10-24 GPU window, with zero
  answer rows and no score. The shared endpoint returned HTTP 400:
  `"auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set`.
  Before retrying, the endpoint owner must enable `--enable-auto-tool-choice` and a compatible
  `--tool-call-parser`, then verify native automatic tool calls with the installed LangMem and
  Trustcall pipeline. Keep extraction, updates, and the default model-selected tool behavior.
  Run a small readiness check before the full public benchmark; the readiness check is not a score.
  Do not restart or reconfigure the serving endpoint during the current window. Preserve the
  failed-attempt evidence in `../results/memory/blocked-rows.json`.

The same endpoint requirement defers the queued LangMem DMR, oracle-haystack, and LME-S100 rows before any attempt. They retain no score and are recorded as unattempted blocked rows.

## Final window ledger (2026-10-24)

The evaluation window ended at about 13:37Z. Unfinished runs remain unscored. Every row below has **no score**. Full retries retain the native pipeline; partial answers are evidence only. Completed replacements for earlier failed attempts are listed separately in the window report.

| Queue row | Final status | Reason |
| --- | --- | --- |
| `dmr letta default 2` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle letta default 2` | unrun | Local compute unavailable before the window ended. |
| `dmr mem0 default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `dmr cognee default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `dmr hindsight default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `dmr graphiti default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle mem0 default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle cognee default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle hindsight default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle graphiti default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `locomo hyperag e5+bgev2m3 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `dmr hyperag e5+bgev2m3 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle hyperag e5+bgev2m3 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `locomo hyperag egemma2+nvrerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `dmr hyperag egemma2+nvrerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-oracle hyperag egemma2+nvrerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s100 mem0 default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s100 cognee default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s100 graphiti default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s100 hindsight default 4 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s100 hyperag egemma2+nvrerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s hyperag e5+rerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s hyperag e5+bgev2m3 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s hyperag egemma2+rerank 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `lme-s letta default 2 budget90` | unrun | Local compute unavailable before the window ended. |
| `locomo hyperag nemotron-int8 2 budget90` | blocked | Community INT8 embedding model was not served; INT8_EMBED_MODEL unset. |
| `dmr hyperag nemotron-int8 2 budget90` | blocked | Community INT8 embedding model was not served; INT8_EMBED_MODEL unset. |
| `lme-oracle hyperag nemotron-int8 2 budget90` | blocked | Community INT8 embedding model was not served; INT8_EMBED_MODEL unset. |
| `lme-s100 hyperag nemotron-int8 2 budget90` | blocked | Community INT8 embedding model was not served; INT8_EMBED_MODEL unset. |
| `lme-s100 hyperag nemotron+nvrerank 2` | failed | 53 answers; extra adapter wall-clock deadline expired across machine pause; fixed, full retry prevented by vault rate limit then VM stop. |
| `beir:nfcorpus hyperag nemotron 1 budget90` | failed | Embedding transport 502 as GPU VM became unreachable; no run/meta; full retry required. |
| `beir:nfcorpus hyperag nemotron+nvrerank 1 budget90` | failed | Embedding transport 502 as GPU VM became unreachable; no run/meta; full retry required. |
| `beir:arguana hyperag nemotron 1 budget90` | failed | Embedding transport 502 as GPU VM became unreachable; no run/meta; full retry required. |
| `beir:arguana hyperag nemotron+nvrerank 1 budget90` | failed | Embedding transport 502 as GPU VM became unreachable; no run/meta; full retry required. |
| `beir:scidocs hyperag nemotron 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `beir:scidocs hyperag nemotron+nvrerank 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `beir:fiqa hyperag nemotron 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `beir:fiqa hyperag nemotron+nvrerank 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `beir:trec-covid hyperag nemotron 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `beir:trec-covid hyperag nemotron+nvrerank 1 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `lme-s engram default 4 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `lme-s memvid default 4 budget90` | unrun | VM stopped on idle timer before dispatch. |
| `lme-oracle hyperag egemma2+rerank 2` | unfinished | 384/500 answers; local compute was paused; window ended; no score. |
| `locomo langmem default` | blocked | Native automatic tool selection rejected HTTP 400; needs --enable-auto-tool-choice and --tool-call-parser. |
| `lme-oracle langmem default` | blocked | Native automatic tool selection rejected HTTP 400; needs --enable-auto-tool-choice and --tool-call-parser. |
| `dmr langmem default` | blocked | Native automatic tool selection rejected HTTP 400; needs --enable-auto-tool-choice and --tool-call-parser. |
| `lme-s100 langmem default` | blocked | Native automatic tool selection rejected HTTP 400; needs --enable-auto-tool-choice and --tool-call-parser. |
