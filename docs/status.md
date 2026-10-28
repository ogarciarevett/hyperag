# Status

## Rust build (`crates/hyperag`)

| Feature | Implemented | Tested | Notes |
| --- | --- | --- | --- |
| Core, store, graph, HTTP service with token auth | yes | 13 behaviour scenarios in CI against the running service | same schema and routes as the TypeScript reference |
| Batch ingest, models API, `/healthz` | yes | yes (suite runs, switch race) | |
| Model specs and switching (`hyperag models list`, `set`) | yes | unit tests; switch under concurrent writes in CI | remote providers parsed, never called |
| MCP server (stdio and `/mcp`) | yes | 12+ end-to-end checks in CI | thin client of the service |
| Container and compose stack | yes | on OrbStack (Apple silicon): image build, first-start model pull, README quick start, MCP at /mcp | `docker compose -p hyperag --profile service up -d` |
| Temporal channel (`channels.temporal`, off by default) | yes | date-rule unit tests; exploratory BM25 dev check, 386/398 vs 385/398 evidence-session hits at 10 | no retrieval gain observed; default-template and answer-quality runs pending; see [temporal.md](temporal.md) |
| Parity with the TypeScript reference | yes | smoke suite: equal or within noise on every variant, 13/13 scenarios, lower latency with BM25 only and with the full profile; see eval/RESULTS.md | |

## TypeScript reference (`src/`)


| Feature | Implemented | Tested | Notes |
| --- | --- | --- | --- |
| Postgres store, BM25, versions, valid time | yes | yes, integration | |
| pgvector search, per-space tables, model switch | yes | yes, integration | |
| RRF fusion | yes | yes, through retrieval | |
| Neo4j graph: relate, expand, scope-filtered paths, delete | yes | yes, integration against Neo4j 5.26 | |
| Export and import (`hyperag export`, `hyperag import`) | yes | unit tests for the format; round trip `eval/roundtrip.ts` pending a run | vectors are not exported; the target re-embeds |
| Local embedder (multilingual-e5-small) | yes | yes, contract | |
| Local re-ranker (bge-reranker-base) | yes | yes, contract | English-centred model; Spanish quality is measured in the evaluation |
| OpenAI-compatible embedder (NVIDIA, TEI, vLLM, custom) | yes | contract against a local fake only | no live provider called |
| Cloudflare Workers AI embedder | yes | contract against a local fake only | no live account called |
| HTTP service with token auth | yes | yes, integration | |
| Context with token budget and citations | yes | yes | tokens estimated as chars / 4 |
| Result cache | no | | none exists, so none to invalidate |
| Hugging Face models other than the two defaults | via `createLocalEmbedder({ model, revision, dimensions })` | no | not every model runs on every machine |

## Pending

- The older `single-agent.ts` and `two-agents.ts` examples are run by hand. The seven `examples/readme/` modules run against isolated services in CI; remote embedding coverage uses a loopback fake.
- Live calls to NVIDIA, Cloudflare or a hosted Hugging Face endpoint: not made.

## Known limits

- Token counts are estimated, not counted with the generator's tokenizer.
- BM25 uses the `simple` text configuration: no stemming, the same for English and Spanish.
- BM25 materialises the caller's readable corpus per query (no GIN index use): cost grows linearly with corpus size.
