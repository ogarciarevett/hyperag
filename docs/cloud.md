# Hosting Hyperag (cloud)

Hyperag runs the same way on a server as on a laptop: one service, Postgres with pgvector, and Neo4j. A hosted copy serves every agent of a team, each with its own token. This page describes the Railway layout. It is a plan in this repository: nothing is deployed, and a hosted deployment has a monthly cost that its owner approves first.

## Layout

| Service | Image | Network | Volume |
| --- | --- | --- | --- |
| `hyperag` | built from this repository's `Dockerfile` | private, plus one public HTTPS domain for agents | `/data`: model cache and tokens file |
| `postgres` | `pgvector/pgvector:pg18` (pinned digest, as in `docker-compose.yml`) | private only | `/var/lib/postgresql/data` |
| `neo4j` | `neo4j:5.26.28-community` (pinned digest) | private only | `/data` |

- The graph is on, as in the default template. Hyperag reaches Postgres and Neo4j by their private domain names, so neither database is exposed to the internet.
- The models run inside the Hyperag service on CPU (EmbeddingGemma 2 and bge-reranker-v2-m3, about 1.9 GB of memory in use). The first start downloads them (about 900 MB) into the volume and checks each file against the hub's SHA-256. Later starts load them offline.
- Agents connect over HTTPS to the public domain: the HTTP API at `/v1`, and MCP at `/mcp`.

## Tokens: one per principal

Each agent or person is a principal with its own token. The service keeps only the SHA-256 of each token, in `/data/config/tokens.json`, and re-reads the file when it changes, so no restart is needed. Make a token from a shell inside the service:

```sh
railway ssh --service hyperag -- hyperag token alice --file /data/config/tokens.json   # prints the token once
```

What a principal may read or write comes from grants that an admin (`HYPERAG_ADMINS`) makes with `POST /v1/grants`. Revoking a grant takes effect on the next call.

## Configuration as code

`.railway/railway.ts` declares the three services, their volumes and their variables. It uses Railway's infrastructure-as-code format. Secrets are not in the file: `preserve()` keeps values set in Railway. Set these in Railway right after the first apply (each one redeploys its service):

- `POSTGRES_PASSWORD` on `postgres`
- `NEO4J_PASSWORD` on `neo4j` (Neo4j's login and Hyperag's both read it, so they cannot drift apart)
- `HYPERAG_ADMINS` on `hyperag`

From a project linked with `railway link`, review the plan before applying it:

```sh
railway config plan    # shows what would be created; changes nothing
railway config apply   # only after the plan is reviewed
```

## Owner steps, in order

The owner's local setup script (kept outside the repository) runs steps 1 to 4, and stops before anything that costs money unless you type `yes`:

1. `railway link`: pick or create the project.
2. `railway config plan`: read the plan.
3. `railway config apply`: creates the three services (paid from here on).
4. Set `POSTGRES_PASSWORD` on `postgres` and `NEO4J_PASSWORD` on `neo4j`, then `HYPERAG_ADMINS` on `hyperag`, in that order. Each change redeploys its service. Until then the first deploys of `postgres` and `neo4j` fail without a password, and `hyperag` cannot reach them. The script generates the passwords and never prints them, then creates a private template.
5. Add a public domain to `hyperag` in the dashboard. Until then `HYPERAG_MCP_ALLOWED_HOSTS` is empty and `/mcp` accepts only local hosts.
6. Make the first token: `railway ssh --service hyperag -- hyperag token admin --file /data/config/tokens.json`.

## Cost (estimate, not measured)

From Railway's prices on 2026-10-24 (memory about $10 per GB per month, vCPU about $20 per vCPU per month, volumes about $0.15 per GB per month, Hobby plan $5 per month including $5 of usage), for the three services always on:

| Part | Assumption | Per month |
| --- | --- | --- |
| Memory | Hyperag with its models about 2 GB, Neo4j about 1 GB (512 MB heap, 256 MB page cache), Postgres about 0.2 GB | about $32 |
| CPU | about 0.2 vCPU on average for a small team; embedding and re-ranking run only on writes and queries | about $4 |
| Volumes | 11 GB (5 + 2 + 4) | about $2 |
| Total on Hobby | after the $5 included usage | about $38 |

Heavier use raises the CPU line. The Hobby plan allows volumes up to 5 GB each, which fits this layout.

## One-click template

A Railway template made from this layout gives a "Deploy on Railway" button. The template is made from a project with `railway templates create`. It is published only when the repository is public. Until then the README shows no live button.

## Backups and moving

`hyperag export --out memory.jsonl` (run inside the service, or anywhere with `HYPERAG_PG_URL` and `HYPERAG_NEO4J_URL`) writes everything to one file, and `hyperag import` loads it into another stack, local or hosted, with the models chosen there ([export and import](export.md)).

## Limits

- `railway config plan` has not run against a project. The file uses the helpers in Railway's IaC reference. The composed values use Railway's `${{service.VARIABLE}}` variable syntax, which the reference does not show inside IaC; the first plan must confirm they resolve. The healthcheck timeout allows for the first model download.
- One Hyperag replica: the model files and tokens live on one volume. More replicas need shared token storage.
- Neo4j Community is GPL-3.0 and runs as its own service, unmodified.
