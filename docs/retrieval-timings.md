# Rust retrieval stage timings

Send `"profile": true` to `/v1/retrieve` (or `/v1/context`). The retrieval object adds `timingsMs`; omitting the option or using false leaves the response unchanged. Timing output contains numeric milliseconds only and introduces no payload logging.

| Field | Measured work |
|---|---|
| embedding | Query embedding, including model queue/tokenization/inference |
| lexical | Selected lexical store query and result decoding |
| vector | Vector store query and result decoding |
| graph | Graph expansion, including the Neo4j HTTP round trip, query and decoding |
| fusion | All reciprocal-rank fusion calls, accumulated |
| rerank | Re-ranker queue/tokenization/inference |
| payloadLoad | Candidate payload fetch/decoding, including temporal preloads when enabled |
| other | Authorization, model-lock acquisition, input/result preparation and remaining core work |
| total | Complete Hyperag::retrieve wall time |

The pipeline is serial: the seven stages plus other sum to total. A stage that is not called remains zero. Dependency failures retain their existing error/fallback behavior and their awaited duration is recorded. HTTP work before/after retrieve, including final JSON serialization, is outside total; compare with client wall time to observe that remainder. These are wall times, not CPU attribution. Profiling performs no clock reads when disabled.

Synthetic tests cover opt-in serialization/HTTP typing, exactly-once success/error execution, synchronous fusion and real lexical retrieval with profiling on/off yielding identical results. Live per-query stage distributions require a separately gated run after merge and coordinator GO; no latency improvement is claimed by this instrumentation.
