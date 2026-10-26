use super::*;
use crate::{
    core::{Channels, Config, Hyperag, RetrieveOptions},
    lexical_consumer::Consumer,
    store::Store,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
async fn authorized_snapshot_keeps_fences_payloads_and_whole_fallback() -> anyhow::Result<()> {
    // Run the opt-in environment in its own process, without racing other tests' defaults.
    if std::env::var("HYPERAG_LEXICAL_READ_CHILD").as_deref() != Ok("1") {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "lexical_read::tests::authorized_snapshot_keeps_fences_payloads_and_whole_fallback",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HYPERAG_LEXICAL_READ_CHILD", "1")
            .env("HYPERAG_LEXICAL_SCORER", "tantivy")
            .status()?;
        anyhow::ensure!(status.success(), "opt-in snapshot child failed");
        return Ok(());
    }
    let options =
        PgConnectOptions::from_str(&std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_| {
            "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into()
        }))?;
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    let name = format!("lexical_read_{}", hex::encode(nonce));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.database(&name))
        .await?;
    let result = async {
        let store = Store::new(pool.clone());
        store.migrate().await?;
        exercise(store).await
    }
    .await;
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database {name} with(force)"
    )))
    .execute(&admin)
    .await?;
    admin.close().await;
    result
}

async fn write(store: &Store, sql: &str) -> anyhow::Result<()> {
    let mut tx = store.begin_write().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn exercise(store: Store) -> anyhow::Result<()> {
    write(&store, "insert into memories(id,scope,kind,created_by,current_version) values
        ('a','allowed','fact','fixture',1),('b','allowed','fact','fixture',1),('hidden','denied','fact','fixture',1);
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        select id,1,'alpha','fixture','fixture','hash' from memories;
        update memory_versions set valid_to='2000-01-01T00:00:00Z' where memory_id='b'").await?;
    store.grant("reader", "allowed", "read", "fixture").await?;
    let scratch = tempfile::tempdir()?;
    let mut consumer = Consumer::open(&scratch.path().join("index"))?;
    let first = consumer.update(&store.pool).await?;
    let reader = consumer.reader();
    let mut read = Read::open(&store.pool, "reader", "ALPHA!", None).await?;
    anyhow::ensure!(
        read.scopes == ["allowed"] && read.terms == ["alpha"],
        "fresh authorization/normalization missing"
    );
    let hits = read.search(&reader, 24).await?;
    anyhow::ensure!(
        read.used_tantivy && hits.len() == 2 && hits.iter().all(|h| h.id != "hidden"),
        "exact-fenced authorized reader not used"
    );
    write(&store, "insert into memory_versions(memory_id,version,text,source,author,content_hash)
        values('a',2,'beta','fixture','fixture','new'); update memories set current_version=2 where id='a'").await?;
    let loaded = read.load(&["a".into()]).await?;
    anyhow::ensure!(
        loaded[0].version == 1 && loaded[0].text == "alpha",
        "payload escaped its authorized snapshot"
    );
    read.close().await?;
    let mut lag = Read::open(&store.pool, "reader", "beta", None).await?;
    let hits = lag.search(&reader, 24).await?;
    anyhow::ensure!(
        !lag.used_tantivy && hits.len() == 1 && hits[0].id == "a",
        "lag must use whole SQL fallback"
    );
    lag.close().await?;
    // Simulate a mismatched pin to exercise payload validation even if a fence were misbound.
    let mut mismatched = Read::open(&store.pool, "reader", "alpha", None).await?;
    mismatched.fence = first;
    let hits = mismatched.search(&reader, 24).await?;
    anyhow::ensure!(
        !mismatched.used_tantivy && hits.len() == 1 && hits[0].id == "b",
        "version mismatch must replace the whole lexical list"
    );
    mismatched.close().await?;
    consumer.update(&store.pool).await?;
    let mut fresh = Read::open(&store.pool, "reader", "beta", None).await?;
    anyhow::ensure!(fresh.search(&reader, 24).await?.len() == 1 && fresh.used_tantivy);
    fresh.close().await?;
    let at = crate::store::parse_time("2000-01-01T00:00:00Z").unwrap();
    let mut bounded = Read::open(&store.pool, "reader", "alpha", Some(at)).await?;
    anyhow::ensure!(
        bounded.search(&reader, 24).await?.is_empty(),
        "valid_to boundary included"
    );
    bounded.close().await?;
    let core = Hyperag::init(Config::new(
        store.clone().with_lexical_reader(reader.clone()),
    ))
    .await?;
    let options = RetrieveOptions {
        channels: Some(Channels {
            vector: Some(false),
            graph: Some(false),
            rerank: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    let retrieved = core.retrieve("reader", "beta", &options).await?;
    anyhow::ensure!(retrieved.items.len() == 1 && retrieved.items[0].memory.version == 2);
    store.revoke("reader", "allowed", "read", "fixture").await?;
    anyhow::ensure!(
        core.retrieve("reader", "beta", &options)
            .await?
            .items
            .is_empty(),
        "revoked grants reused"
    );
    store.grant("reader", "allowed", "read", "fixture").await?;
    write(&store, "delete from memories where id='a'; update memory_versions set status='invalidated' where memory_id='b';
        select lexical_index_prune((select epoch from lexical_index_state))").await?;
    let mut missing = Read::open(&store.pool, "reader", "alpha beta", None).await?;
    anyhow::ensure!(
        missing
            .search(&ReaderHandle::default(), 24)
            .await?
            .is_empty()
            && !missing.used_tantivy,
        "missing reader fallback leaked invalid rows"
    );
    missing.close().await?;
    anyhow::ensure!(
        core.retrieve("reader", "alpha beta", &options)
            .await?
            .items
            .is_empty(),
        "stale reader survived delete/invalidation/floor"
    );
    Ok(())
}
