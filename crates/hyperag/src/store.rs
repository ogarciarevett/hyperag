//! The Postgres store: source text, versions, BM25, grants, one vector table per embedding space.
//! A port of `src/adapters/postgres.ts`; it applies the same migration file.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use pgvector::Vector;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{Postgres, Row, Transaction};

use crate::types::{EmbeddingSpace, Error, Hit, Memory, Result, WriteResult, not_found};

/// Dynamic SQL here only splices validated table names and constants, never caller text.
macro_rules! sql {
    ($($t:tt)*) => { sqlx::AssertSqlSafe(format!($($t)*)) };
}

const MIGRATIONS: [&str; 4] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_settings.sql"),
    include_str!("../migrations/0003_memory_text_limit.sql"),
    include_str!("../migrations/0004_lexical_cache.sql"),
];

// Legacy corpus BM25 remains available for runtime comparison and rollback.
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// The rows a caller may rank: current, active, not deleted, in a readable scope, valid at `asOf`.
const RANKABLE: &str = "m.deleted_at IS NULL AND v.version = m.current_version AND v.status = 'active'
  AND m.scope = ANY($1::text[])
  AND ($2::timestamptz IS NULL OR ((v.valid_from IS NULL OR v.valid_from <= $2)
       AND (v.valid_to IS NULL OR v.valid_to > $2)))";

const VERSION_COLUMNS: &str = "m.id, m.scope, m.kind, v.version, v.text, v.source, v.author, v.valid_from,
  v.valid_to, v.recorded_at, v.status, v.status_reason, v.superseded_by, v.metadata";

/// BM25 over the caller's readable corpus only (see the TypeScript store for the full rationale).
fn bm25_sql() -> String {
    format!(
        "with scoped as (
  select v.memory_id, v.version, v.text_search, extract(epoch from v.recorded_at)::float8 as at
    from memories m join memory_versions v on v.memory_id = m.id
   where {RANKABLE}
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
select p.memory_id as id, p.at,
       sum(ln(1 + (st.n - df.df + 0.5) / (df.df + 0.5)) * (p.tf * ({BM25_K1} + 1))
           / (p.tf + {BM25_K1} * (1 - {BM25_B} + {BM25_B} * p.dl / greatest(st.avgdl, 1)))) as score
  from postings p join df using (lexeme) cross join stats st
 group by p.memory_id, p.version, p.at
 order by score desc, p.at desc, p.memory_id
 limit $4"
    )
}

/// Per-term indexed candidates preserve rare-term coverage before the final budget. Only the query
/// vector is unpacked; document vectors are scored in PostgreSQL without expanding their lexemes.
fn fast_lexical_sql() -> String {
    format!(
        "with terms as materialized (
  select lexeme, plainto_tsquery('simple', lexeme) as q
    from unnest(to_tsvector('simple', $3))
   where ts_lexize('english_stem', lexeme) is distinct from '{{}}'::text[]
),
per_term as materialized (
  select t.lexeme, h.*, row_number() over (
    partition by t.lexeme order by h.score desc, h.at desc, h.id
  ) as term_rank
    from terms t cross join lateral (
      select v.memory_id as id, v.version,
             extract(epoch from v.recorded_at)::float8 as at,
             ts_rank_cd(v.text_search, t.q)::float8 as score
        from memory_versions v
       where v.text_search @@ t.q
         and exists (select 1 from memories m where m.id = v.memory_id and {RANKABLE})
       order by score desc, at desc, id
       limit $4
    ) h
),
covered as (
  select id, version, at, min(term_rank) as best_term_rank,
         count(*) as terms_matched, sum(1.0 / (60.0 + term_rank))::float8 as score
    from per_term group by id, version, at
),
candidates as materialized (
  select * from covered
   order by best_term_rank, terms_matched desc, score desc, at desc, id
   limit $4
)
select id, at, score from candidates
 order by best_term_rank, terms_matched desc, score desc, at desc, id"
    )
}

