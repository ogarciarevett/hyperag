# API

The HTTP API is the contract; the TypeScript library is the reference implementation of the same calls.

## HTTP service (Rust: `hyperag serve`; TypeScript: `bun src/main.ts`)

```sh
HYPERAG_PG_URL=postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag \
HYPERAG_NEO4J_URL=http://127.0.0.1:57474 HYPERAG_NEO4J_PASSWORD=fake-test-only \
HYPERAG_TOKENS_FILE=./tokens.json HYPERAG_ADMINS=root \
HYPERAG_RERANKER=local bun src/main.ts
```

`tokens.json` maps the SHA-256 hex of each bearer token to a principal: `{ "<sha256>": "agent-a" }`. The service ignores any identity in a request body. Only principals in `HYPERAG_ADMINS` may change grants.

| Method and path | Call |
| --- | --- |
| `POST /v1/memories` | ingest |
| `PATCH /v1/memories/:id` | update |
| `POST /v1/memories/:id/invalidate` | invalidate |
| `DELETE /v1/memories/:id?reason=` | delete |
| `GET /v1/memories/:id/explain` | explain |
| `GET /v1/memories/:id/relations` | relations |
| `POST /v1/relations` | relate |
| `POST /v1/retrieve`, `POST /v1/context` | retrieve, context |
| `POST /v1/grants`, `DELETE /v1/grants` | grant, revoke (admin) |
| `GET /v1/whoami` | the verified principal |

Ids in paths are percent-encoded (an explicit id is stored as `<scope>/<id>`). Bodies are limited to 1 MiB and validated; `limit` is at most 50, `candidatesPerChannel` 150, `maxTokens` 32,000.

Errors: 401 unauthenticated, 403 forbidden, 404 not found (also for memories outside the caller's scopes), 409 version conflict, existing id or not active, 400 invalid input.

Local or remote deployment is configuration only: every store is a URL.

The Rust service also has `POST /v1/memories/batch` (up to 512), `GET` and `POST /v1/models`, and `/healthz` without a token.
`GET /healthz` checks Postgres with a one-second deadline: HTTP 200 and `{"ok":true}` when available, otherwise HTTP 503 and `{"ok":false}`. Pool acquisition for service operations is bounded to two seconds; this does not bound query or model execution time.

## TypeScript library

```ts
import { Pool } from "pg";
import { Hyperag, createPostgresStore, createLocalEmbedder, createLocalReranker, createNeo4jGraph } from "hyperag";

const h = new Hyperag({
  store: createPostgresStore(new Pool({ connectionString: process.env.HYPERAG_PG_URL })),
  // The TypeScript reference's defaults (the Rust service defaults to EmbeddingGemma 2 and bge-reranker-v2-m3):
  embedder: createLocalEmbedder({ model: "Xenova/multilingual-e5-small" }),  // CPU, 118M params q8, 384 dims; or null: BM25 only
  reranker: createLocalReranker({ model: "Xenova/bge-reranker-base" }),       // CPU, 278M params q8; or null: fused order
  graph: createNeo4jGraph({ baseUrl, username, password }), // the default template; null turns the graph channel off
  admins: ["root"],                         // who may grant and revoke
});
await h.init();                             // schema, embedding space, graph constraint (idempotent)
```

Every call names the acting principal. In-process the principal is trusted as given; the HTTP service is where identity is verified.

| Call | What it does |
| --- | --- |
| `grant(admin, principal, scope, "read" \| "write")`, `revoke(admin, principal, scope, perm)` | Manage access; `admin` must be in `config.admins`; logged in `grant_log`. Scopes come only from grants. |
| `ingest(p, { scope, text, source, kind?, id?, validFrom?, validTo?, metadata?, supersedes?, idempotencyKey? })` | New memory, version 1. |
| `update(p, id, { text, source, expectedVersion, ... })` | New version; the previous one becomes `superseded`. |
| `invalidate(p, id, reason)` | Stops ranking, ends validity now, keeps history. |
| `delete(p, id, reason)` | Removes text, vectors and graph node; keeps a content-free tombstone. |
| `relate(p, { from, to, type })`, `relations(p, id)` | Typed edges between memories (Neo4j). |
| `retrieve(p, query, { limit?, asOf?, channels? })` | Ranked items with per-channel ranks, re-rank score, `insufficientEvidence`, `pathsUsed`, `limitations`. |
| `context(p, query, { maxTokens? })` | Retrieval packed into a token budget with citations. |
| `explain(p, id)` | Every version, author, recorded and valid time, status reason, what it corrected, relations, tombstone. |
| `reindex(embedder)` | Embeds all current memories into a new embedding space's own table. |
