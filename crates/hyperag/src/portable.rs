//! `hyperag export` and `hyperag import`: everything a Hyperag store knows, in one portable file.
//!
//! The file is JSON Lines. The first line is a header with the format name and version, the last
//! line an end record with the count of every record type, so a cut file is refused. Between them:
//! grants and the grant log (principals and scopes only; tokens never leave the tokens file), every
//! memory with all its versions, valid time, provenance and status (a deleted memory is its
//! tombstone), and every graph relation. Vectors are not exported: the importing store embeds the
//! current versions with the embedder it is configured with, so the user picks the database, the
//! embedder and the re-ranker on import.
//!
//! Import is idempotent: a memory that already exists with the same versions is skipped, one that
//! exists with different versions is a conflict and nothing is written.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::Row;

use crate::core::Hyperag;
use crate::types::{Error, Relation, Result};

pub const FORMAT: &str = "hyperag-export";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub version: i32,
    pub text: String,
    pub source: String,
    pub author: String,
    pub content_hash: String,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub recorded_at: String,
    pub status: String,
    pub status_reason: Option<String>,
    pub superseded_by: Option<String>,
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deleted {
    pub at: String,
    pub by: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "camelCase")]
pub enum Record {
    #[serde(rename_all = "camelCase")]
    Header { format: String, format_version: u32, exported_at: String, hyperag: String },
    #[serde(rename_all = "camelCase")]
    Grant { principal: String, scope: String, permission: String, granted_by: String, granted_at: String },
    #[serde(rename_all = "camelCase")]
    GrantLog { principal: String, scope: String, permission: String, action: String, actor: String, at: String },
    #[serde(rename_all = "camelCase")]
    Memory {
        id: String,
        scope: String,
        kind: String,
        created_by: String,
        created_at: String,
        current_version: i32,
        deleted: Option<Deleted>,
        versions: Vec<Version>,
    },
    #[serde(rename_all = "camelCase")]
    Relation {
        from: String,
        to: String,
        #[serde(rename = "type")]
        kind: String,
        created_by: String,
        created_at: String,
        from_scope: String,
        to_scope: String,
    },
    End { counts: BTreeMap<String, u64> },
}

impl Record {
    fn name(&self) -> &'static str {
        match self {
            Record::Header { .. } => "header",
            Record::Grant { .. } => "grant",
            Record::GrantLog { .. } => "grantLog",
            Record::Memory { .. } => "memory",
            Record::Relation { .. } => "relation",
            Record::End { .. } => "end",
        }
    }
}

/// Full precision (microseconds), so an imported timestamp equals the exported one.
fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_ts(t: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t).map(|t| t.with_timezone(&Utc)).map_err(|_| Error::InvalidInput(format!("bad timestamp {t:?}")))
}

fn version_record(v: &sqlx::postgres::PgRow) -> Version {
    let metadata = match v.get::<Value, _>("metadata") { Value::Object(m) => m, _ => Map::new() };
    Version {
        version: v.get("version"), text: v.get("text"), source: v.get("source"), author: v.get("author"),
        content_hash: v.get("content_hash"), valid_from: v.get::<Option<DateTime<Utc>>, _>("valid_from").map(ts),
        valid_to: v.get::<Option<DateTime<Utc>>, _>("valid_to").map(ts), recorded_at: ts(v.get("recorded_at")),
        status: v.get("status"), status_reason: v.get("status_reason"), superseded_by: v.get("superseded_by"), metadata,
    }
}

fn memory_record(m: &sqlx::postgres::PgRow, versions: Vec<Version>) -> Record {
    let deleted = m.get::<Option<DateTime<Utc>>, _>("deleted_at").map(|at| Deleted { at: ts(at), by: m.get("deleted_by"), reason: m.get("delete_reason") });
    Record::Memory { id: m.get("id"), scope: m.get("scope"), kind: m.get("kind"), created_by: m.get("created_by"),
        created_at: ts(m.get("created_at")), current_version: m.get("current_version"), deleted, versions }
}

const PAGE: i64 = 500;

