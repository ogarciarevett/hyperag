//! A lexical request must bind authorization, source fence and payloads to one snapshot.
use crate::{
    lexical_consumer::ReaderHandle,
    lexical_snapshot::Checkpoint,
    store::{RANKABLE, VERSION_COLUMNS, bm25_sql, memory_of},
    types::{Error, Hit, Memory, Result},
};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};
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
        let count = usize::try_from(limit)
            .map_err(|_| Error::InvalidInput("negative lexical limit".into()))?;
        self.used_tantivy = false;
        if let Some(searcher) = reader.at(&self.fence) {
            let scopes = self.scopes.clone();
            let terms = self.terms.clone();
            let at = self.as_of.map(|time| time.timestamp_micros());
            let ranked = tokio::task::spawn_blocking(move || {
                crate::lexical_scorer::top(&searcher, &scopes, at, &terms, count)
            })
            .await;
            if let Ok(Ok(ranked)) = ranked {
                let ids: Vec<_> = ranked.iter().map(|hit| hit.id.clone()).collect();
                let payloads = self.load(&ids).await?;
                if ranked.len() == payloads.len()
                    && ranked.iter().zip(&payloads).all(|(hit, memory)| {
                        hit.id == memory.id && hit.version == i64::from(memory.version)
                    })
                {
                    self.used_tantivy = true;
                    return Ok(ranked
                        .into_iter()
                        .map(|hit| Hit {
                            id: hit.id,
                            score: f64::from(hit.score),
                            at: hit.at as f64 / 1_000_000.0,
                        })
                        .collect());
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
