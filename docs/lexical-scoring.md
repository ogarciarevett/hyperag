# Private exhaustive lexical scoring

The private scorer ranks an immutable complete generation. The opt-in Rust request
path below resolves authorization and an exact source fence before selecting its
searcher. Service/index writer lifecycle and attachment remain separate.

## Authorized request integration

`HYPERAG_LEXICAL_SCORER=tantivy` opts into one read-only repeatable-read request
snapshot for lexical, vector and payload reads. The default remains `bm25`;
`fast` and `indexed-bm25` remain available. The exact value is case-sensitive.
Fresh grants, PostgreSQL simple query terms and the database/epoch/token fence
are read together. Only a reader at that fence may supply candidates. Missing,
lagging, incompatible or failed readers, or a selected payload version/scope
mismatch, replace the whole lexical list with legacy SQL in the same snapshot.
Temporal and final fused payload loads use that snapshot; it closes before reranking.
No writer advisory lock is taken. Fusion, candidate budgets and rerank caps are unchanged.

The current-reader path uses PostgreSQL-compatible BM25, not widened native scores.
It counts exact distinct lexemes (`length(text_search)`) for document length, all
authorized active documents for N/average, and masked postings for TF/DF. The generation
stores `num_lexemes`; its schema fingerprint rejects older generations until rebuilt.
Tantivy supplies these statistics and query postings to the request transaction.
PostgreSQL evaluates the shared float8 expression (k1=1.2, b=0.75), numeric average
and `ln`, then applies score/time/ID ordering using the database's ID collation.
Both SQL and current-reader paths sum contributions in C lexeme order. Ordering the
formerly unordered default SQL sum can change score bits; frozen-30 default ID/order
and score-bit comparisons are required evidence, alongside real-copy latency.
This path scans no source corpus in SQL, but includes serialization, transfer and SQL
arithmetic; the earlier native scorer's ~97 ms p95 is not proof of its latency.

Vector SQL reuses the snapshot connection, avoiding a second pool acquisition when
all eight service connections are held by requests. Scoring and vector savepoints
keep whole-list SQL fallback and lexical payload reads usable after SQL failures.

The store defaults to an empty reader and SQL fallback. Service/index writer
lifecycle and attachment are a separate slice; this change starts no worker.

## Native offline reference

The native scorer remains available for offline comparisons and its existing tests;
it does not supply request scores. It builds scope and valid-time masks over live documents.
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
versions, recorded times and native score bits. Request integration tests separately
require SQL IDs/order/score-bit equality for repeated terms, ties and validity masks.

Native WAND was rejected: scoped averages can invalidate the stored block-max
pair, and dynamic OR accumulation produced a 1 ULP discrepancy. The retained
512-document regression previously omitted the true winner even in one segment.
