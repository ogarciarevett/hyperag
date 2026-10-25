# Hyperag

Shared memory and retrieval for agents, with permissions, version history and cited context.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ogarciarevett/hyperag/main/scripts/install.sh | sh
```

Verify:

```sh
~/.local/bin/hyperag --version
```

The installer downloads a prebuilt binary and checks its SHA-256. [Windows, Homebrew, source and package options](docs/install.md). The first public release is prepared; release assets must exist before the installer can download them.

[![CI](https://github.com/ogarciarevett/hyperag/actions/workflows/ci.yml/badge.svg)](https://github.com/ogarciarevett/hyperag/actions/workflows/ci.yml) [![Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

## Run a local stack

The binary needs Postgres with pgvector. The full template also uses Neo4j. To run everything together:

```sh
git clone https://github.com/ogarciarevett/hyperag && cd hyperag
docker compose -p hyperag --profile service up -d
```

The service listens on loopback port 7480. First startup downloads the pinned local embedding and re-ranking models. Container passwords in this example are deliberately fake; configure your own credentials before hosting a shared service.

```sh
# Create an admin token and an agent token. Each is shown once; keep them private.
ADMIN=$(docker compose -p hyperag exec -T hyperag hyperag token admin --file /config/tokens.json)
AGENT=$(docker compose -p hyperag exec -T hyperag hyperag token agent-a --file /config/tokens.json)

# Grant this agent read and write access to a scope.
for permission in read write; do
  curl -fsS localhost:7480/v1/grants -H "authorization: Bearer $ADMIN" -H 'content-type: application/json' \
    -d "{\"principal\":\"agent-a\",\"scope\":\"team\",\"permission\":\"$permission\"}"
done

# Save a decision, then retrieve cited context.
curl -fsS localhost:7480/v1/memories -H "authorization: Bearer $AGENT" -H 'content-type: application/json' \
  -d '{"scope":"team","source":"demo","text":"Failed jobs retry three times with exponential backoff."}'
curl -fsS localhost:7480/v1/context -H "authorization: Bearer $AGENT" -H 'content-type: application/json' \
  -d '{"query":"How many times do failed jobs retry?"}'
```

The result names each memory's version, source and author. The MCP server exposes the same operations using the agent's token; see [MCP setup](docs/mcp.md).

## What it does

- Server-side grants control every search, context and graph path. Revocation takes effect on the next query.
- Corrections retain versions and valid times. Concurrent updates report conflicts instead of silently overwriting each other.
- BM25 and vector retrieval use reciprocal-rank fusion, optional typed graph expansion and re-ranking.
- Context includes numbered citations or an `insufficientEvidence` signal when evidence is weak.
- Models run locally from pinned revisions. Store, embedding, re-ranking and graph slots can be configured independently.

## Benchmarks

Two evaluation windows, October 23–25, 2026. These figures use only completed runs and display **N/D** for unrun or unfinished rows. The default template has graph enabled; the other Hyperag rows explicitly label graph on/off. These are observations, with no confidence intervals or broad quality claim.

| Hyperag configuration | LoCoMo F1 (categories 1–4) | DMR accuracy |
| --- | --- | --- |
| Default template (graph on) | 0.4913 | 0.8220 |
| e5+bgev2m3 (graph off) | 0.4635 | N/D |

![LoCoMo F1 comparison](docs/benchmarks/locomo.svg)

*LoCoMo token F1 over categories 1–4. Every competitor and Hyperag variant is shown; missing results remain unscored.*

![DMR accuracy comparison](docs/benchmarks/dmr.svg)

![LongMemEval oracle comparison](docs/benchmarks/lme-oracle.svg)

![LongMemEval S100 comparison](docs/benchmarks/lme-s100.svg)

![BEIR SciFact comparison](docs/benchmarks/scifact.svg)

![BEIR NFCorpus comparison](docs/benchmarks/nfcorpus.svg)

![BEIR ArguAna comparison](docs/benchmarks/arguana.svg)

How measured: all memory systems use the same answer/judge harness. LoCoMo completes 1,986 questions, with F1 reported over 1,540 category 1–4 questions. DMR and oracle-haystack LongMemEval use 500 questions; S100 uses the fixed 100-question reporting subset. Oracle means a top-10 search over the oracle haystack, not upstream oracle-session retrieval. SciFact uses all 300 official test queries and nDCG@10. Failed native searches remain in scoring; graph-off rows are not the default template.

[Window 1 results](eval/public/results/window-2026-10-24.json) · [Window 2 scores and completion evidence](eval/public/results/window-2026-10-25.json) · [BEIR raw scores](eval/public/results/beir-scores.json) · [Window 1 details](eval/public/notes/window-2026-10-24.md) · [Window 2 details](eval/public/notes/window-2026-10-25.md) · [Final row statuses](eval/public/notes/next-window.md).

No full 500-question LME-S run completed. Outstanding comparisons include several competitors on DMR/oracle/S100, INT8 profiles, endpoint-blocked native tool calls and unfinished BEIR datasets. Partial attempts are not zeros. The full LME-S includes development questions and is not a held-out temporal score; see the [temporal protocol](docs/temporal.md).

Regenerate the figures from result metadata:

```sh
python3 -I eval/public/benchmark_charts.py
```

Graph experiments use two deterministic conversation rules: link each turn to its previous session turn, and link successive mentions of the same extracted entity. BEIR documents have no supplied relationships, so merely enabling graph adds no corpus edges. See [the exact graph rules](eval/public/conversation-graph.ts).

## Architecture

**Rust** (`crates/hyperag`) is the installed product: one binary provides the HTTP service, MCP server, CLI, local models and database adapters.

**TypeScript** (`src/`) is the reference library and HTTP implementation, retained for parity checks and library consumers. The benchmark runners and adapters in `eval/` use TypeScript and Python so every system shares question loading, retrieval budgets and scoring. The TypeScript library is being prepared for npm distribution; running the Rust binary does not require it.

Postgres 18 with pgvector stores versions, grants, BM25 statistics and separate vector indexes per embedding space. Neo4j 5.26 Community supplies typed graph expansion. The default model slots are pinned EmbeddingGemma 2 q8 and bge-reranker-v2-m3 q8; `hyperag models recommend` suggests alternatives for the resources free on your machine. Hyperag returns evidence; your application supplies the answer model.

## Developer tooling

The TypeScript SDK/tooling requires Node 24.22.1 and TypeScript 7.0.2.
Optional Python benchmark environments require uv >=0.13.4 and use the
CA constraints in `eval/public/python-ca-constraints.txt`. Run
`sh scripts/bootstrap-python-ca.sh python3` with Python >=3.10 to bootstrap
hash-verified pip 26.2.2, certifi >=2026.10.19 and uv >=0.13.4 before package downloads.
This does not rerun or change recorded benchmark results.

## Docs

[Install](docs/install.md) · [Concepts](docs/concepts.md) · [Configuration](docs/configuration.md) · [API](docs/api.md) · [MCP](docs/mcp.md) · [Hosting](docs/cloud.md) · [Export and import](docs/export.md) · [Models](docs/models.md) · [Architecture](docs/architecture.md) · [Status and limits](docs/status.md) · [FAQ](docs/faq.md) · [Contributing](CONTRIBUTING.md)

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