#[derive(Debug, PartialEq)]
enum LexicalScorer { Bm25, Fast }

impl LexicalScorer {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            None | Some("bm25") => Ok(Self::Bm25),
            Some("fast") => Ok(Self::Fast),
            _ => Err(Error::InvalidInput("HYPERAG_LEXICAL_SCORER must be bm25 or fast".into())),
        }
    }
    fn selected() -> Result<Self> {
        Self::parse(std::env::var("HYPERAG_LEXICAL_SCORER").ok().as_deref())
    }
}

pub fn space_id(space: &EmbeddingSpace) -> String {
    let key = serde_json::to_string(&(
        &space.model,
        &space.revision,
        space.dimensions,
        space.normalized,
        &space.preprocessing,
    ))
    .expect("serialisable");
    hex::encode(Sha256::digest(key.as_bytes()))[..16].to_string()
}

/// The default stable identity of a memory: what it says, where, from which source.
pub fn identity_hash(scope: &str, source: &str, text: &str) -> String {
    let key = serde_json::to_string(&(scope, source, text)).expect("serialisable");
    hex::encode(Sha256::digest(key.as_bytes()))[..24].to_string()
}

/// Everything a version records; an update equal to the current version on all of it is a no-op.
pub fn version_hash(scope: &str, v: &VersionInput) -> String {
    let iso = |t: &Option<String>| t.as_deref().and_then(parse_time).map(iso_ms);
    let key = serde_json::to_string(&(
        scope,
        &v.source,
        &v.text,
        iso(&v.valid_from),
        iso(&v.valid_to),
        v.metadata.clone().unwrap_or_default(),
    ))
    .expect("serialisable");
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// RFC 3339, or a date or date-time without a zone (read as UTC).
pub fn parse_time(t: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t).ok().map(|d| d.with_timezone(&Utc)).or_else(|| {
        ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"]
            .iter()
            .find_map(|f| chrono::NaiveDateTime::parse_from_str(t, f).ok())
            .or_else(|| chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))
            .map(|d| d.and_utc())
    })
}

/// The same text as JavaScript's `Date.toISOString()`.
pub fn iso_ms(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[derive(Clone, Debug, Default)]
pub struct VersionInput {
    pub text: String,
    pub source: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    pub at: String,
    pub by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub created_by: String,
    pub created_at: String,
    pub current_version: i32,
    pub deleted: Option<Tombstone>,
    pub versions: Vec<Memory>,
    pub corrects: Vec<String>,
}

pub struct VectorRow {
    pub space: String,
    pub values: Vec<f32>,
}

#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
}

fn vector_table(space: &str) -> Result<String> {
    if space.len() == 16 && space.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        Ok(format!("vectors_{space}"))
    } else {
        Err(Error::InvalidInput("bad embedding space id".into()))
    }
}

fn memory_of(r: &PgRow) -> Result<Memory> {
    let t = |c: &str| -> Result<Option<String>> { Ok(r.try_get::<Option<DateTime<Utc>>, _>(c)?.map(iso_ms)) };
    let metadata: Value = r.try_get("metadata")?;
    Ok(Memory {
        id: r.try_get("id")?,
        scope: r.try_get("scope")?,
        kind: r.try_get("kind")?,
        version: r.try_get("version")?,
        text: r.try_get("text")?,
        source: r.try_get("source")?,
        author: r.try_get("author")?,
        valid_from: t("valid_from")?,
        valid_to: t("valid_to")?,
        recorded_at: iso_ms(r.try_get("recorded_at")?),
        status: r.try_get("status")?,
        status_reason: r.try_get("status_reason")?,
        superseded_by: r.try_get("superseded_by")?,
        metadata: match metadata {
            Value::Object(m) => m,
            _ => Map::new(),
        },
    })
}

fn time_param(t: &Option<String>) -> Result<Option<DateTime<Utc>>> {
    match t {
        None => Ok(None),
        Some(s) => parse_time(s).map(Some).ok_or_else(|| Error::InvalidInput(format!("not a date: {s}"))),
    }
}

