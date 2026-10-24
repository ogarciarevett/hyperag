# Configuration

Every component is a URL or a spec string, so local and remote deployments differ only in configuration.

| Variable | Default | What it does |
| --- | --- | --- |
| `HYPERAG_PG_URL` | (required) | Postgres with pgvector. The schema is applied at start. |
| `HYPERAG_NEO4J_URL` | set by the Docker stack (`http://neo4j:7474`): graph on | Neo4j HTTP endpoint, for example `http://127.0.0.1:57474`. The default template has the graph on; unset, the graph channel is off. |
| `HYPERAG_NEO4J_USER`, `HYPERAG_NEO4J_PASSWORD` | `neo4j`, empty | Neo4j login. |
| `HYPERAG_EMBEDDING` | the last `hyperag models set`, else EmbeddingGemma 2 q8 | Embedding spec; `none` for BM25 only. See [models](models.md). |
| `HYPERAG_RERANKER` | the last `hyperag models set`, else bge-reranker-v2-m3 q8 | Re-ranker spec. |
| `HYPERAG_MODEL_CACHE` | `~/.cache/hyperag/models` | Where model files are downloaded. |
| `HYPERAG_MODEL_THREADS` (or `HYPERAG_THREADS`) | half the cores, at most 8 | ONNX Runtime threads per model. |
| `HYPERAG_TOKENS_FILE` | (required for `serve`) | JSON `{ "<sha256 of token>": "<principal>" }`. Add entries with `hyperag token <principal>`. |
| `HYPERAG_ADMINS` | empty | Comma-separated principals that may grant, revoke and switch models. |
| `HYPERAG_HOST`, `HYPERAG_PORT` | `127.0.0.1`, `7480` | Where the service listens. |
| `HYPERAG_MCP_ALLOWED_HOSTS` | loopback and the bound address | Extra `Host` values accepted at `/mcp` (rebinding protection). |
| `HYPERAG_URL`, `HYPERAG_TOKEN` | `http://127.0.0.1:7480`, (required) | For `hyperag mcp`: the service and the agent's token. |

Access: a principal reads and writes only the scopes granted to it (`POST /v1/grants`, admin only). A scope is a name such as `team:payments` or `agent:coder`; one agent's private memory is a scope only it can read, shared memory is a scope several agents can read.
