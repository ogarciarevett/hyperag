//! A lexical request must bind authorization, source fence and payloads to one snapshot.
use crate::{
    lexical_consumer::ReaderHandle,
    lexical_snapshot::Checkpoint,
    types::{Hit, Memory, Result},
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

pub(super) struct Read {
    pub scopes: Vec<String>,
    terms: Vec<String>,
    fence: Checkpoint,
    pub used_tantivy: bool,
}

impl Read {
    pub async fn open(_: &PgPool, _: &str, _: &str, _: Option<DateTime<Utc>>) -> Result<Self> {
        Ok(Self {
            scopes: Vec::new(),
            terms: Vec::new(),
            fence: Checkpoint {
                database_id: String::new(),
                epoch: 0,
                fence_token: String::new(),
            },
            used_tantivy: false,
        })
    }
    pub async fn search(&mut self, _: &ReaderHandle, _: i64) -> Result<Vec<Hit>> {
        Ok(Vec::new())
    }
    pub async fn load(&mut self, _: &[String]) -> Result<Vec<Memory>> {
        Ok(Vec::new())
    }
    pub async fn close(self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "lexical_read_tests.rs"]
mod tests;
