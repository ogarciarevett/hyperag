# Private exhaustive lexical scoring

This unit scores an immutable Tantivy searcher supplied by a future caller. It
does not enable a configuration value, start a service worker, or change retrieval.
The caller must resolve fresh grants and an exact source fence in one authorized
PostgreSQL snapshot before selecting the searcher. That integration follows separately.

The scorer builds scope and valid-time masks over live generation documents.
Generation construction already excludes deleted and noncurrent/inactive source
versions. N, total token count, and each query term's DF include only the masked
corpus. A stored `num_tokens` field preserves exact pretokenized lengths for the
average; individual document lengths retain Tantivy's quantized field norms.
The schema fingerprint changes, so older generations require rebuild/fallback.

Query terms are deduplicated and sorted by lexeme. The scorer visits their
postings exhaustively, masks each document before accumulating its BM25 score,
and adds native term contributions in that order using `f32`. A bounded heap
selects score descending, recorded time descending, ID ascending before truncation.
There is no pruning, no statistics cache, and no full-text payload in the index.

The independent reference builds an index containing only authorized documents,
uses native TermQuery weights without pruning, adds their contributions in the
same lexeme order and type, then sorts all matches. Tests require identical IDs,
versions, recorded times and score bits. This does not assert SQL-score equality.

Native WAND was rejected: scoped averages can invalidate the stored block-max
pair, and dynamic OR accumulation produced a 1 ULP discrepancy. The retained
512-document regression previously omitted the true winner even in one segment.
