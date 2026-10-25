import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import type { Pool, PoolClient } from "pg";

import { type EmbeddingSpace, type Hit, HyperagError, type Memory } from "../types.js";

// Okapi BM25 constants, as the original agent memory store uses them.
const BM25_K1 = 1.2;
const BM25_B = 0.75;

const MIGRATIONS = [
  new URL("../../crates/hyperag/migrations/0001_init.sql", import.meta.url),
  new URL("../../crates/hyperag/migrations/0002_settings.sql", import.meta.url),
  new URL("../../crates/hyperag/migrations/0003_memory_text_limit.sql", import.meta.url),
];

type VersionRow = {
  id: string;
  scope: string;
  kind: string;
  version: number;
  text: string;
  source: string;
  author: string;
  valid_from: Date | null;
  valid_to: Date | null;
  recorded_at: Date;
  status: Memory["status"];
  status_reason: string | null;
  superseded_by: string | null;
  metadata: Record<string, unknown>;
};

const VERSION_COLUMNS = `m.id, m.scope, m.kind, v.version, v.text, v.source, v.author, v.valid_from,
  v.valid_to, v.recorded_at, v.status, v.status_reason, v.superseded_by, v.metadata`;

/** The rows a caller may rank: current, active, not deleted, in a readable scope, valid at `asOf`. */
const RANKABLE = `m.deleted_at IS NULL AND v.version = m.current_version AND v.status = 'active'
  AND m.scope = ANY($1::text[])
  AND ($2::timestamptz IS NULL OR ((v.valid_from IS NULL OR v.valid_from <= $2)
       AND (v.valid_to IS NULL OR v.valid_to > $2)))`;

/**
 * BM25 over the caller's readable corpus only: corpus statistics come from the rows the caller may
 * read, so another scope's text changes neither which rows match nor how they score. Document
 * length is the distinct lexeme count; the query matches any of its lexemes. From the original
 * agents-runtime store, with the scope widened from one agent to the caller's granted scopes.
 * The `scoped` CTE is read twice, so Postgres materialises it and scans the readable corpus instead
 * of using the GIN index: linear in corpus size, measured in the evaluation.
 */
const BM25_SQL = `
with scoped as (
  select v.memory_id, v.version, v.text_search, extract(epoch from v.recorded_at)::float8 as at
    from memories m join memory_versions v on v.memory_id = m.id
   where ${RANKABLE}
),
stats as (select count(*)::float8 as n, coalesce(avg(length(text_search)), 0)::float8 as avgdl from scoped),
query as (select replace(plainto_tsquery('simple', $3)::text, ' & ', ' | ')::tsquery as q),
terms as (select distinct lexeme from unnest(to_tsvector('simple', $3))),
postings as (
  select s.memory_id, s.version, s.at, length(s.text_search)::float8 as dl, u.lexeme,
         coalesce(array_length(u.positions, 1), 1)::float8 as tf
    from scoped s cross join query cross join lateral unnest(s.text_search) u
   where s.text_search @@ query.q and u.lexeme in (select lexeme from terms)
),
df as (select lexeme, count(*)::float8 as df from postings group by lexeme)
select p.memory_id as id, p.version, p.at,
       sum(ln(1 + (st.n - df.df + 0.5) / (df.df + 0.5)) * (p.tf * (${BM25_K1} + 1))
           / (p.tf + ${BM25_K1} * (1 - ${BM25_B} + ${BM25_B} * p.dl / greatest(st.avgdl, 1)))) as score
  from postings p join df using (lexeme) cross join stats st
 group by p.memory_id, p.version, p.at
 order by score desc, p.at desc, p.memory_id
 limit $4`;

export type PostgresStore = ReturnType<typeof createPostgresStore>;

export function spaceId(space: EmbeddingSpace): string {
  const key = JSON.stringify([
    space.model,
    space.revision,
    space.dimensions,
    space.normalized,
    space.preprocessing,
  ]);
  return createHash("sha256").update(key).digest("hex").slice(0, 16);
}

/** The default stable identity of a memory: what it says, where, from which source. */
export function identityHash(scope: string, source: string, text: string): string {
  return createHash("sha256").update(JSON.stringify([scope, source, text])).digest("hex").slice(0, 24);
}

