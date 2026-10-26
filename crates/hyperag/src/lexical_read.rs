//! A lexical request must bind authorization, source fence and payloads to one snapshot.
use crate::{
    lexical_consumer::ReaderHandle,
    lexical_snapshot::Checkpoint,
    store::{RANKABLE, Store, VERSION_COLUMNS, bm25_sql, bm25_sum, memory_of},
    types::{Error, Hit, Memory, Result},
};
use chrono::{DateTime, Utc};
use sqlx::{Acquire, PgPool, Postgres, Row, Transaction};
use std::collections::HashMap;

pub(super) struct Read {
    tx: Transaction<'static, Postgres>,
    pub scopes: Vec<String>,
    terms: Vec<String>,
    fence: Checkpoint,
    text: String,
    as_of: Option<DateTime<Utc>>,
    pub used_tantivy: bool,
}

impl Read {
    pub async fn open(
        pool: &PgPool,
        principal: &str,
        text: &str,
        as_of: Option<DateTime<Utc>>,
    ) -> Result<Self> {
        let mut tx = pool.begin().await?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *tx)
            .await?;
        let (scopes, terms, database_id, epoch, fence_token): (Vec<String>, Vec<String>, String, i64, String) = sqlx::query_as(r#"
            select array(select scope from grants where principal=$1 and permission='read' order by scope),
                array(select lexeme from unnest(to_tsvector('simple',$2)) order by lexeme collate "C"),
                database_id::text,epoch,fence_token::text from lexical_index_state
        "#).bind(principal).bind(text).fetch_one(&mut *tx).await?;
        Ok(Self {
            tx,
            scopes,
            terms,
            fence: Checkpoint {
                database_id,
                epoch,
                fence_token,
            },
            text: text.into(),
            as_of,
            used_tantivy: false,
        })
    }
    pub async fn search(&mut self, reader: &ReaderHandle, limit: i64) -> Result<Vec<Hit>> {
        usize::try_from(limit).map_err(|_| Error::InvalidInput("negative lexical limit".into()))?;
        self.used_tantivy = false;
        if let Some(searcher) = reader.at(&self.fence) {
            let scopes = self.scopes.clone();
            let terms = self.terms.clone();
            let at = self.as_of.map(|time| time.timestamp_micros());
            let corpus = tokio::task::spawn_blocking(move || {
                crate::lexical_scorer::sql_corpus(&searcher, &scopes, at, &terms)
            })
            .await;
            if let Ok(Ok(corpus)) = corpus {
                // A savepoint keeps SQL fallback usable if the indexed calculation fails.
                let mut scoring = self.tx.begin().await?;
                let score = bm25_sum();
                let rows = sqlx::query(sqlx::AssertSqlSafe(format!(r#"
                    with st as (select ($1::jsonb->>'n')::float8 as n,
                        ((($1->>'total_len')::numeric) / greatest(($1->>'n')::numeric,1))::float8 as avgdl),
                    postings as (select memory_id,version,extract(epoch from recorded_at)::float8 as at,dl,lexeme,tf
                        from jsonb_to_recordset($1->'postings')
                        as p(memory_id text,version bigint,recorded_at timestamptz,dl float8,lexeme text,tf float8)),
                    df as (select lexeme,count(*)::float8 as df from postings group by lexeme)
                    select p.memory_id as id,p.version,p.at,{score} as score
                        from postings p join df using(lexeme) cross join st
                        group by p.memory_id,p.version,p.at
                        order by score desc,p.at desc,p.memory_id limit $2
                "#))).bind(sqlx::types::Json(corpus)).bind(limit).fetch_all(&mut *scoring).await;
                let rows = match rows {
                    Ok(rows) => {
                        scoring.commit().await?;
                        Some(rows)
                    }
                    Err(_) => {
                        scoring.rollback().await?;
                        None
                    }
                };
                if let Some(rows) = rows {
                    let ids: Vec<String> = rows.iter().map(|row| row.get("id")).collect();
                    let payloads = self.load(&ids).await?;
                    if rows.len() == payloads.len()
                        && rows.iter().zip(&payloads).all(|(row, memory)| {
                            row.get::<String, _>("id") == memory.id
                                && row.get::<i64, _>("version") == i64::from(memory.version)
                        })
                    {
                        self.used_tantivy = true;
                        return Ok(rows
                            .iter()
                            .map(|row| Hit {
                                id: row.get("id"),
                                score: row.get("score"),
                                at: row.get("at"),
                            })
                            .collect());
                    }
                }
            }
        }
        // Replace the entire list; never combine stale/indexed candidates with SQL results.
        let rows = sqlx::query(sqlx::AssertSqlSafe(bm25_sql()))
            .bind(&self.scopes)
            .bind(self.as_of)
            .bind(&self.text)
            .bind(limit)
            .fetch_all(&mut *self.tx)
            .await?;
        Ok(rows
            .iter()
            .map(|row| Hit {
                id: row.get("id"),
                score: row.get("score"),
                at: row.get("at"),
            })
            .collect())
    }
    pub async fn vector(&mut self, space: &str, query: &[f32], limit: i64) -> Result<Vec<Hit>> {
        let mut tx = self.tx.begin().await?;
        match Store::vector_in(&mut tx, space, &self.scopes, query, limit, self.as_of).await {
            Ok(hits) => {
                tx.commit().await?;
                Ok(hits)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
    pub async fn load(&mut self, ids: &[String]) -> Result<Vec<Memory>> {
        if ids.is_empty() || self.scopes.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "select {VERSION_COLUMNS} from memories m join memory_versions v on v.memory_id=m.id where {RANKABLE} and m.id=any($3::text[])"
        ))).bind(&self.scopes).bind(self.as_of).bind(ids).fetch_all(&mut *self.tx).await?;
        let mut by_id: HashMap<String, Memory> = HashMap::new();
        for row in &rows {
            let memory = memory_of(row)?;
            by_id.insert(memory.id.clone(), memory);
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }
    pub async fn close(self) -> Result<()> {
        self.tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "lexical_read_tests.rs"]
mod tests;
