# Module interfaces

Hyperag retrieves evidence for an application's answer model. The installed Rust service
and TypeScript reference share the [HTTP contract](api.md); their model configuration
surfaces differ. [Runnable examples](../README.md#module-examples) exercise the TypeScript
reference service with Postgres and Neo4j on isolated test data.

| Module | Interface | Backend and limits | Details |
| --- | --- | --- | --- |
| Ingest | `POST /v1/memories`; `Hyperag.ingest(principal, NewMemory)` | Text, source, scope and optional metadata; returns id/version. Parsing and chunking are caller responsibilities. Rust also supports batches of up to 512. | [API](api.md), [concepts](concepts.md) |
| Graph relations | `POST /v1/relations`; `relate`, `relations`; `GraphStore` | Neo4j stores scoped ids and typed edges, not text. Expansion filters every path by readable scopes. Neo4j or off in the service; custom adapters require TypeScript integration work. No automatic edge migration. | [Architecture](architecture.md), [configuration](configuration.md), [types](../src/types.ts) |
| Embeddings | `Embedder.embedDocuments`, `embedQuery`, `describeModel`; `reindex` | Local ONNX, OpenAI-compatible HTTP or Cloudflare adapters in TypeScript. Every model/revision/preprocessing combination has its own vector space. Reindex first, then construct an instance with the new embedder. | [Models](models.md), [adapters](../src/adapters/embedders.ts) |
| Re-ranking | `Reranker.rerank(query, documents)` returns one score per document; `name` identifies it | Local cross-encoder supplied; custom TypeScript scorer can implement the interface. `channels.rerank: false` skips it. Rust config uses a separate reranker model spec. | [Models](models.md), [types](../src/types.ts) |
| Search | `POST /v1/retrieve`, `/v1/context`; `retrieve`, `context` | BM25/vector fusion, optional graph expansion and re-ranking. Returns channel evidence, limitations and `insufficientEvidence`; context includes numbered citations. Token counts are estimates. | [API](api.md), [concepts](concepts.md) |
| Host plugin | `loadPlugin` from `hyperag/plugin`; `save`, `recall`, `search`, `health` | Remote ESM client. Dedicated principal with server-side grants; private owner-only token file. Health proves identity/connectivity, not scope access. No models or databases in the host. | [Plugin contract](plugin.md), [host example](../examples/plugin-host.ts), [MCP alternative](mcp.md) |

## Assemble a graph-backed application

Use Postgres/pgvector for versioned source text and vectors, Neo4j for typed relations,
EmbeddingGemma for embeddings and an independently configured reranker. The Rust
[default template](../docker-compose.yml) selects pinned EmbeddingGemma 2 q8 and
bge-reranker-v2-m3 q8. EmbeddingGemma 300m is a different model option; see [model specs](models.md).
The TypeScript reference defaults to multilingual-e5-small when using `hyperagFromEnv`;
its programmatic constructor accepts explicit slots.

Grant each application principal only the scopes it needs. Ingest source text, relate
saved ids when the application has evidence for an edge, then retrieve cited context.
The caller supplies the answer model and must respect weak-evidence signals. Creating
a graph backend does not extract relationships automatically.

For the Rust service, choose embeddings and reranking with `HYPERAG_EMBEDDING` and
`HYPERAG_RERANKER`, or the admin `models set` command. A model switch builds the new
embedding index before activation. Neo4j connection settings take effect on service
startup; changing a URL does not migrate existing edges. TypeScript environment setup
uses `HYPERAG_EMBEDDER=local|none` instead of Rust model specs; use the programmatic
adapters for other providers. Remote embedding contracts are tested with local fakes;
live provider acceptance and application-specific retrieval quality require separate validation.