/** Everything a version records; an update equal to the current version on all of it is a no-op. */
export function versionHash(
  scope: string,
  v: Readonly<{ text: string; source: string; validFrom?: string; validTo?: string; metadata?: Record<string, unknown> }>,
): string {
  const validity = [v.validFrom ?? null, v.validTo ?? null].map((t) => (t === null ? null : new Date(t).toISOString()));
  return createHash("sha256")
    .update(JSON.stringify([scope, v.source, v.text, ...validity, v.metadata ?? {}]))
    .digest("hex");
}

type VersionInput = Readonly<{
  text: string;
  source: string;
  validFrom?: string;
  validTo?: string;
  metadata?: Record<string, unknown>;
}>;

export function createPostgresStore(pool: Pool) {
  async function tx<T>(fn: (c: PoolClient) => Promise<T>): Promise<T> {
    const client = await pool.connect();
    try {
      await client.query("begin");
      const value = await fn(client);
      await client.query("commit");
      return value;
    } catch (error) {
      await client.query("rollback").catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async function insertVersion(
    c: PoolClient,
    id: string,
    version: number,
    author: string,
    hash: string,
    input: VersionInput,
    vector: { space: string; values: number[] } | null,
  ) {
    await c.query(
      `insert into memory_versions (memory_id, version, text, source, author, content_hash,
         valid_from, valid_to, metadata)
       values ($1, $2, $3, $4, $5, $6, $7, $8, $9)`,
      [
        id,
        version,
        input.text,
        input.source,
        author,
        hash,
        input.validFrom ?? null,
        input.validTo ?? null,
        input.metadata ?? {},
      ],
    );
    if (vector !== null)
      await c.query(
        `insert into ${vectorTable(vector.space)} (memory_id, version, embedding) values ($1, $2, $3::vector)`,
        [id, version, vectorLiteral(vector.values)],
      );
  }

  async function idempotent(c: PoolClient, principal: string, key: string | undefined) {
    if (key === undefined) return null;
    const seen = await c.query<{ memory_id: string; version: number }>(
      "select memory_id, version from idempotent_writes where principal = $1 and idempotency_key = $2",
      [principal, key],
    );
    const row = seen.rows[0];
    return row === undefined ? null : { id: row.memory_id, version: row.version, created: false };
  }

  async function remember(
    c: PoolClient,
    principal: string,
    key: string | undefined,
    id: string,
    version: number,
  ) {
    if (key !== undefined)
      await c.query(
        "insert into idempotent_writes (idempotency_key, principal, memory_id, version) values ($1, $2, $3, $4)",
        [key, principal, id, version],
      );
  }

  return {
    async migrate() {
      for (const migration of MIGRATIONS) await pool.query(readFileSync(migration, "utf8"));
    },

    async scopes(principal: string, permission: "read" | "write"): Promise<string[]> {
      const rows = await pool.query<{ scope: string }>(
        "select scope from grants where principal = $1 and permission = $2 order by scope",
        [principal, permission],
      );
      return rows.rows.map((r) => r.scope);
    },

    async grant(principal: string, scope: string, permission: "read" | "write", by: string) {
      await tx(async (c) => {
        await c.query(
          "insert into grants (principal, scope, permission, granted_by) values ($1, $2, $3, $4) on conflict do nothing",
          [principal, scope, permission, by],
        );
        await c.query(
          "insert into grant_log (principal, scope, permission, action, actor) values ($1, $2, $3, 'grant', $4)",
          [principal, scope, permission, by],
        );
      });
    },

    async revoke(principal: string, scope: string, permission: "read" | "write", by: string) {
      await tx(async (c) => {
        await c.query("delete from grants where principal = $1 and scope = $2 and permission = $3", [
          principal,
          scope,
          permission,
        ]);
        await c.query(
          "insert into grant_log (principal, scope, permission, action, actor) values ($1, $2, $3, 'revoke', $4)",
          [principal, scope, permission, by],
        );
      });
    },

    /** Registers a space and creates its own vector table; an existing space is left as it is. */
    async ensureSpace(space: EmbeddingSpace): Promise<string> {
      const id = spaceId(space);
      const table = vectorTable(id);
      await pool.query(
        `insert into embedding_spaces (id, model, revision, dimensions, normalized, preprocessing)
         values ($1, $2, $3, $4, $5, $6) on conflict (id) do nothing`,
        [id, space.model, space.revision, space.dimensions, space.normalized, space.preprocessing],
      );
      await pool.query(`create table if not exists ${table} (
          memory_id text not null, version int not null, embedding vector(${space.dimensions}) not null,
          primary key (memory_id, version),
          foreign key (memory_id, version) references memory_versions (memory_id, version) on delete cascade)`);
      await pool.query(
        `create index if not exists ${table}_hnsw on ${table} using hnsw (embedding vector_cosine_ops)`,
      );
      return id;
    },

    /** Scopes of the live (not deleted) memories among `ids`. */
    async scopeOf(ids: readonly string[]): Promise<Map<string, string>> {
      const rows = await pool.query<{ id: string; scope: string }>(
        "select id, scope from memories where id = any($1::text[]) and deleted_at is null",
        [ids],
      );
      return new Map(rows.rows.map((r) => [r.id, r.scope]));
    },

    async create(
      principal: string,
      input: VersionInput & {
        id: string;
        scope: string;
        kind: string;
        supersedes: readonly string[];
        idempotencyKey?: string;
      },
      hash: string,
      vector: { space: string; values: number[] } | null,
    ) {
      return tx(async (c) => {
        const replay = await idempotent(c, principal, input.idempotencyKey);
        if (replay !== null) return replay;
        const inserted = await c.query(
          `insert into memories (id, scope, kind, created_by, current_version) values ($1, $2, $3, $4, 1)
           on conflict (id) do nothing returning id`,
          [input.id, input.scope, input.kind, principal],
        );
        if (inserted.rowCount === 0) {
          const existing = await c.query<{ current_version: number; content_hash: string; deleted_at: Date | null }>(
            `select m.current_version, v.content_hash, m.deleted_at from memories m
               left join memory_versions v on v.memory_id = m.id and v.version = m.current_version
              where m.id = $1`,
            [input.id],
          );
          const row = existing.rows[0];
          if (row !== undefined && row.deleted_at === null && row.content_hash === hash)
            return { id: input.id, version: row.current_version, created: false };
          throw new HyperagError("already_exists", `memory ${input.id} already exists`, {
            deleted: row?.deleted_at !== null,
          });
        }
        await insertVersion(c, input.id, 1, principal, hash, input, vector);
        // Lock the corrected memories first, so a concurrent update cannot revive what this corrects.
        if (input.supersedes.length > 0)
          await c.query("select id from memories where id = any($1::text[]) order by id for update", [
            input.supersedes,
          ]);
        for (const old of input.supersedes) {
          // The reason is fixed text: the correcting id lives only in superseded_by, which explain masks.
          const marked = await c.query(
            `update memory_versions v set status = 'superseded', superseded_by = $2, status_reason = 'corrected'
              from memories m
             where m.id = v.memory_id and m.id = $1 and v.version = m.current_version
               and m.deleted_at is null and v.status = 'active'`,
            [old, input.id],
          );
          if (marked.rowCount === 0)
            throw new HyperagError("not_active", `memory ${old} is not active`, { memoryId: old });
        }
        await remember(c, principal, input.idempotencyKey, input.id, 1);
        return { id: input.id, version: 1, created: true };
      });
    },

    async update(
      principal: string,
      id: string,
      input: VersionInput & { expectedVersion: number; idempotencyKey?: string },
      hash: string,
      vector: { space: string; values: number[] } | null,
    ) {
      return tx(async (c) => {
        const replay = await idempotent(c, principal, input.idempotencyKey);
        if (replay !== null) return replay;
        // The row lock serialises two writers of one memory; the second sees the first's version.
        // Lock first, then read the head: a join under FOR UPDATE would see the old snapshot's version.
        const locked = await c.query<{ current_version: number }>(
          "select current_version from memories where id = $1 and deleted_at is null for update",
          [id],
        );
        const head = locked.rows[0];
        const current = head === undefined ? undefined : await c.query<{ content_hash: string; status: string }>(
          "select content_hash, status from memory_versions where memory_id = $1 and version = $2",
          [id, head.current_version],
        );
        const row = head === undefined ? undefined : { ...head, ...current?.rows[0] };
        if (row === undefined) throw new HyperagError("not_found", `memory ${id} not found`);
        // A superseded or invalidated fact keeps its reason; it is not silently revived by an update.
        if (row.status !== "active")
          throw new HyperagError("not_active", `memory ${id} is ${row.status}`, { status: row.status });
        if (row.content_hash === hash)
          return { id, version: row.current_version, created: false };
        if (row.current_version !== input.expectedVersion)
          throw new HyperagError("version_conflict", `memory ${id} is at version ${row.current_version}`, {
            currentVersion: row.current_version,
          });
        const next = row.current_version + 1;
        await c.query(
          `update memory_versions set status = 'superseded', status_reason = 'updated to version ' || $3
            where memory_id = $1 and version = $2`,
          [id, row.current_version, next],
        );
        await insertVersion(c, id, next, principal, hash, input, vector);
        await c.query("update memories set current_version = $2 where id = $1", [id, next]);
        await remember(c, principal, input.idempotencyKey, id, next);
        return { id, version: next, created: true };
      });
    },

    async invalidate(id: string, reason: string, by: string) {
      const head = await pool.query<{ status: string }>(
        `select v.status from memories m join memory_versions v on v.memory_id = m.id and v.version = m.current_version
          where m.id = $1 and m.deleted_at is null`,
        [id],
      );
      const status = head.rows[0]?.status;
      if (status !== undefined && status !== "active")
        throw new HyperagError("not_active", `memory ${id} is ${status}`, { status });
      const res = await pool.query(
        `update memory_versions v set status = 'invalidated', status_reason = $2 || ' (by ' || $3 || ')',
           valid_to = coalesce(v.valid_to, now())
          from memories m
         where m.id = v.memory_id and m.id = $1 and v.version = m.current_version and m.deleted_at is null
           and v.status = 'active'`,
        [id, reason, by],
      );
      if (res.rowCount === 0) throw new HyperagError("not_found", `memory ${id} not found`);
    },

    /** Removes every version and vector; the memories row stays as a content-free tombstone. */
    async delete(id: string, reason: string, by: string) {
      await tx(async (c) => {
        const res = await c.query(
          "update memories set deleted_at = now(), deleted_by = $2, delete_reason = $3 where id = $1 and deleted_at is null",
          [id, by, reason],
        );
        if (res.rowCount === 0) throw new HyperagError("not_found", `memory ${id} not found`);
        await c.query("delete from memory_versions where memory_id = $1", [id]);
        await c.query("delete from idempotent_writes where memory_id = $1", [id]);
      });
    },

    async bm25(scopes: readonly string[], text: string, limit: number, asOf: string | null): Promise<Hit[]> {
      if (scopes.length === 0 || text.trim() === "") return [];
      const rows = await pool.query<{ id: string; version: number; score: number; at: number }>(BM25_SQL, [
        scopes,
        asOf,
        text,
        limit,
      ]);
      return rows.rows.map((r) => ({ id: r.id, version: r.version, score: Number(r.score), at: Number(r.at) }));
    },

    async vector(
      space: string,
      scopes: readonly string[],
      query: number[],
      limit: number,
      asOf: string | null,
    ): Promise<Hit[]> {
      if (scopes.length === 0) return [];
      return tx(async (c) => {
        // The iterative scan keeps reading the HNSW graph until enough rows pass the scope filter. The
        // default ef_search (40) found 92% of the exact top 24 on the cross-team corpus; 100 found all.
        await c.query("set local hnsw.iterative_scan = strict_order");
        await c.query(`set local hnsw.ef_search = ${Math.min(1000, Math.max(100, 4 * Math.trunc(limit)))}`);
        const rows = await c.query<{ id: string; version: number; distance: number; at: number }>(
          `select m.id, v.version, e.embedding <=> $3::vector as distance,
                  extract(epoch from v.recorded_at)::float8 as at
             from ${vectorTable(space)} e
             join memory_versions v on v.memory_id = e.memory_id and v.version = e.version
             join memories m on m.id = v.memory_id
            where ${RANKABLE}
            order by e.embedding <=> $3::vector
            limit $4`,
          [scopes, asOf, vectorLiteral(query), limit],
        );
        return rows.rows.map((r) => ({ id: r.id, version: r.version, score: 1 - Number(r.distance), at: Number(r.at) }));
      });
    },

    /** The current, active versions of `ids` the caller may read, in the order of `ids`. */
    async load(ids: readonly string[], scopes: readonly string[], asOf: string | null): Promise<Memory[]> {
      if (ids.length === 0 || scopes.length === 0) return [];
      const rows = await pool.query<VersionRow>(
        `select ${VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id = m.id
          where ${RANKABLE} and m.id = any($3::text[])`,
        [scopes, asOf, ids],
      );
      const byId = new Map(rows.rows.map((r) => [r.id, memoryOf(r)]));
      return ids.flatMap((id) => byId.get(id) ?? []);
    },

    /** Every version of one memory, oldest first, plus its tombstone when deleted. */
    async history(id: string) {
      const head = await pool.query<{
        id: string;
        scope: string;
        kind: string;
        created_by: string;
        created_at: Date;
        current_version: number;
        deleted_at: Date | null;
        deleted_by: string | null;
        delete_reason: string | null;
      }>("select * from memories where id = $1", [id]);
      const memory = head.rows[0];
      if (memory === undefined) return null;
      const versions = await pool.query<VersionRow>(
        `select ${VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id = m.id
          where m.id = $1 order by v.version`,
        [id],
      );
      const supersededBy = await pool.query<{ memory_id: string }>(
        "select distinct memory_id from memory_versions where superseded_by = $1",
        [id],
      );
      return {
        id: memory.id,
        scope: memory.scope,
        kind: memory.kind,
        createdBy: memory.created_by,
        createdAt: memory.created_at.toISOString(),
        currentVersion: memory.current_version,
        deleted:
          memory.deleted_at === null
            ? null
            : { at: memory.deleted_at.toISOString(), by: memory.deleted_by, reason: memory.delete_reason },
        versions: versions.rows.map(memoryOf),
        corrects: supersededBy.rows.map((r) => r.memory_id),
      };
    },

    /** Current active versions with no vector in `space` yet: what a model switch must embed. */
    async missingVectors(space: string, limit: number) {
      const rows = await pool.query<{ id: string; version: number; text: string }>(
        `select m.id, v.version, v.text from memories m
           join memory_versions v on v.memory_id = m.id and v.version = m.current_version
          where m.deleted_at is null and not exists (
            select 1 from ${vectorTable(space)} e where e.memory_id = v.memory_id and e.version = v.version)
          order by m.id limit $1`,
        [limit],
      );
      return rows.rows;
    },

    async putVectors(space: string, rows: ReadonlyArray<{ id: string; version: number; values: number[] }>) {
      for (const row of rows)
        await pool.query(
          `insert into ${vectorTable(space)} (memory_id, version, embedding) values ($1, $2, $3::vector)
           on conflict do nothing`,
          [row.id, row.version, vectorLiteral(row.values)],
        );
    },

    async stats() {
      const rows = await pool.query<{ memories: string; versions: string; bytes: string }>(
        `select (select count(*) from memories where deleted_at is null) as memories,
                (select count(*) from memory_versions) as versions,
                pg_database_size(current_database()) as bytes`,
      );
      const r = rows.rows[0];
      return { memories: Number(r?.memories), versions: Number(r?.versions), databaseBytes: Number(r?.bytes) };
    },
  };
}

function vectorTable(space: string): string {
  if (!/^[0-9a-f]{16}$/u.test(space)) throw new HyperagError("invalid_input", "bad embedding space id");
  return `vectors_${space}`;
}

function vectorLiteral(values: readonly number[]): string {
  if (!values.every(Number.isFinite)) throw new HyperagError("invalid_input", "vector has a non-finite value");
  return `[${values.join(",")}]`;
}

function memoryOf(r: VersionRow): Memory {
  return {
    id: r.id,
    scope: r.scope,
    kind: r.kind,
    version: r.version,
    text: r.text,
    source: r.source,
    author: r.author,
    validFrom: r.valid_from?.toISOString() ?? null,
    validTo: r.valid_to?.toISOString() ?? null,
    recordedAt: r.recorded_at.toISOString(),
    status: r.status,
    statusReason: r.status_reason,
    supersededBy: r.superseded_by,
    metadata: r.metadata,
  };
}
