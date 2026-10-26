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

The current-reader path computes BM25 directly in Rust `f64` over native exhaustive
postings. Document length is the stored exact distinct-lexeme count
(`length(text_search)`), TF is unquantized, and N/DF/average include only authorized,
active, temporally valid documents. Empty documents count toward N and average.
It uses SQL's expression grouping, k1=1.2 and b=0.75, without native field norms.
The `num_lexemes` schema fingerprint rejects older generations until rebuilt.

Query terms are deduplicated and summed in lexeme order. A bounded heap selects
score/time candidates, retaining every cutoff tie; PostgreSQL applies its ID
collation to those final candidates. No postings or scoring expression are sent
to PostgreSQL. Distant recorded times with unsafe float8 ordering fall back to SQL.
Rounding ties retain native scores and deterministic score/time/ID ordering.
Only incompatible metadata/readers or request errors trigger whole-list SQL fallback.
The default SQL query, including its unordered SUM, is unchanged from the baseline;
`fast` and `indexed-bm25` keep their existing behavior. Floating-point rounding may
differ. Parity requires identical IDs/timestamps, strict order between SQL-anchored
4-ULP score groups, and equal membership inside each group; anchoring prevents
transitive tolerance widening. Tests reject any non-tie position change and report
absolute, relative and ULP score gaps. Frozen-30 parity and latency require separate
evidence on a current immutable copy.

Vector SQL reuses the snapshot connection, avoiding a second pool acquisition when
all eight service connections are held by requests. Candidate ordering and vector savepoints
keep whole-list SQL fallback and lexical payload reads usable after SQL failures.
Retrieval takes the model read lock before opening its pooled snapshot, matching
the model switch's lock order and leaving connections available to its final pass.

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
require equal SQL IDs and non-tie positions, equal tie-group membership and score gaps.

Native WAND was rejected: scoped averages can invalidate the stored block-max
pair, and dynamic OR accumulation produced a 1 ULP discrepancy. The retained
512-document regression previously omitted the true winner even in one segment.