async fn insert_version(
    c: &mut Transaction<'_, Postgres>,
    id: &str,
    version: i32,
    author: &str,
    hash: &str,
    input: &VersionInput,
    vector: Option<&VectorRow>,
) -> Result<()> {
    sqlx::query(
        "insert into memory_versions (memory_id, version, text, source, author, content_hash,
           valid_from, valid_to, metadata) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(version)
    .bind(&input.text)
    .bind(&input.source)
    .bind(author)
    .bind(hash)
    .bind(time_param(&input.valid_from)?)
    .bind(time_param(&input.valid_to)?)
    .bind(Value::Object(input.metadata.clone().unwrap_or_default()))
    .execute(&mut **c)
    .await?;
    if let Some(v) = vector {
        sqlx::query(sql!(
            "insert into {} (memory_id, version, embedding) values ($1, $2, $3)",
            vector_table(&v.space)?
        ))
        .bind(id)
        .bind(version)
        .bind(Vector::from(v.values.clone()))
        .execute(&mut **c)
        .await?;
    }
    Ok(())
}

async fn idempotent(c: &mut Transaction<'_, Postgres>, principal: &str, key: &Option<String>) -> Result<Option<WriteResult>> {
    let Some(key) = key else { return Ok(None) };
    let row = sqlx::query("select memory_id, version from idempotent_writes where principal = $1 and idempotency_key = $2")
        .bind(principal)
        .bind(key)
        .fetch_optional(&mut **c)
        .await?;
    Ok(row.map(|r| WriteResult { id: r.get("memory_id"), version: r.get("version"), created: false }))
}

async fn remember(c: &mut Transaction<'_, Postgres>, principal: &str, key: &Option<String>, id: &str, version: i32) -> Result<()> {
    if let Some(key) = key {
        sqlx::query("insert into idempotent_writes (idempotency_key, principal, memory_id, version) values ($1, $2, $3, $4)")
            .bind(key)
            .bind(principal)
            .bind(id)
            .bind(version)
            .execute(&mut **c)
            .await?;
    }
    Ok(())
}

