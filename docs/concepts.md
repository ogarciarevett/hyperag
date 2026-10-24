# Concepts

## Memory record

Stable `id` (derived from scope + source + text, or a given id stored as `<scope>/<id>`), `scope`, `kind`, `version`, `text`, `source`, `author`, `recordedAt` (when Hyperag learned it), `validFrom`/`validTo` (when the fact held), `status` (`active`, `superseded`, `invalidated`), `statusReason`, `supersededBy`, `metadata`.

## Write semantics

- **Idempotent writes.** The same scope + source + text maps to the same id; re-ingesting it returns `created: false`. A write with an `idempotencyKey` already seen for that principal returns the first result. Updating to the current text is a no-op.
- **Two agents updating one memory.** Optimistic concurrency. `update` takes the `expectedVersion` the writer read; the memory row is locked, and a writer whose version is stale gets `version_conflict` (HTTP 409) with `currentVersion`. It re-reads and decides; nothing is merged silently, and every accepted version stays in history.
- **Corrections across memories.** `ingest({ supersedes: [oldId] })` marks the old memory's current version `superseded` with `supersededBy`, and adds a `SUPERSEDES` edge. The writer needs write access to the old memory's scope.
- **Not active.** `update` and `invalidate` on a superseded or invalidated version fail with `not_active` (409), so a corrected fact is never silently revived and its reason is never overwritten.
- **Deletion** removes every version and vector row in one transaction, then the graph node with its edges. A deleted id cannot be re-created. If the graph step fails, calling `delete` again finishes it; until then reads drop edges whose ends are deleted in Postgres. Memories a deleted correction had superseded stay superseded.
- **Revocation** takes effect on the next call: search, context, graph expansion and explain all read the caller's scopes from the grants table on every call. Graph paths are kept only when every node on the path is readable. There is no result cache, so there is nothing else to invalidate.

## Embeddings

The core needs only `embedDocuments(texts)`, `embedQuery(question)` and `describeModel()`. `describeModel()` returns model, revision, dimensions, normalization and preprocessing (including query/document mode); its hash is the embedding space id, and each space has its own vector table, so two models with the same dimension never share an index. A query is only compared against vectors of the configured space.

Switching models: `reindex(newEmbedder)` fills the new space's table, then construct Hyperag with the new embedder. Source text, BM25, the graph and the old index stay.

## Evidence and abstention

With a re-ranker, the top item must reach `minRerankScore` (default 0). Without one, the best cosine similarity must reach `minVectorScore` (default 0.82). With neither embedder nor re-ranker the service cannot abstain and says so in `limitations`.
