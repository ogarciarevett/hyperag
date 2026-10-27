# Hosting Hyperag (cloud)

Hyperag runs the same way on a server as on a laptop: one service, Postgres with pgvector, and Neo4j. A hosted copy serves every agent of a team, each with its own token. This page describes the source blueprint for a Railway template. It has not been deployed or validated against a Railway project. A hosted deployment has a monthly cost.

## Layout

| Service | Image | Network | Volume |
| --- | --- | --- | --- |
| `hyperag` | built from this repository's `Dockerfile` | private, plus one public HTTPS domain for agents | `/data`: model cache and tokens file |
| `postgres` | `pgvector/pgvector:pg18` (pinned digest, as in `docker-compose.yml`) | private only | `/var/lib/postgresql` |
| `neo4j` | `neo4j:5.26.28-community` (pinned digest) | private only | `/data` |

- The graph is on, as in the default template. Hyperag reaches Postgres and Neo4j by their private domain names, so neither database is exposed to the internet.
- The models run inside the Hyperag service on CPU (EmbeddingGemma 2 and bge-reranker-v2-m3, about 1.9 GB of memory in use). The first start downloads them (about 900 MB) into the volume and checks each file against the hub's SHA-256. Later starts load them offline.
- Agents connect over HTTPS to the public domain: the HTTP API at `/v1`, and MCP at `/mcp`.

## Tokens: one per principal

Each agent or person is a principal with its own token. The service keeps only the SHA-256 of each token, in `/data/config/tokens.json`, and re-reads the file when it changes, so no restart is needed. The first start creates an empty registry. In the Railway dashboard, open the Hyperag service’s [Console tab](https://railway.com/changelog/2026-05-29-cli-volumes) container shell and issue the first admin token:

```sh
hyperag token admin --file /data/config/tokens.json
```

The command generates a fresh secret and prints it once: keep it private and use it as a bearer token. `HYPERAG_ADMINS=admin` names the principal, not the token. Issue separate agent tokens with `hyperag token alice --file /data/config/tokens.json`. No plaintext token belongs in the template or repository.

What a principal may read or write comes from grants that an admin (`HYPERAG_ADMINS`) makes with `POST /v1/grants`. Revoking a grant takes effect on the next call.

## Template source

[`.railway/railway.ts`](../.railway/railway.ts) declares the three services, their pinned sources, volumes, healthcheck and environment variables using Railway’s IaC helpers. It is a blueprint for the [template composer](https://docs.railway.com/templates/create), not a command to provision a project.

The database password values use Railway’s template-only `${{secret(64, "abcdef0123456789")}}` function. The composer generates a different password for every deployment; these expressions are not passwords and do not generate values during ordinary IaC apply. Enter them as generated template variables, or replace them in the composer with required user-supplied secrets. Do not paste the unresolved functions into an existing service’s variables.

- `POSTGRES_PASSWORD` is generated on `postgres`; Hyperag’s connection URL references that value. Hex characters keep the URL safe without escaping.
- `NEO4J_PASSWORD` is generated on `neo4j`; both `NEO4J_AUTH` and Hyperag’s login reference it, so they cannot drift.
- `HYPERAG_ADMINS=admin` is a non-secret principal name. Tokens are generated in the container console as described above.
- Postgres 18 keeps `PGDATA=/var/lib/postgresql/18/docker` below its volume root. Neo4j explicitly listens on IPv6 for Railway’s private network.

## Template composer settings

An authorized template maintainer can copy this blueprint into Railway’s [template editor](https://railway.com/workspace/templates), without a CLI:

1. Add `hyperag` from `ogarciarevett/hyperag` on `main`, using the root `Dockerfile`; add `postgres` and `neo4j` from the exact image references in the blueprint.
2. Copy each service’s variables, including the generated password expressions and cross-service references. Attach the three volumes at the paths and sizes declared in the blueprint. `RAILWAY_RUN_UID=0` lets Hyperag write its Railway volume; the Docker image otherwise defaults to uid 10001.
3. Enable **HTTP public networking only on `hyperag`**, target port `7480`. This supplies `RAILWAY_PUBLIC_DOMAIN` for `HYPERAG_MCP_ALLOWED_HOSTS`. Give neither database a public domain or TCP proxy.
4. Set Hyperag’s healthcheck path to `/healthz` and timeout to `600` seconds for the initial model download. Keep a single Hyperag replica.
5. Review the generated variables, sources, volume paths, networking and cost before deploying. Create and share the template URL through the dashboard; replace the README placeholder only once that URL is available.

The repository does not create an account, token, project or template. A template URL is still pending; the README’s `<TEMPLATE_URL>` is deliberately a placeholder, not a working deployment link.

## Cost (estimate, not measured)

From Railway's prices on 2026-10-24 (memory about $10 per GB per month, vCPU about $20 per vCPU per month, volumes about $0.15 per GB per month, Hobby plan $5 per month including $5 of usage), for the three services always on:

| Part | Assumption | Per month |
| --- | --- | --- |
| Memory | Hyperag with its models about 2 GB, Neo4j about 1 GB (512 MB heap, 256 MB page cache), Postgres about 0.2 GB | about $32 |
| CPU | about 0.2 vCPU on average for a small team; embedding and re-ranking run only on writes and queries | about $4 |
| Volumes | 11 GB (5 + 2 + 4) | about $2 |
| Total on Hobby | after the $5 included usage | about $38 |

Heavier use raises the CPU line. The Hobby plan allows volumes up to 5 GB each, which fits this layout.

## Backups and moving

`hyperag export --out memory.jsonl` (run inside the service, or anywhere with `HYPERAG_PG_URL` and `HYPERAG_NEO4J_URL`) writes everything to one file, and `hyperag import` loads it into another stack, local or hosted, with the models chosen there ([export and import](export.md)).

## Limits

- Source validation does not prove a live Railway deployment. The template composer must resolve generated and reference variables, attach volumes and configure the public domain. Database readiness, private DNS, model download and authenticated HTTP/MCP must be checked on a fresh deployment before describing it as verified.
- One Hyperag replica: the model files and tokens live on one volume. More replicas need shared token storage.
- Neo4j Community is GPL-3.0 and runs as its own service, unmodified.
