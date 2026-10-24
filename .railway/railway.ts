// Source blueprint for Railway's template composer; see docs/cloud.md.
// secret() expressions run when a template is deployed, not during ordinary IaC apply.
// Only Hyperag gets public HTTPS in the composer; neither database gets a domain or TCP proxy.
import { defineRailway, github, image, project, service, volume } from "railway/iac";

export default defineRailway(() => {
  // The same pinned image as docker-compose.yml, so pgvector is present.
  const postgres = service("postgres", {
    source: image("pgvector/pgvector:pg18@sha256:2358fcba361ed2233a5ed81b5fe4ca779ccb304120ce531a3bf51c0ed7e2bc11"),
    env: {
      POSTGRES_USER: "hyperag",
      POSTGRES_DB: "hyperag",
      POSTGRES_PASSWORD: '${{secret(64, "abcdef0123456789")}}',
      // Postgres 18 stores clusters below the versioned directory.
      PGDATA: "/var/lib/postgresql/18/docker",
    },
    volumeMounts: { "/var/lib/postgresql": volume("pgdata", { sizeMB: 5120 }) },
  });

  const neo4j = service("neo4j", {
    source: image("neo4j:5.26.28-community@sha256:ff32db30b2baff97971e441b46bfd9c832c1b62c970398ef579244c06b21d357"),
    env: {
      // Generate once; NEO4J_AUTH and Hyperag both reference this same value.
      NEO4J_PASSWORD: '${{secret(64, "abcdef0123456789")}}',
      NEO4J_AUTH: "neo4j/${{NEO4J_PASSWORD}}",
      NEO4J_server_http_listen__address: "[::]:7474",
      NEO4J_server_bolt_listen__address: "[::]:7687",
      NEO4J_server_memory_heap_max__size: "512m",
      NEO4J_server_memory_pagecache_size: "256m",
    },
    volumeMounts: { "/data": volume("neo4jdata", { sizeMB: 2048 }) },
  });

  // Built from the repository's Dockerfile. One volume holds the model cache and the tokens file.
  const hyperag = service("hyperag", {
    source: github("ogarciarevett/hyperag", { branch: "main" }),
    healthcheck: "/healthz",
    // The first start downloads the default models (about 900 MB).
    healthcheckTimeout: 600,
    env: {
      HYPERAG_PG_URL: "postgres://hyperag:${{postgres.POSTGRES_PASSWORD}}@${{postgres.RAILWAY_PRIVATE_DOMAIN}}:5432/hyperag",
      HYPERAG_NEO4J_URL: "http://${{neo4j.RAILWAY_PRIVATE_DOMAIN}}:7474",
      HYPERAG_NEO4J_USER: "neo4j",
      HYPERAG_NEO4J_PASSWORD: neo4j.env.NEO4J_PASSWORD,
      // This is a principal name, not a credential; issue its token in the service console.
      HYPERAG_ADMINS: "admin",
      // Listen on IPv6 and IPv4, including the local MCP callback.
      HYPERAG_HOST: "::",
      HYPERAG_PORT: "7480",
      PORT: "7480",
      HYPERAG_MODEL_CACHE: "/data/models",
      HYPERAG_TOKENS_FILE: "/data/config/tokens.json",
      // MCP at /mcp checks the Host header; the service's own public domain, once one is added.
      HYPERAG_MCP_ALLOWED_HOSTS: "${{RAILWAY_PUBLIC_DOMAIN}}",
      // The image runs as uid 10001; Railway volumes mount as root, so the service runs as root to write them.
      RAILWAY_RUN_UID: "0",
    },
    volumeMounts: { "/data": volume("hyperagdata", { sizeMB: 4096 }) },
  });

  return project("hyperag", { resources: [postgres, neo4j, hyperag] });
});
