# Systems under test

Every system runs its own default pipeline as its docs describe it, with one shared open model for
every LLM stage (`EVAL_LLM_*`, the rented endpoint) and, where a system takes an external embedder,
the same local `BAAI/bge-small-en-v1.5` (`eval/fastembed_server.py`, port 58322). Retrieval models
still differ where a product ships its own: Hyperag's GPU profile is `e5+rerank` (multilingual-e5-small
and a bge-reranker-base cross-encoder); Hindsight uses its default local reranker; the other systems
have no reranker. Every results table says so next to the numbers. Adapters live in
`systems/`; Python ones run in their own venv (`eval/public/.venv-<name>`) behind `systems/py.ts`.
Each adapter's docstring has its exact install command and configuration; every result file
records versions and config.

The common GPU transport reset policy and its activation boundary are disclosed in
[transport-retry.md](notes/transport-retry.md). It applies the same bounded recovery
to every system using the shared GPU endpoints; native SDK retries remain documented
separately. Historical rows are labeled by the policy active when they ran.

| System (label in tables) | Version | Pipeline as run | Stores and services | LLM at ingestion |
| --- | --- | --- | --- | --- |
| Hyperag | repo build | `systems/hyperag.ts` (its own profiles) | Postgres (`docker compose -p hyperag`) | no |
| Engram | engram 3.3.2 | MCP `mem_save` / `mem_search`, tool defaults (`default`) and `match_mode=any` (`any`) | fresh `ENGRAM_DATA_DIR` per run | no |
| Memvid | memvid-sdk 2.0.160 | SDK defaults: lexical (Tantivy) index, `find` mode auto | one `.mv2` per namespace | no |
| Letta 0.16.8 self-hosted | letta/letta:0.16.8 | archival memory insert and archival search (the agent's own search tool) | Docker, its bundled Postgres | no |
| Mem0 | mem0ai 2.2.1, spaCy en_core_web_sm 3.8.0 | `add(infer=True)`, default `search` | embedded Qdrant | yes |
| Graphiti | graphiti-core 0.31.2 | `add_episode`, `search` (EDGE_HYBRID_SEARCH_RRF) | Neo4j 5.26.28 in Docker | yes |
| Cognee | cognee 1.6.5 | `add` + `cognify` (defaults), `SearchType.CHUNKS` | embedded SQLite, LanceDB, Ladybug | yes |
| Hindsight | hindsight-api 1.1.0 | `retain`, `recall` (defaults) | its server, embedded Postgres (pg0), its local embedder and reranker | yes |
| LangMem | langmem 0.0.30 | `create_memory_store_manager` defaults, store search | LangGraph InMemoryStore with vector index | yes |

## Status and limits

- **Graphiti output budget:** Graphiti 0.31.2 natively requests 16,384 output tokens for edge
  extraction. This run caps outgoing requests at the same 8,192-token ceiling shared by every
  system on the 32,768-token endpoint, while preserving smaller requests. Each row records this
  deviation from the native default in `output_token_cap_note`. The earlier partial attempt
  failed on the native 16,384-token request; its 105 answers are preserved without a score.
- **LangMem in this GPU window: blocked, no score.** The default memory manager's native
  extraction uses automatic tool selection. The shared endpoint rejected it with HTTP 400:
  `"auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set`.
  The server and pipeline stay unchanged in this window. See `notes/next-window.md` for the retry
  requirement and `results/memory/blocked-rows.json` for the failed attempt.
- **Memvid semantic mode: N/D.** memvid-sdk 2.0.160 assigns vectors to the wrong frames when a
  document is stored as several pages, so semantic search returns wrong documents. Repro and an
  unfiled issue text: `notes/memvid-issue.md`. Only the default lexical mode is reported.
- **Memvid capacity: blocked over 50 MB.** Without `MEMVID_API_KEY` (a Memvid account) a memory
  file holds at most 52,428,800 bytes (MV001 CapacityExceededError). ArguAna already exceeds it, and
  the larger BEIR sets will too, so those Memvid rows are "blocked: needs a Memvid account", not 0.
- **Memvid lexical** returns a hit only when every query word matches (its default), so long
  natural-language queries often return nothing. Failed queries are counted (`failedQueries`).
- **Letta:** current Letta (Letta Code) runs agents on Letta Cloud by default; its local backend is
  experimental. We run the last self-hosted server, 0.16.8, labelled "Letta 0.16.8 self-hosted".
- **Engram:** `mem_search` returns at most 20 results, so Recall@100 is not comparable.
- **LME-S:** systems that call the LLM at ingestion run "LME-S subset n=100" (`lme-s100`, seeded,
  the same 100 questions for every system); retrieval systems run the full 500. The two are never
  mixed in one table.
- **Cognee** is searched with `SearchType.CHUNKS` (vector search over its chunks) so BEIR can rank
  documents; its graph is built at ingestion but not used at query time. Its default search type
  returns an LLM answer, not a ranked list.
- **LME-S subset table:** retrieval systems (Hyperag, Engram, Memvid, Letta) run the full 500; their
  subset rows are the same 100 question ids taken from that run (same namespaces, temperature 0).
- **LME oracle** here is top-10 retrieval over the oracle haystack, labelled "LME oracle haystack,
  top-10 retrieval"; it is not LongMemEval's oracle-session setting and is not compared with it.
- **LongMemEval history format:** retrieved memories are joined in rank order; `run_generation.py`
  sorts session chunks by date and wraps each in a session header, which does not apply to memories.
- **LoCoMo prompt:** the official RAG path (retrieved context + `QA_PROMPT`, no conversation header,
  retrieval with the question only); the category 5 option order uses a seeded coin instead of
  `random.random()`.
- **Answer context:** retrieved memories in rank order up to 90,000 characters (about 22,500
  tokens) for every system, so each prompt fits the endpoint's 32,768-token context.
- **Failures:** a failed search scores 0 (BEIR and memory sets); a run that stops partway is never
  scored (`score_locomo.py` needs the full question count).
- **NVIDIA and quantized rows:** Nemotron 3.5 Embed 8B and Rerank 8B (OpenMDW-1.1) run only on the
  rented H200, next to Llama. The INT8 embed row uses the community build
  kmouratidis/Nemotron-3.5-Embed-8B-INT8-W8A8 (labelled "community INT8 W8A8", not NVIDIA). Q4 embed and
  every quantized rerank row are N/D: no build loads on an H200 with vLLM (rerank exists only as
  NVFP4 for Blackwell GPUs; Q4 embed only as GGUF).
- **Dry runs:** `stub_llm.py` answers every call with the smallest reply that fits the requested
  schema. It checks wiring only; nothing scored against it is reported.