pub struct CreateInput<'a> {
    pub id: &'a str,
    pub scope: &'a str,
    pub kind: &'a str,
    pub supersedes: &'a [String],
    pub idempotency_key: &'a Option<String>,
    pub version: VersionInput,
}

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Store { pool }
    }

    /// Acquire before any source row/table lock; shared with triggers and bounded backfill.
    pub(crate) async fn begin_write(&self) -> Result<Transaction<'_, Postgres>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(1213811052,1)").execute(&mut *tx).await?;
        Ok(tx)
    }

    pub async fn migrate(&self) -> Result<()> {
        LexicalScorer::selected()?;
        let mut c = self.begin_write().await?;
        for m in MIGRATIONS {
            sqlx::raw_sql(m).execute(&mut *c).await?;
        }
        c.commit().await?;
        Ok(())
    }

    pub async fn scopes(&self, principal: &str, permission: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("select scope from grants where principal = $1 and permission = $2 order by scope")
            .bind(principal)
            .bind(permission)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn grant(&self, principal: &str, scope: &str, permission: &str, by: &str) -> Result<()> {
        let mut c = self.begin_write().await?;
        sqlx::query("insert into grants (principal, scope, permission, granted_by) values ($1, $2, $3, $4) on conflict do nothing")
            .bind(principal).bind(scope).bind(permission).bind(by).execute(&mut *c).await?;
        sqlx::query("insert into grant_log (principal, scope, permission, action, actor) values ($1, $2, $3, 'grant', $4)")
            .bind(principal).bind(scope).bind(permission).bind(by).execute(&mut *c).await?;
        c.commit().await?;
        Ok(())
    }

    pub async fn revoke(&self, principal: &str, scope: &str, permission: &str, by: &str) -> Result<()> {
        let mut c = self.begin_write().await?;
        sqlx::query("delete from grants where principal = $1 and scope = $2 and permission = $3")
            .bind(principal).bind(scope).bind(permission).execute(&mut *c).await?;
        sqlx::query("insert into grant_log (principal, scope, permission, action, actor) values ($1, $2, $3, 'revoke', $4)")
            .bind(principal).bind(scope).bind(permission).bind(by).execute(&mut *c).await?;
        c.commit().await?;
        Ok(())
    }

    /// Registers a space and creates its own vector table; an existing space is left as it is.
    pub async fn ensure_space(&self, space: &EmbeddingSpace) -> Result<String> {
        let mut c = self.begin_write().await?;
        let id = space_id(space);
        let table = vector_table(&id)?;
        sqlx::query(
            "insert into embedding_spaces (id, model, revision, dimensions, normalized, preprocessing)
             values ($1, $2, $3, $4, $5, $6) on conflict (id) do nothing",
        )
        .bind(&id).bind(&space.model).bind(&space.revision).bind(space.dimensions as i32).bind(space.normalized).bind(&space.preprocessing)
        .execute(&mut *c)
        .await?;
        sqlx::raw_sql(sql!(
            "create table if not exists {table} (
               memory_id text not null, version int not null, embedding vector({}) not null,
               primary key (memory_id, version),
               foreign key (memory_id, version) references memory_versions (memory_id, version) on delete cascade);
             create index if not exists {table}_hnsw on {table} using hnsw (embedding vector_cosine_ops);",
            space.dimensions
        ))
        .execute(&mut *c)
        .await?;
        c.commit().await?;
        Ok(id)
    }

    /// Scopes of the live (not deleted) memories among `ids`.
    pub async fn scope_of(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        let rows = sqlx::query("select id, scope from memories where id = any($1::text[]) and deleted_at is null")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("scope"))).collect())
    }

    pub async fn create(&self, principal: &str, input: &CreateInput<'_>, hash: &str, vector: Option<&VectorRow>) -> Result<WriteResult> {
        let mut c = self.begin_write().await?;
        if let Some(replay) = idempotent(&mut c, principal, input.idempotency_key).await? {
            return Ok(replay);
        }
        let inserted = sqlx::query(
            "insert into memories (id, scope, kind, created_by, current_version) values ($1, $2, $3, $4, 1)
             on conflict (id) do nothing",
        )
        .bind(input.id).bind(input.scope).bind(input.kind).bind(principal)
        .execute(&mut *c)
        .await?;
        if inserted.rows_affected() == 0 {
            let row = sqlx::query(
                "select m.current_version, v.content_hash, m.deleted_at is not null as deleted from memories m
                   left join memory_versions v on v.memory_id = m.id and v.version = m.current_version
                  where m.id = $1",
            )
            .bind(input.id)
            .fetch_optional(&mut *c)
            .await?;
            let deleted = row.as_ref().is_some_and(|r| r.get::<bool, _>("deleted"));
            if let Some(r) = &row {
                let existing: Option<String> = r.get("content_hash");
                if !deleted && existing.as_deref() == Some(hash) {
                    return Ok(WriteResult { id: input.id.into(), version: r.get("current_version"), created: false });
                }
            }
            return Err(Error::AlreadyExists { message: format!("memory {} already exists", input.id), deleted });
        }
        insert_version(&mut c, input.id, 1, principal, hash, &input.version, vector).await?;
        // Lock the corrected memories first, so a concurrent update cannot revive what this corrects.
        if !input.supersedes.is_empty() {
            sqlx::query("select id from memories where id = any($1::text[]) order by id for update")
                .bind(input.supersedes)
                .execute(&mut *c)
                .await?;
        }
        for old in input.supersedes {
            // The reason is fixed text: the correcting id lives only in superseded_by, which explain masks.
            let marked = sqlx::query(
                "update memory_versions v set status = 'superseded', superseded_by = $2, status_reason = 'corrected'
                  from memories m
                 where m.id = v.memory_id and m.id = $1 and v.version = m.current_version
                   and m.deleted_at is null and v.status = 'active'",
            )
            .bind(old).bind(input.id)
            .execute(&mut *c)
            .await?;
            if marked.rows_affected() == 0 {
                return Err(Error::NotActive { message: format!("memory {old} is not active"), status: "not active".into(), memory_id: Some(old.clone()) });
            }
        }
        remember(&mut c, principal, input.idempotency_key, input.id, 1).await?;
        c.commit().await?;
        Ok(WriteResult { id: input.id.into(), version: 1, created: true })
    }

    pub async fn update(
        &self,
        principal: &str,
        id: &str,
        input: &VersionInput,
        expected_version: i32,
        idempotency_key: &Option<String>,
        hash: &str,
        vector: Option<&VectorRow>,
    ) -> Result<WriteResult> {
        let mut c = self.begin_write().await?;
        if let Some(replay) = idempotent(&mut c, principal, idempotency_key).await? {
            return Ok(replay);
        }
        // Lock first, then read the head: a join under FOR UPDATE would see the old snapshot's version.
        let current: i32 = sqlx::query_scalar("select current_version from memories where id = $1 and deleted_at is null for update")
            .bind(id)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(|| not_found(id))?;
        let head = sqlx::query("select content_hash, status from memory_versions where memory_id = $1 and version = $2")
            .bind(id).bind(current)
            .fetch_one(&mut *c)
            .await?;
        let status: String = head.get("status");
        if status != "active" {
            return Err(Error::NotActive { message: format!("memory {id} is {status}"), status, memory_id: None });
        }
        if head.get::<String, _>("content_hash") == hash {
            return Ok(WriteResult { id: id.into(), version: current, created: false });
        }
        if current != expected_version {
            return Err(Error::VersionConflict { message: format!("memory {id} is at version {current}"), current_version: current });
        }
        let next = current + 1;
        sqlx::query("update memory_versions set status = 'superseded', status_reason = 'updated to version ' || $3 where memory_id = $1 and version = $2")
            .bind(id).bind(current).bind(next.to_string())
            .execute(&mut *c)
            .await?;
        insert_version(&mut c, id, next, principal, hash, input, vector).await?;
        sqlx::query("update memories set current_version = $2 where id = $1").bind(id).bind(next).execute(&mut *c).await?;
        remember(&mut c, principal, idempotency_key, id, next).await?;
        c.commit().await?;
        Ok(WriteResult { id: id.into(), version: next, created: true })
    }

    pub async fn invalidate(&self, id: &str, reason: &str, by: &str) -> Result<()> {
        let mut c = self.begin_write().await?;
        let status: Option<String> = sqlx::query_scalar(
            "select v.status from memories m join memory_versions v on v.memory_id = m.id and v.version = m.current_version
              where m.id = $1 and m.deleted_at is null",
        )
        .bind(id)
        .fetch_optional(&mut *c)
        .await?;
        match status.as_deref() {
            None => return Err(not_found(id)),
            Some("active") => {}
            Some(s) => return Err(Error::NotActive { message: format!("memory {id} is {s}"), status: s.into(), memory_id: None }),
        }
        let res = sqlx::query(
            "update memory_versions v set status = 'invalidated', status_reason = $2 || ' (by ' || $3 || ')',
               valid_to = coalesce(v.valid_to, now())
              from memories m
             where m.id = v.memory_id and m.id = $1 and v.version = m.current_version and m.deleted_at is null
               and v.status = 'active'",
        )
        .bind(id).bind(reason).bind(by)
        .execute(&mut *c)
        .await?;
        if res.rows_affected() == 0 {
            return Err(not_found(id));
        }
        c.commit().await?;
        Ok(())
    }

    /// Removes every version and vector; the memories row stays as a content-free tombstone.
    pub async fn delete(&self, id: &str, reason: &str, by: &str) -> Result<()> {
        let mut c = self.begin_write().await?;
        let res = sqlx::query("update memories set deleted_at = now(), deleted_by = $2, delete_reason = $3 where id = $1 and deleted_at is null")
            .bind(id).bind(by).bind(reason)
            .execute(&mut *c)
            .await?;
        if res.rows_affected() == 0 {
            return Err(not_found(id));
        }
        sqlx::query("delete from memory_versions where memory_id = $1").bind(id).execute(&mut *c).await?;
        sqlx::query("delete from idempotent_writes where memory_id = $1").bind(id).execute(&mut *c).await?;
        c.commit().await?;
        Ok(())
    }

    pub async fn bm25(&self, scopes: &[String], text: &str, limit: i64, as_of: Option<DateTime<Utc>>) -> Result<Vec<Hit>> {
        if scopes.is_empty() || text.trim().is_empty() {
            return Ok(vec![]);
        }
        let sql = match LexicalScorer::selected()? { LexicalScorer::Bm25 => bm25_sql(), LexicalScorer::Fast => fast_lexical_sql() };
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(scopes).bind(as_of).bind(text).bind(limit).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| Hit { id: r.get("id"), score: r.get("score"), at: r.get("at") }).collect())
    }

    pub async fn vector(&self, space: &str, scopes: &[String], query: &[f32], limit: i64, as_of: Option<DateTime<Utc>>) -> Result<Vec<Hit>> {
        if scopes.is_empty() {
            return Ok(vec![]);
        }
        let table = vector_table(space)?;
        let mut c = self.pool.begin().await?;
        // The iterative scan keeps reading the HNSW graph until enough rows pass the scope filter. The
        // default ef_search (40) found 92% of the exact top 24 on the cross-team corpus; 100 found all.
        sqlx::query("set local hnsw.iterative_scan = strict_order").execute(&mut *c).await?;
        sqlx::query(sql!("set local hnsw.ef_search = {}", (4 * limit).clamp(100, 1000))).execute(&mut *c).await?;
        let rows = sqlx::query(sql!(
            "select m.id, (e.embedding <=> $3)::float8 as distance, extract(epoch from v.recorded_at)::float8 as at
               from {table} e
               join memory_versions v on v.memory_id = e.memory_id and v.version = e.version
               join memories m on m.id = v.memory_id
              where {RANKABLE}
              order by e.embedding <=> $3
              limit $4"
        ))
        .bind(scopes).bind(as_of).bind(Vector::from(query.to_vec())).bind(limit)
        .fetch_all(&mut *c)
        .await?;
        c.commit().await?;
        Ok(rows.iter().map(|r| Hit { id: r.get("id"), score: 1.0 - r.get::<f64, _>("distance"), at: r.get("at") }).collect())
    }

    /// The current, active versions of `ids` the caller may read, in the order of `ids`.
    pub async fn load(&self, ids: &[String], scopes: &[String], as_of: Option<DateTime<Utc>>) -> Result<Vec<Memory>> {
        if ids.is_empty() || scopes.is_empty() {
            return Ok(vec![]);
        }
        let rows = sqlx::query(sql!(
            "select {VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id = m.id where {RANKABLE} and m.id = any($3::text[])"
        ))
        .bind(scopes).bind(as_of).bind(ids)
        .fetch_all(&self.pool)
        .await?;
        let mut by_id: HashMap<String, Memory> = HashMap::new();
        for r in &rows {
            let m = memory_of(r)?;
            by_id.insert(m.id.clone(), m);
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// The newest current, active memories of one scope, newest first.
    pub async fn recent(&self, scope: &str, limit: i64) -> Result<Vec<Memory>> {
        let rows = sqlx::query(sql!(
            "select {VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id = m.id
              where m.deleted_at is null and v.version = m.current_version and v.status = 'active' and m.scope = $1
              order by v.recorded_at desc, m.id limit $2"
        ))
        .bind(scope)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(memory_of).collect()
    }

    /// Every version of one memory, oldest first, plus its tombstone when deleted.
    pub async fn history(&self, id: &str) -> Result<Option<History>> {
        let Some(head) = sqlx::query("select * from memories where id = $1").bind(id).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let versions = sqlx::query(sql!(
            "select {VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id = m.id where m.id = $1 order by v.version"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let corrects: Vec<String> = sqlx::query_scalar("select distinct memory_id from memory_versions where superseded_by = $1")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        let deleted_at: Option<DateTime<Utc>> = head.get("deleted_at");
        Ok(Some(History {
            id: head.get("id"),
            scope: head.get("scope"),
            kind: head.get("kind"),
            created_by: head.get("created_by"),
            created_at: iso_ms(head.get("created_at")),
            current_version: head.get("current_version"),
            deleted: deleted_at.map(|at| Tombstone { at: iso_ms(at), by: head.get("deleted_by"), reason: head.get("delete_reason") }),
            versions: versions.iter().map(memory_of).collect::<Result<_>>()?,
            corrects,
        }))
    }

    /// Current active versions with no vector in `space` yet: what a model switch must embed.
    pub async fn missing_vectors(&self, space: &str, limit: i64) -> Result<Vec<(String, i32, String)>> {
        let table = vector_table(space)?;
        let rows = sqlx::query(sql!(
            "select m.id, v.version, v.text from memories m
               join memory_versions v on v.memory_id = m.id and v.version = m.current_version
              where m.deleted_at is null and not exists (
                select 1 from {table} e where e.memory_id = v.memory_id and e.version = v.version)
              order by m.id limit $1"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("id"), r.get("version"), r.get("text"))).collect())
    }

    pub async fn put_vectors(&self, space: &str, rows: &[(String, i32, Vec<f32>)]) -> Result<()> {
        let mut c = self.begin_write().await?;
        let table = vector_table(space)?;
        for (id, version, values) in rows {
            sqlx::query(sql!("insert into {table} (memory_id, version, embedding) values ($1, $2, $3) on conflict do nothing"))
                .bind(id).bind(version).bind(Vector::from(values.clone()))
                .execute(&mut *c)
                .await?;
        }
        c.commit().await?;
        Ok(())
    }

    /// Ids among `ids` that already exist (live or tombstoned).
    pub async fn existing(&self, ids: &[String]) -> Result<HashSet<String>> {
        let rows: Vec<String> = sqlx::query_scalar("select id from memories where id = any($1::text[])").bind(ids).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn spaces(&self) -> Result<Vec<(String, EmbeddingSpace, i64)>> {
        let rows = sqlx::query("select id, model, revision, dimensions, normalized, preprocessing from embedding_spaces order by created_at")
            .fetch_all(&self.pool)
            .await?;
        let mut out = vec![];
        for r in rows {
            let id: String = r.get("id");
            let count: i64 = sqlx::query_scalar(sql!("select count(*) from {}", vector_table(&id)?)).fetch_one(&self.pool).await?;
            out.push((
                id,
                EmbeddingSpace {
                    model: r.get("model"),
                    revision: r.get("revision"),
                    dimensions: r.get::<i32, _>("dimensions") as usize,
                    normalized: r.get("normalized"),
                    preprocessing: r.get("preprocessing"),
                },
                count,
            ));
        }
        Ok(out)
    }

    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("select value from settings where key = $1").bind(key).fetch_optional(&self.pool).await?)
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let mut c = self.begin_write().await?;
        sqlx::query("insert into settings (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value, updated_at = now()")
            .bind(key).bind(value)
            .execute(&mut *c)
            .await?;
        c.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod lexical_scorer_tests {
    use super::*;
    #[test]
    fn lexical_scorer_defaults_to_bm25_and_rejects_unknown_settings() {
        assert_eq!(LexicalScorer::parse(None).unwrap(), LexicalScorer::Bm25);
        assert_eq!(LexicalScorer::parse(Some("bm25")).unwrap(), LexicalScorer::Bm25);
        assert_eq!(LexicalScorer::parse(Some("fast")).unwrap(), LexicalScorer::Fast);
        assert!(LexicalScorer::parse(Some("unknown")).is_err());
    }
}
