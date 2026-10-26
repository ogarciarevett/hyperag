use super::*;
use crate::store::Store;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

async fn write(pool: &PgPool, sql: &str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("select pg_advisory_xact_lock(1213811052,1)")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn drain(snapshot: &mut SourceSnapshot) -> Vec<Batch> {
    let mut batches = Vec::new();
    while let Some(batch) = snapshot.next_batch().await.unwrap() {
        assert!(batch.ids.len() <= 64);
        assert!(
            snapshot.completed_checkpoint().is_none(),
            "pages are not completed epochs"
        );
        batches.push(batch);
    }
    assert!(snapshot.completed_checkpoint().is_some());
    batches
}

#[tokio::test]
async fn snapshot_replay_is_complete_consistent_and_requires_intact_lineage() {
    let url = std::env::var("HYPERAG_PG_URL")
        .unwrap_or_else(|_| "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let name = format!(
        "lexical_snapshot_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(&name))
        .await
        .unwrap();
    Store::new(pool.clone()).migrate().await.unwrap();
    let result = exercise(&pool).await;
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database {name} with(force)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn exercise(pool: &PgPool) -> anyhow::Result<()> {
    let mut empty = SourceSnapshot::open(pool, None).await?;
    assert!(empty.completed_checkpoint().is_none());
    assert!(drain(&mut empty).await.is_empty());
    let zero = empty.completed_checkpoint().unwrap().clone();
    drop(empty);
    write(pool, "insert into memories(id,scope,kind,created_by,current_version)
        select 'id-'||lpad(n::text,3,'0'),'allowed','fact','fixture',1 from generate_series(1,130) n;
        insert into memory_versions(memory_id,version,text,source,author,content_hash,valid_from,valid_to)
        select id,1,case when id='id-129' then '!!!' else 'ALPHA alpha Rare!' end,
        'fixture','fixture','hash','2020-01-01','2030-01-01' from memories").await;
    let mut rebuilding = SourceSnapshot::open(pool, None).await?;
    assert_eq!(rebuilding.next_batch().await?.unwrap().documents.len(), 64);
    let mut replay = SourceSnapshot::open(pool, Some(&zero)).await?;
    let first = replay.next_batch().await?.unwrap();
    assert_eq!(first.ids.len(), 64);
    assert_eq!(first.documents.len(), 64);
    assert!(replay.completed_checkpoint().is_none());
    let doc = &first.documents[0];
    assert_eq!(
        (&doc.id, doc.scope.as_str(), doc.version),
        (&first.ids[0], "allowed", 1)
    );
    assert_eq!(doc.terms.0, vec![("alpha".into(), 2), ("rare".into(), 1)]);
    assert!(doc.valid_from < doc.valid_to && doc.recorded_at.timestamp() > 0);
    // Writers remain available while later pages continue to read the original fence.
    tokio::time::timeout(std::time::Duration::from_secs(5), write(pool,
        "update memory_versions set text='newlexeme' where memory_id='id-070';
         delete from memories where id='id-090';
         update memory_versions set status='invalidated' where memory_id='id-100';
         update memories set scope='denied' where id='id-110';
         insert into grants(principal,scope,permission,granted_by) values('reader','allowed','read','fixture')")).await?;
    let rest = drain(&mut replay).await;
    assert_eq!(
        rest.iter().map(|b| b.ids.len()).collect::<Vec<_>>(),
        vec![64, 2]
    );
    assert_eq!(rest[0].documents.len(), 64);
    assert!(rest[1].documents[0].terms.0.is_empty());
    assert!(
        rest[0]
            .documents
            .iter()
            .all(|d| d.terms.0[0].0 == "alpha" && d.scope == "allowed")
    );
    let one = replay.completed_checkpoint().unwrap().clone();
    assert_eq!(one.epoch, zero.epoch + 1);
    assert_eq!(
        drain(&mut rebuilding)
            .await
            .iter()
            .map(|b| b.documents.len())
            .sum::<usize>(),
        66
    );
    assert_eq!(rebuilding.completed_checkpoint(), Some(&one));
    drop(rebuilding);
    drop(replay);
    // Multiple transactions affecting an ID replay its final row once; deleted/invalid rows tombstone.
    write(
        pool,
        "insert into memory_versions(memory_id,version,text,source,author,content_hash)
         values('id-070',2,'final final','fixture','fixture','new-hash');
         update memories set current_version=2 where id='id-070'",
    )
    .await;
    let mut delta = SourceSnapshot::open(pool, Some(&one)).await?;
    let batches = drain(&mut delta).await;
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].ids, ["id-070", "id-090", "id-100", "id-110"]);
    assert_eq!(batches[0].documents.len(), 2);
    assert_eq!(batches[0].documents[0].terms.0, [("final".into(), 2)]);
    assert_eq!(batches[0].documents[0].version, 2);
    assert!(batches[0].documents[0].valid_from.is_none());
    assert_eq!(batches[0].documents[1].scope, "denied");
    let current = delta.completed_checkpoint().unwrap().clone();
    drop(delta);
    for broken in [
        Checkpoint {
            database_id: "other".into(),
            ..current.clone()
        },
        Checkpoint {
            fence_token: "other".into(),
            ..current.clone()
        },
        Checkpoint {
            epoch: current.epoch + 1,
            ..current.clone()
        },
        Checkpoint {
            epoch: -1,
            ..current.clone()
        },
        Checkpoint {
            fence_token: "other".into(),
            ..one.clone()
        },
    ] {
        assert!(SourceSnapshot::open(pool, Some(&broken)).await.is_err());
    }
    let mut unchanged = SourceSnapshot::open(pool, Some(&current)).await?;
    assert!(drain(&mut unchanged).await.is_empty());
    drop(unchanged);
    write(
        pool,
        "update lexical_index_changes set fence_token=gen_random_uuid()
        where epoch=2 and memory_id='id-070'",
    )
    .await;
    assert!(SourceSnapshot::open(pool, Some(&one)).await.is_err());
    write(
        pool,
        "update lexical_index_changes set fence_token=(
        select previous_token from lexical_index_changes where epoch=3 limit 1) where epoch=2",
    )
    .await;
    write(pool, "delete from grants").await;
    let mut authorization = SourceSnapshot::open(pool, Some(&current)).await?;
    assert!(drain(&mut authorization).await.is_empty());
    assert_eq!(
        authorization.completed_checkpoint().unwrap().epoch,
        current.epoch + 1
    );
    drop(authorization);
    write(pool, "select lexical_index_prune(1)").await;
    assert!(SourceSnapshot::open(pool, Some(&zero)).await.is_err());
    drop(SourceSnapshot::open(pool, Some(&one)).await?); // The floor itself is still replayable.
    // Losing an unpruned epoch also requires rebuild, independently of the retention floor.
    write(pool, "delete from lexical_index_changes where epoch=2").await;
    assert!(SourceSnapshot::open(pool, Some(&one)).await.is_err());
    write(pool, "truncate grants").await;
    assert!(SourceSnapshot::open(pool, Some(&current)).await.is_err());
    let mut rebuild = SourceSnapshot::open(pool, None).await?;
    let batches = drain(&mut rebuild).await;
    assert_eq!(
        batches.iter().map(|b| b.documents.len()).sum::<usize>(),
        128
    );
    let checkpoint = rebuild.completed_checkpoint().unwrap().clone();
    drop(rebuild);
    write(pool, "insert into memories(id,scope,kind,created_by,current_version)
        select 'bulk-'||lpad(n::text,6,'0'),'fixture','fact','fixture',1 from generate_series(1,20000) n;
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        select id,1,'synthetic lexical fixture','fixture','fixture','hash' from memories where id like 'bulk-%';
        analyze memories; analyze memory_versions; analyze lexical_index_changes").await;
    for after in [Some(&checkpoint), None] {
        let started = std::time::Instant::now();
        let mut snapshot = SourceSnapshot::open(pool, after).await?;
        let mut ids = 0;
        while let Some(batch) = snapshot.next_batch().await? {
            assert!(batch.ids.len() <= 64 && snapshot.completed_checkpoint().is_none());
            ids += batch.ids.len();
        }
        assert_eq!(ids, if after.is_some() { 20000 } else { 20129 });
        // Same-backend transaction counters include index-only scans and final-row fetches.
        let visits: Vec<(String, i64)> = sqlx::query_as(
            "select t.relname,t.seq_tup_read+coalesce((select sum(pg_stat_get_xact_tuples_returned(i.indexrelid))
               from pg_index i where i.indrelid=t.relid),0)::bigint
             from pg_stat_xact_user_tables t
             where t.relname in ('memories','memory_versions','lexical_index_changes') order by t.relname"
        ).fetch_all(&mut *snapshot.tx).await?;
        let total: i64 = visits.iter().map(|(_, count)| count).sum();
        println!(
            "source-paging-cost mode={} ids={ids} visits={visits:?} ms={}",
            if after.is_some() { "replay" } else { "rebuild" },
            started.elapsed().as_millis()
        );
        anyhow::ensure!(
            total > 0 && total <= ids as i64 * 8,
            "source work must stay within 8 visits per ID: {total}"
        );
    }
    Ok(())
}