/// Writes the whole store (and the graph, when configured) to `out`. Returns the counts.
pub async fn export(h: &Hyperag, out: &mut impl Write) -> anyhow::Result<BTreeMap<String, u64>> {
    let mut tx = h.store.pool.begin().await?;
    sqlx::query("set transaction isolation level repeatable read read only").execute(&mut *tx).await?;
    let mut counts = BTreeMap::new();
    let mut emit = |out: &mut dyn Write, r: &Record| -> anyhow::Result<()> {
        if !matches!(r, Record::Header { .. } | Record::End { .. }) {
            *counts.entry(r.name().to_owned()).or_insert(0) += 1;
        }
        serde_json::to_writer(&mut *out, r)?;
        out.write_all(b"\n")?;
        Ok(())
    };
    let header = Record::Header { format: FORMAT.into(), format_version: FORMAT_VERSION, exported_at: ts(Utc::now()), hyperag: env!("CARGO_PKG_VERSION").into() };
    emit(out, &header)?;
    for r in sqlx::query("select * from grants order by principal, scope, permission").fetch_all(&mut *tx).await? {
        let g = Record::Grant { principal: r.get("principal"), scope: r.get("scope"), permission: r.get("permission"), granted_by: r.get("granted_by"), granted_at: ts(r.get("granted_at")) };
        emit(out, &g)?;
    }
    for r in sqlx::query("select * from grant_log order by at, principal, scope, permission, action").fetch_all(&mut *tx).await? {
        let g = Record::GrantLog { principal: r.get("principal"), scope: r.get("scope"), permission: r.get("permission"), action: r.get("action"), actor: r.get("actor"), at: ts(r.get("at")) };
        emit(out, &g)?;
    }
    let mut after = String::new();
    loop {
        let heads = sqlx::query("select * from memories where id > $1 order by id limit $2").bind(&after).bind(PAGE).fetch_all(&mut *tx).await?;
        let Some(last) = heads.last() else { break };
        after = last.get("id");
        let ids: Vec<String> = heads.iter().map(|r| r.get("id")).collect();
        let mut versions: BTreeMap<String, Vec<Version>> = BTreeMap::new();
        for v in sqlx::query("select * from memory_versions where memory_id = any($1::text[]) order by memory_id, version").bind(&ids).fetch_all(&mut *tx).await? {
            versions.entry(v.get("memory_id")).or_default().push(version_record(&v));
        }
        for m in &heads {
            let id: String = m.get("id");
            emit(out, &memory_record(m, versions.remove(&id).unwrap_or_default()))?;
        }
    }
    tx.commit().await?;
    // Neo4j is a separate store; callers must quiesce graph writes for a cross-store snapshot.
    if let Some(graph) = &h.graph {
        let mut skip = 0;
        loop {
            let page = graph.all_relations(skip, PAGE).await?;
            for (r, from_scope, to_scope) in &page {
                let record = Record::Relation { from: r.from.clone(), to: r.to.clone(), kind: r.kind.clone(), created_by: r.created_by.clone(), created_at: r.created_at.clone(), from_scope: from_scope.clone(), to_scope: to_scope.clone() };
                emit(out, &record)?;
            }
            if (page.len() as i64) < PAGE {
                break;
            }
            skip += PAGE;
        }
    }
    let end = Record::End { counts: counts.clone() };
    serde_json::to_writer(&mut *out, &end)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(counts)
}

/// Reads and checks a whole export file: header first, end last, counts equal to the records.
pub fn read(input: impl BufRead) -> Result<Vec<Record>> {
    let mut records = vec![];
    for (n, line) in input.lines().enumerate() {
        let line = line.map_err(|e| Error::InvalidInput(format!("line {}: {e}", n + 1)))?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str::<Record>(&line).map_err(|e| Error::InvalidInput(format!("line {}: {e}", n + 1)))?);
    }
    match records.first() {
        Some(Record::Header { format, format_version, .. }) if format == FORMAT && *format_version == FORMAT_VERSION => {}
        Some(Record::Header { format, format_version, .. }) => {
            return Err(Error::InvalidInput(format!("unsupported file: {format} version {format_version} (this build reads {FORMAT} version {FORMAT_VERSION})")));
        }
        _ => return Err(Error::InvalidInput("not a Hyperag export: the first line is not a header".into())),
    }
    let Some(Record::End { counts }) = records.last() else {
        return Err(Error::InvalidInput("the file is cut: no end record".into()));
    };
    let mut seen: BTreeMap<String, u64> = BTreeMap::new();
    for r in &records[1..records.len() - 1] {
        if matches!(r, Record::Header { .. } | Record::End { .. }) {
            return Err(Error::InvalidInput("a header or end record in the middle of the file".into()));
        }
        *seen.entry(r.name().to_owned()).or_insert(0) += 1;
    }
    if &seen != counts {
        return Err(Error::InvalidInput(format!("record counts {seen:?} differ from the end record {counts:?}")));
    }
    Ok(records)
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub memories: u64,
    pub skipped: u64,
    pub versions: u64,
    pub grants: u64,
    pub grant_log: u64,
    pub relations: u64,
}

