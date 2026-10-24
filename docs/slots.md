# Slots

Hyperag is a set of slots with small interfaces. The default template fills each slot with one choice; any slot can be swapped without touching the others.

| Slot | Interface (Rust, `crates/hyperag`) | Default | Alternatives shipped |
| --- | --- | --- | --- |
| Store | `store::Store`: versions, BM25, vector tables per embedding space, grants | Postgres + pgvector | any Postgres 16+ with pgvector |
| Embedder | `models::Embedder`: `embed_documents`, `embed_query`, `describe` (model, revision, dimensions, normalisation, prompts) | EmbeddingGemma 2 q8 | any Hugging Face ONNX export (`hf:`), OpenAI-compatible endpoints (`http:`), NVIDIA (`nvidia:`), Cloudflare (`cloudflare:`), `none` |
| Re-ranker | `models::Reranker`: one score per document | bge-reranker-v2-m3 q8 | other fastembed or ONNX cross-encoders, `none` |
| Graph | `graph::Graph`: relate, scope-filtered expansion, delete | Neo4j over its HTTP API, on | another graph database behind the same interface; off (no `HYPERAG_NEO4J_URL`) is possible but is not the default template |
| LLM | none | none | Hyperag returns context; the agent's own model answers |

Rules that hold for every choice:

- An embedding model's description is hashed into its embedding space; each space has its own vector table, so models never share an index.
- Switching an embedder builds the new index before it becomes active; documents, BM25 and the graph stay.
- A slot that fails at query time is reported in `limitations`, and the other slots still answer.

How to pick for a machine: `hyperag models recommend` (see the README for the formula and the tiers).
