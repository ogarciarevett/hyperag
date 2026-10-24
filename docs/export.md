# Export and import

`hyperag export` writes everything a store knows to one file. `hyperag import` reads it into any configured store, with the database, embedder and re-ranker you choose there.

```sh
hyperag export --out memory.jsonl                      # HYPERAG_PG_URL and HYPERAG_NEO4J_URL of the source
hyperag import memory.jsonl                            # into the target's store, embedded with its active model
hyperag import memory.jsonl --embedding <spec> --reranker <spec> --as <admin>   # pick the models on import
hyperag import memory.jsonl --without-graph            # into a store with no graph: relations are dropped
```

## Format (`hyperag-export`, version 1)

JSON Lines, one record per line, each with a `record` field:

| Record | Holds |
| --- | --- |
| `header` (first line) | `format`, `formatVersion`, `exportedAt`, the Hyperag version |
| `grant` | principal, scope, permission, who granted it and when |
| `grantLog` | every grant and revocation, with actor and time |
| `memory` | id, scope, kind, creator, creation time, current version, the tombstone of a deleted memory, and every version: text, source, author, content hash, valid time (`validFrom`, `validTo`), recorded time, status (`active`, `superseded`, `invalidated`), its reason, what superseded it, metadata |
| `relation` | typed graph edge with its endpoints' scopes, creator and time |
| `end` (last line) | the count of every record type |

Timestamps keep microseconds, so an imported time equals the exported one.

What is not in the file:
- Tokens and secrets. Tokens stay in the tokens file; the file names principals only.
- Vectors. The importing store embeds the current versions with the embedder it is given, in that model's own index. So the source and target can use different models.

## Import rules

- The file is checked before anything is written. The first line must be a supported header, and the end record's counts must equal the records, so a cut file is refused.
- Imports serialize through a database transaction lock. Store rows go in one transaction. A memory that already exists with the same complete history (including status, valid time, metadata, provenance and tombstone) is skipped, so importing a file twice changes nothing. A memory that exists with different history is a conflict, and nothing is imported.
- Graph writes come after the commit. They are idempotent merges, so a retry after a graph failure finishes them. A file with relations needs a graph (`HYPERAG_NEO4J_URL`), or `--without-graph`.
- `--embedding` or `--reranker` switch the target's models (an admin act, `--as`). The new embedding index is filled before the switch takes effect. Without them, the target's active embedder embeds what is missing.

Check: `bun eval/roundtrip.ts` builds a fixture with versions, a correction, valid time, an invalidation, a deletion, grants, a revocation and a relation. It exports it, imports it into a second stack with the same embedder, and checks that every retrieve and explain answer is the same. It also imports it into a third stack with BM25 only and no graph, and checks that every history is the same. Results go to `eval/results/roundtrip.json`.

Build the Rust binary first (`cargo build --release -p hyperag`). The check starts that binary by default and creates temporary `rs_*` databases. To use a separate stack, set `HYPERAG_PG_URL`, `HYPERAG_NEO4J_URL`, `HYPERAG_NEO4J_USER` and `HYPERAG_NEO4J_PASSWORD` to its endpoints and credentials.

Export reads all Postgres rows from one repeatable-read snapshot, so concurrent writes cannot mix memory heads and versions. Neo4j is read afterwards and has no shared transaction with Postgres: pause writes to the source while taking an export that must preserve the graph and store at the same instant. User memory text and metadata are exported verbatim and may themselves contain private data; only configured service tokens and credentials are excluded.

File exports use a unique temporary file next to the destination and publish it only after writing succeeds, including when the destination ends in `.part`. A failed write leaves the previous destination intact.
