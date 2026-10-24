-- Hyperag schema. Source text, versions, BM25 and grants live here; vectors live in one table per
-- embedding space (created by the store, `vectors_<space id>`); relationships live in Neo4j.
CREATE EXTENSION IF NOT EXISTS vector;

-- Who may read or write which scope. The service resolves a caller's scopes from this table on
-- every call, so a revoked grant stops the next search, context and graph read.
CREATE TABLE IF NOT EXISTS grants (
  principal text NOT NULL,
  scope text NOT NULL,
  permission text NOT NULL CHECK (permission IN ('read', 'write')),
  granted_by text NOT NULL,
  granted_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (principal, scope, permission)
);

-- Every grant and revocation, with who made it.
CREATE TABLE IF NOT EXISTS grant_log (
  principal text NOT NULL,
  scope text NOT NULL,
  permission text NOT NULL,
  action text NOT NULL CHECK (action IN ('grant', 'revoke')),
  actor text NOT NULL,
  at timestamptz NOT NULL DEFAULT now()
);

-- One row per stable memory identity. A deleted memory keeps this row as its tombstone and loses
-- every version (text, vectors) and its graph node.
CREATE TABLE IF NOT EXISTS memories (
  id text PRIMARY KEY,
  scope text NOT NULL,
  kind text NOT NULL,
  created_by text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  current_version int NOT NULL,
  deleted_at timestamptz,
  deleted_by text,
  delete_reason text
);

-- Every version a memory had. `recorded_at` is when Hyperag learned it; `valid_from`/`valid_to`
-- is when the fact held in the world.
CREATE TABLE IF NOT EXISTS memory_versions (
  memory_id text NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  version int NOT NULL,
  text text NOT NULL CHECK (char_length(text) BETWEEN 1 AND 16000),
  source text NOT NULL,
  author text NOT NULL,
  content_hash text NOT NULL,
  valid_from timestamptz,
  valid_to timestamptz,
  recorded_at timestamptz NOT NULL DEFAULT now(),
  status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded', 'invalidated')),
  status_reason text,
  superseded_by text,
  metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
  text_search tsvector GENERATED ALWAYS AS (to_tsvector('simple', text)) STORED,
  PRIMARY KEY (memory_id, version)
);
CREATE INDEX IF NOT EXISTS memory_versions_text_search_idx ON memory_versions USING gin (text_search);
CREATE INDEX IF NOT EXISTS memories_scope_idx ON memories (scope) WHERE deleted_at IS NULL;

-- Every embedding space ever used. The id hashes all of these fields, so two models with the same
-- dimension never share a vector table.
CREATE TABLE IF NOT EXISTS embedding_spaces (
  id text PRIMARY KEY,
  model text NOT NULL,
  revision text NOT NULL,
  dimensions int NOT NULL,
  normalized boolean NOT NULL,
  preprocessing text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- A write retried with the same idempotency key returns the first result.
CREATE TABLE IF NOT EXISTS idempotent_writes (
  idempotency_key text NOT NULL,
  principal text NOT NULL,
  memory_id text NOT NULL,
  version int NOT NULL,
  at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (principal, idempotency_key)
);
