# Private exhaustive lexical scoring

The private scorer ranks an immutable complete generation. The opt-in Rust request
path below resolves authorization and an exact source fence before selecting its
searcher. Service/index writer lifecycle and attachment remain separate.

## Authorized request integration

`HYPERAG_LEXICAL_SCORER=tantivy` opts into one read-only repeatable-read request
snapshot for the lexical channel and payload loads. The default remains `bm25`;
`fast` and `indexed-bm25` remain available. The exact value is case-sensitive.
Fresh grants, PostgreSQL simple query terms and the database/epoch/token fence
are read together. Only a reader at that fence may supply candidates. Missing,
lagging, incompatible or failed readers, or a selected payload version/scope
mismatch, replace the whole lexical list with legacy SQL in the same snapshot.
Temporal and final fused payload loads use that snapshot; it closes before reranking.
No writer advisory lock is taken. Fusion, candidate budgets and rerank caps are unchanged.

The store defaults to an empty reader and SQL fallback. Service/index writer
lifecycle and attachment are a separate slice; this change starts no worker.

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
