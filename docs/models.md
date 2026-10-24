# Models


One string chooses each model; switching an embedding builds its own index first and keeps documents, BM25 and the graph.

```sh
hyperag models list --available
hyperag models set embedding=hf:BAAI/bge-small-en-v1.5 --as admin
hyperag models set embedding='hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8' reranker=hf:BAAI/bge-reranker-base --as admin
```

Specs: `hf:<repo>[?revision=&dtype=fp32|fp16|q8|q4&pooling=mean|cls|none&output=&dims=]`, `http:<url>?model=&dims=`, `nvidia:<model>?dims=`, `cloudflare:<model>?dims=`, `none`.

Presets (fastembed): `hyperag models list --available` prints every id. Examples:

| Spec | Model | Dimensions |
| --- | --- | --- |
| `hf:onnx-community/embeddinggemma-2-ONNX?revision=daa72c51243991dfcaf9f9137d2c573d8f7790c0&dtype=q8&pooling=none&output=sentence_embedding&dims=768` | EmbeddingGemma 2, q8 (the default) | 768 |
| `hf:Xenova/multilingual-e5-small?revision=761b726dd34fb83930e26aab4e9ac3899aa1fa78&dtype=q8&pooling=mean` | multilingual e5 small, q8 | 384 |
| `hf:Qdrant/bge-small-en-v1.5-onnx-Q` | BGE small English, q8 | 384 |
| `hf:onnx-community/embeddinggemma-300m-ONNX?dtype=q8` | EmbeddingGemma 300m, q8 | 768 |
| `hf:onnx-community/bge-reranker-v2-m3-ONNX?revision=6f5ff65298512715a1e669753bc754d2bc8f367b&dtype=q8` | bge-reranker-v2-m3, q8 (the default) | re-ranker |
| `hf:BAAI/bge-reranker-base` | BGE re-ranker base | re-ranker |

Each spec records model, revision, dimensions, normalisation and query/document prompts. Their hash is the embedding space; each space has its own vector table, so models never share an index even at the same dimension. A switch embeds every current memory into the new space first, then makes it active; the old index stays for a switch back.

Remote providers are configured but never called by tests: `http:` (any OpenAI-compatible `/v1/embeddings`, key in `HYPERAG_EMBEDDING_API_KEY`), `nvidia:` (`NVIDIA_API_KEY`), `cloudflare:` (`CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN`). Not every model runs on every machine; local ONNX models run on CPU.

## Downloading and licences

`hyperag models pull [--embedding <spec>] [--reranker <spec>]` downloads the active (or given) models into `HYPERAG_MODEL_CACHE`, checks every large file against the SHA-256 Hugging Face publishes for the pinned revision, shows each model's licence (the card's, or its base model's when the conversion declares none) and records it in `licences.json`. The service downloads and checks a missing model on first start too, so `docker compose up` fills the `models` volume once; only `pull` writes `licences.json`. A fastembed preset (a spec without options, such as `hf:BAAI/bge-small-en-v1.5`) is downloaded by fastembed into the same cache, without Hyperag's hash check.

Spec options for `hf:`: `revision=` (pin a commit), `dtype=fp32|fp16|q8|q4`, `pooling=mean|cls|none`, `output=<tensor>` (a model's own pooled output, with `pooling=none`), `dims=`. For `http:`: `model=`, `dims=`, `truncate=true` (keep the first `dims` values and re-normalise, for Matryoshka models), `normalized=true`.

## Recommending models

`hyperag models recommend [--json]` reads free memory and swap (macOS `vm_stat` and `sysctl`, Linux `/proc/meminfo`), CPU cores and free GPU memory (`nvidia-smi`), and picks the best listed model per slot that fits: memory = parameters x bytes per parameter x 1.2 + batch working set, within free memory minus 2 GB (halved while the machine swaps). GPU-only models are suggested only when a CUDA GPU has room for them.
