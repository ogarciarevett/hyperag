//! Private source reader; no index writer, scorer, pruning or service wiring.
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction, types::Json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Checkpoint {
    pub database_id: String,
    pub epoch: i64,
    pub fence_token: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct SourceDocument {
    pub id: String,
    pub scope: String,
    pub version: i32,
    pub recorded_at: DateTime<Utc>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub terms: Json<Vec<(String, i32)>>,
}

#[derive(Debug)]
pub(super) struct Batch {
    /// Delete these IDs, then add the active documents (absent rows are tombstones).
    pub ids: Vec<String>,
    pub documents: Vec<SourceDocument>,
}

pub(super) struct SourceSnapshot {
    tx: Transaction<'static, Postgres>,
    fence: Checkpoint,
    after_epoch: Option<i64>,
    cursor: Option<String>,
    drained: bool,
}

impl SourceSnapshot {
    /// `None` starts a rebuild. Invalid replay lineage requires fallback and a fresh rebuild.
    pub async fn open(pool: &PgPool, after: Option<&Checkpoint>) -> anyhow::Result<Self> {
        let mut tx = pool.begin().await?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *tx)
            .await?;
        let (database_id, epoch, fence_token, floor): (String, i64, String, i64) = sqlx::query_as(
            "select database_id::text,epoch,fence_token::text,retained_after from lexical_index_state"
        ).fetch_one(&mut *tx).await?;
        let fence = Checkpoint {
            database_id,
            epoch,
            fence_token,
        };
        if let Some(after) = after {
            anyhow::ensure!(
                after.database_id == fence.database_id
                    && after.epoch >= floor
                    && after.epoch <= fence.epoch,
                "lexical source identity/retention mismatch: rebuild required"
            );
            // Validate every epoch, including marker-only transactions, before reading any IDs.
            let intact: bool = sqlx::query_scalar(
                r#"
                with epochs as (
                  select epoch,min(previous_token::text) as previous_token,
                         min(fence_token::text) as fence_token,
                         count(distinct previous_token)=1 and count(distinct fence_token)=1
                           and not bool_or(kind='reset') as consistent
                    from lexical_index_changes where epoch>$1 and epoch<=$3 group by epoch
                ), chain as (
                  select *,lag(fence_token,1,$2) over(order by epoch) as predecessor from epochs
                )
                select count(*)=$3-$1
                   and coalesce(bool_and(consistent and previous_token=predecessor),true)
                   and coalesce((select fence_token from chain order by epoch desc limit 1),$2)=$4
                  from chain
            "#,
            )
            .bind(after.epoch)
            .bind(&after.fence_token)
            .bind(fence.epoch)
            .bind(&fence.fence_token)
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                intact,
                "lexical source gap/reset/lineage mismatch: rebuild required"
            );
        }
        Ok(Self {
            tx,
            fence,
            after_epoch: after.map(|c| c.epoch),
            cursor: None,
            drained: false,
        })
    }

    /// All pages share the fence's MVCC snapshot; no advisory writer lock is taken.
    pub async fn next_batch(&mut self) -> anyhow::Result<Option<Batch>> {
        if self.drained {
            return Ok(None);
        }
        let ids: Vec<String> = if let Some(epoch) = self.after_epoch {
            sqlx::query_scalar(
                r#"
                select distinct memory_id collate "C" as id from lexical_index_changes
                 where epoch>$1 and epoch<=$2 and kind='memory'
                   and ($3::text is null or memory_id collate "C">$3 collate "C")
                 order by id limit 64
            "#,
            )
            .bind(epoch)
            .bind(self.fence.epoch)
            .bind(&self.cursor)
            .fetch_all(&mut *self.tx)
            .await?
        } else {
            sqlx::query_scalar(
                r#"
                select id collate "C" from memories
                 where $1::text is null or id collate "C">$1 collate "C"
                 order by id collate "C" limit 64
            "#,
            )
            .bind(&self.cursor)
            .fetch_all(&mut *self.tx)
            .await?
        };
        if ids.is_empty() {
            self.drained = true;
            return Ok(None);
        }
        let documents = sqlx::query_as::<_, SourceDocument>(r#"
            select m.id,m.scope,v.version,v.recorded_at,v.valid_from,v.valid_to,
                   coalesce((select jsonb_agg(jsonb_build_array(u.lexeme,
                       coalesce(cardinality(u.positions),1)) order by u.lexeme collate "C")
                     from unnest(v.text_search) u),'[]'::jsonb) as terms
              from memories m join memory_versions v on v.memory_id=m.id and v.version=m.current_version
             where m.id=any($1::text[]) and m.deleted_at is null and v.status='active'
             order by m.id collate "C"
        "#).bind(&ids).fetch_all(&mut *self.tx).await?;
        self.cursor = ids.last().cloned();
        Ok(Some(Batch { ids, documents }))
    }

    /// This proves source exhaustion only; index commit/reload and query fencing remain mandatory.
    pub fn completed_checkpoint(&self) -> Option<&Checkpoint> {
        self.drained.then_some(&self.fence)
    }
}

#[cfg(test)]
#[path = "lexical_snapshot_tests.rs"]
mod tests;