/// Writes `records` into the store in one transaction, then the relations into the graph.
/// Embedding is the caller's next step (`Hyperag::reindex` with the chosen embedder).
pub async fn import(h: &Hyperag, records: &[Record], without_graph: bool) -> Result<Imported> {
    let relations = records.iter().filter(|r| matches!(r, Record::Relation { .. })).count();
    if relations > 0 && h.graph.is_none() && !without_graph {
        return Err(Error::InvalidInput(format!("the file has {relations} relations and no graph is configured (HYPERAG_NEO4J_URL); pass --without-graph to drop them")));
    }
    let mut done = Imported::default();
    let mut tx = h.store.pool.begin().await?;
    // One importer per database makes each history check and insert atomic against other imports.
    sqlx::query("select pg_advisory_xact_lock(7480, 1)").execute(&mut *tx).await?;
    for r in records {
        match r {
            Record::Grant { principal, scope, permission, granted_by, granted_at } => {
                let n = sqlx::query("insert into grants (principal, scope, permission, granted_by, granted_at) values ($1, $2, $3, $4, $5) on conflict do nothing")
                    .bind(principal).bind(scope).bind(permission).bind(granted_by).bind(parse_ts(granted_at)?)
                    .execute(&mut *tx).await?;
                done.grants += n.rows_affected();
            }
            Record::GrantLog { principal, scope, permission, action, actor, at } => {
                let n = sqlx::query(
                    "insert into grant_log (principal, scope, permission, action, actor, at)
                     select $1, $2, $3, $4, $5, $6 where not exists (
                       select 1 from grant_log where principal = $1 and scope = $2 and permission = $3 and action = $4 and actor = $5 and at = $6)",
                )
                .bind(principal).bind(scope).bind(permission).bind(action).bind(actor).bind(parse_ts(at)?)
                .execute(&mut *tx).await?;
                done.grant_log += n.rows_affected();
            }
            Record::Memory { id, scope, kind, created_by, created_at, current_version, deleted, versions } => {
                let existing = sqlx::query("select * from memories where id = $1").bind(id).fetch_optional(&mut *tx).await?;
                if let Some(e) = existing {
                    let rows = sqlx::query("select * from memory_versions where memory_id = $1 order by version")
                        .bind(id).fetch_all(&mut *tx).await?;
                    let stored = memory_record(&e, rows.iter().map(version_record).collect());
                    if &stored != r {
                        return Err(Error::AlreadyExists { message: format!("memory {id} exists with different history; nothing was imported"), deleted: deleted.is_some() });
                    }
                    done.skipped += 1;
                    continue;
                }
                if !deleted.is_some() && !versions.iter().any(|v| v.version == *current_version) {
                    return Err(Error::InvalidInput(format!("memory {id}: current version {current_version} is not in the file")));
                }
                sqlx::query(
                    "insert into memories (id, scope, kind, created_by, created_at, current_version, deleted_at, deleted_by, delete_reason)
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                )
                .bind(id).bind(scope).bind(kind).bind(created_by).bind(parse_ts(created_at)?).bind(current_version)
                .bind(deleted.as_ref().map(|d| parse_ts(&d.at)).transpose()?)
                .bind(deleted.as_ref().and_then(|d| d.by.clone()))
                .bind(deleted.as_ref().and_then(|d| d.reason.clone()))
                .execute(&mut *tx).await?;
                for v in versions {
                    sqlx::query(
                        "insert into memory_versions (memory_id, version, text, source, author, content_hash, valid_from, valid_to,
                           recorded_at, status, status_reason, superseded_by, metadata)
                         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                    )
                    .bind(id).bind(v.version).bind(&v.text).bind(&v.source).bind(&v.author).bind(&v.content_hash)
                    .bind(v.valid_from.as_deref().map(parse_ts).transpose()?)
                    .bind(v.valid_to.as_deref().map(parse_ts).transpose()?)
                    .bind(parse_ts(&v.recorded_at)?).bind(&v.status).bind(&v.status_reason).bind(&v.superseded_by)
                    .bind(Value::Object(v.metadata.clone()))
                    .execute(&mut *tx).await?;
                    done.versions += 1;
                }
                done.memories += 1;
            }
            Record::Header { .. } | Record::End { .. } | Record::Relation { .. } => {}
        }
    }
    tx.commit().await?;
    if let (Some(graph), false) = (&h.graph, without_graph) {
        // Graph writes are MERGEs, so a re-import or a retry after a graph failure is a no-op.
        for r in records {
            if let Record::Memory { id, scope, deleted: None, .. } = r {
                graph.upsert_memory(id, scope).await?;
            }
        }
        for r in records {
            if let Record::Relation { from, to, kind, created_by, created_at, from_scope, to_scope } = r {
                let rel = Relation { from: from.clone(), to: to.clone(), kind: kind.clone(), created_by: created_by.clone(), created_at: created_at.clone() };
                graph.relate(&rel, from_scope, to_scope).await?;
                done.relations += 1;
            }
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(records: &[Record]) -> String {
        records.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect()
    }
    fn header() -> Record {
        Record::Header { format: FORMAT.into(), format_version: FORMAT_VERSION, exported_at: "2026-10-24T00:00:00.000000Z".into(), hyperag: "0.1.0".into() }
    }
    fn grant() -> Record {
        Record::Grant { principal: "a".into(), scope: "s".into(), permission: "read".into(), granted_by: "admin".into(), granted_at: "2026-10-24T00:00:00.000000Z".into() }
    }

    #[test]
    fn reads_a_complete_file_and_refuses_a_cut_or_foreign_one() {
        let end = Record::End { counts: BTreeMap::from([("grant".to_owned(), 1)]) };
        let ok = file(&[header(), grant(), end.clone()]);
        assert_eq!(read(ok.as_bytes()).unwrap().len(), 3);
        // Cut: the end record is missing.
        assert!(read(file(&[header(), grant()]).as_bytes()).is_err());
        // Counts that do not match the records.
        assert!(read(file(&[header(), grant(), grant(), end.clone()]).as_bytes()).is_err());
        // Another format or a newer version.
        let other = Record::Header { format: FORMAT.into(), format_version: 2, exported_at: String::new(), hyperag: String::new() };
        assert!(read(file(&[other, grant(), end]).as_bytes()).is_err());
        assert!(read("{\"record\":\"nope\"}\n".as_bytes()).is_err());
    }

    #[test]
    fn records_round_trip_through_json() {
        let m = Record::Memory {
            id: "s/1".into(),
            scope: "s".into(),
            kind: "fact".into(),
            created_by: "a".into(),
            created_at: "2026-10-24T00:00:00.123456Z".into(),
            current_version: 2,
            deleted: None,
            versions: vec![Version {
                version: 2,
                text: "t".into(),
                source: "src".into(),
                author: "a".into(),
                content_hash: "h".into(),
                valid_from: Some("2026-01-01T00:00:00.000000Z".into()),
                valid_to: None,
                recorded_at: "2026-10-24T00:00:00.123456Z".into(),
                status: "active".into(),
                status_reason: None,
                superseded_by: None,
                metadata: Map::new(),
            }],
        };
        let line = serde_json::to_string(&m).unwrap();
        assert!(line.starts_with("{\"record\":\"memory\""));
        assert_eq!(serde_json::from_str::<Record>(&line).unwrap(), m);
        assert_eq!(parse_ts("2026-10-24T00:00:00.123456Z").map(ts).unwrap(), "2026-10-24T00:00:00.123456Z");
    }

    // Explicit database check: HYPERAG_PG_URL=<isolated stack> cargo test portable::tests::postgres_ -- --ignored
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires an isolated Postgres with pgvector"]
    async fn postgres_snapshot_and_history_conflict() {
        use crate::{Config, Hyperag};
        use crate::store::Store;
        use sqlx::postgres::PgPoolOptions;
        let url = std::env::var("HYPERAG_PG_URL").expect("isolated Postgres URL required");
        let admin = PgPoolOptions::new().connect(&url).await.unwrap();
        let name = format!("rs_portable_{}", std::process::id());
        sqlx::query(sqlx::AssertSqlSafe(format!("create database {name}"))).execute(&admin).await.unwrap();
        let (base, _) = url.rsplit_once('/').unwrap();
        let pool = PgPoolOptions::new().connect(&format!("{base}/{name}")).await.unwrap();
        let h = Hyperag::init(Config::new(Store::new(pool.clone()))).await.unwrap();
        sqlx::raw_sql("insert into grants values ('alice','s','read','admin',now());
            insert into memories (id,scope,kind,created_by,current_version) values ('m','s','fact','alice',1);
            insert into memory_versions (memory_id,version,text,source,author,content_hash) values ('m',1,'old','test','alice','same-hash')")
            .execute(&pool).await.unwrap();

        // Change the source after the grants query establishes the export snapshot.
        struct OnGrant {
            bytes: Vec<u8>,
            lines: usize,
            change: Option<Box<dyn FnOnce() + Send>>,
        }
        impl Write for OnGrant {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                self.lines += bytes.iter().filter(|b| **b == b'\n').count();
                if self.lines == 2 {
                    if let Some(change) = self.change.take() { change(); }
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let other = pool.clone();
        let mut out = OnGrant { bytes: vec![], lines: 0, change: Some(Box::new(move || {
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    sqlx::query("update memory_versions set text = 'new' where memory_id = 'm'")
                        .execute(&other).await.unwrap();
                });
            }).join().unwrap();
        })) };
        export(&h, &mut out).await.unwrap();
        let snapshot = read(out.bytes.as_slice()).unwrap();
        let snapshot_old = snapshot.iter().any(|r| matches!(r, Record::Memory { versions, .. } if versions[0].text == "old"));

        let mut current = vec![];
        export(&h, &mut current).await.unwrap();
        let mut changed = read(current.as_slice()).unwrap();
        for r in &mut changed {
            if let Record::Memory { versions, .. } = r {
                versions[0].status = "invalidated".into();
                versions[0].status_reason = Some("changed with the same content hash".into());
            }
        }
        changed.insert(1, Record::Grant { principal: "bob".into(), scope: "s".into(), permission: "read".into(), granted_by: "admin".into(), granted_at: "2026-01-01T00:00:00Z".into() });
        let conflict = import(&h, &changed, true).await;
        let extra_grants: i64 = sqlx::query_scalar("select count(*) from grants where principal = 'bob'").fetch_one(&pool).await.unwrap();
        // A revoked-grants-only archive must remain idempotent under concurrent imports.
        sqlx::raw_sql("create function rs_delay_log() returns trigger language plpgsql as $$ begin perform pg_sleep(0.2); return new; end $$;
            create trigger rs_delay_log before insert on grant_log for each row execute function rs_delay_log()")
            .execute(&pool).await.unwrap();
        let log_only = vec![Record::GrantLog { principal: "revoked".into(), scope: "s".into(), permission: "read".into(), action: "revoke".into(), actor: "admin".into(), at: "2026-01-01T00:00:00Z".into() }];
        let (one, two) = tokio::join!(import(&h, &log_only, true), import(&h, &log_only, true));
        one.unwrap(); two.unwrap();
        let logs: i64 = sqlx::query_scalar("select count(*) from grant_log where principal = 'revoked'").fetch_one(&pool).await.unwrap();

        // Force a write failure in the child process, preserving an existing .part target.
        sqlx::query("update memory_versions set text = repeat('x', 8192)").execute(&pool).await.unwrap();
        let dir = std::env::temp_dir().join(format!("rs-portable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("archive.part");
        std::fs::write(&dest, b"previous archive").unwrap();
        let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/hyperag");
        let failed = std::process::Command::new("bash").args(["-c", "ulimit -f 1; trap '' XFSZ; exec \"$@\"", "export-limit"])
            .arg(bin).args(["export", "--out"]).arg(&dest)
            .env("HYPERAG_PG_URL", format!("{base}/{name}"))
            .env("HYPERAG_EMBEDDING", "none").env("HYPERAG_RERANKER", "none").env("HYPERAG_NEO4J_URL", "")
            .output().unwrap();
        let archive_preserved = std::fs::read(&dest).unwrap() == b"previous archive";
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!failed.status.success(), "fixture must force an export error");
        assert!(String::from_utf8_lossy(&failed.stderr).contains("File too large"), "fixture must fail while writing, not opening the store");
        pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database {name} with (force)"))).execute(&admin).await.unwrap();
        admin.close().await;
        assert!(snapshot_old, "export mixed snapshots: a concurrent update leaked into the file");
        assert!(matches!(conflict, Err(Error::AlreadyExists { .. })), "changed status must conflict even with the same content hash");
        assert_eq!(extra_grants, 0, "conflict must roll back earlier grant writes");
        assert_eq!((archive_preserved, logs), (true, 1), "failed .part export must preserve the old file; concurrent imports must not duplicate history");
    }

}
