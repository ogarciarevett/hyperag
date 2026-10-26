use super::*;
use crate::{
    core::{Channels, Config, Hyperag, RetrieveOptions},
    lexical_consumer::Consumer,
    store::Store,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn default_bm25_sql_keeps_original_query_bytes() {
    use sha2::{Digest, Sha256};
    // Freeze the default query from c4384c8, including its unordered floating-point SUM.
    assert_eq!(
        hex::encode(Sha256::digest(crate::store::bm25_sql().as_bytes())),
        "d4d3c3c98fd9886fe0667ae66c2225cf52cdbb2757ab98e5019b4ec2f1c5aba8"
    );
}

#[tokio::test]
async fn authorized_snapshot_keeps_fences_payloads_and_whole_fallback() -> anyhow::Result<()> {
    isolated(
        "lexical_read::tests::authorized_snapshot_keeps_fences_payloads_and_whole_fallback",
        exercise,
    )
    .await
}

#[tokio::test]
async fn current_reader_equals_sql_ids_order_and_reports_score_gap() -> anyhow::Result<()> {
    isolated(
        "lexical_read::tests::current_reader_equals_sql_ids_order_and_reports_score_gap",
        sql_parity,
    )
    .await
}

#[tokio::test]
async fn concurrent_opt_in_vector_requests_fit_the_service_pool() -> anyhow::Result<()> {
    isolated(
        "lexical_read::tests::concurrent_opt_in_vector_requests_fit_the_service_pool",
        concurrent_vectors,
    )
    .await
}

pub(crate) async fn isolated(
    test: &str,
    exercise: impl AsyncFn(Store) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Run the opt-in environment in its own process, without racing other tests' defaults.
    if std::env::var("HYPERAG_LEXICAL_READ_CHILD").as_deref() != Ok("1") {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
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
        .max_connections(8)
        .acquire_timeout(std::time::Duration::from_secs(3))
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

async fn sql_parity(store: Store) -> anyhow::Result<()> {
    write(
        &store,
        "insert into memories(id,scope,kind,created_by,current_version) values
        ('A','allowed','fact','fixture',1),('B','allowed','fact','fixture',1),
        ('tie-a','allowed','fact','fixture',1),('tie-A','allowed','fact','fixture',1),
        ('tie_é','allowed','fact','fixture',1),('newer','allowed','fact','fixture',1),
        ('tie_e','allowed','fact','fixture',1),('starts','allowed','fact','fixture',1),
        ('tf-17','allowed','fact','fixture',1),('tf-18','allowed','fact','fixture',1),
        ('empty','allowed','fact','fixture',1),('hidden','denied','fact','fixture',1);
        insert into memory_versions(memory_id,version,text,source,author,content_hash,recorded_at)
        select id,1,case id when 'A' then 'alpha beta beta beta'
            when 'B' then 'alpha beta gamma' when 'empty' then '!!!'
            when 'tf-17' then 'alpha ' || repeat('beta ',17)
            when 'tf-18' then 'alpha ' || repeat('beta ',18)
            when 'hidden' then 'alpha alpha alpha' else 'alpha beta beta beta' end,
            'fixture','fixture','hash',case when id='newer' then '2025-01-02'::timestamptz
            else '2025-01-01'::timestamptz end from memories;
        update memory_versions set valid_to='2025-01-02' where memory_id='B';
        update memory_versions set valid_from='2025-01-02',valid_to='2025-01-03'
            where memory_id='starts'",
    )
    .await?;
    store.grant("reader", "allowed", "read", "fixture").await?;
    let scratch = tempfile::tempdir()?;
    let mut consumer = Consumer::open(&scratch.path().join("index"))?;
    consumer.update(&store.pool).await?;
    let (mut max_absolute, mut max_relative, mut max_ulp) = (0.0_f64, 0.0_f64, 0_u64);
    let mut comparisons = 0;
    for at in [
        None,
        crate::store::parse_time("2025-01-01T00:00:00Z"),
        crate::store::parse_time("2025-01-02T00:00:00Z"),
        crate::store::parse_time("2025-01-03T00:00:00Z"),
    ] {
        for query in [
            "alpha",
            "beta",
            "gamma beta ALPHA! alpha",
            "alpha beta gamma",
            "beta alpha",
            "missing",
            "!!!",
        ] {
            for limit in [1, 3, 24] {
                let mut read = Read::open(&store.pool, "reader", query, at).await?;
                let actual = read.search(&consumer.reader(), limit).await?;
                anyhow::ensure!(read.used_tantivy, "current reader fell back: {query}");
                let expected = store.bm25(&["allowed".into()], query, limit, at).await?;
                let signature = |hits: &[Hit]| {
                    hits.iter()
                        .map(|h| (h.id.clone(), h.at.to_bits()))
                        .collect::<Vec<_>>()
                };
                anyhow::ensure!(
                    signature(&actual) == signature(&expected),
                    "current reader differs: query={query},limit={limit},at={at:?}\nactual={actual:?}\nSQL={expected:?}"
                );
                for (actual, expected) in actual.iter().zip(&expected) {
                    let absolute = (actual.score - expected.score).abs();
                    let relative = absolute / expected.score.abs().max(f64::MIN_POSITIVE);
                    anyhow::ensure!(
                        actual.score.is_finite() && expected.score > 0.0 && relative <= 1e-12,
                        "BM25 score differs for {}: native={},SQL={}",
                        actual.id,
                        actual.score,
                        expected.score
                    );
                    max_absolute = max_absolute.max(absolute);
                    max_relative = max_relative.max(relative);
                    max_ulp =
                        max_ulp.max(actual.score.to_bits().abs_diff(expected.score.to_bits()));
                }
                comparisons += 1;
                read.close().await?;
            }
        }
    }
    println!(
        "native-sql-parity comparisons={comparisons} max_absolute={max_absolute:e} max_relative={max_relative:e} max_ulp={max_ulp}"
    );
    // Three unequal contributions can change an unordered SQL sum at a tied rank boundary.
    write(
        &store,
        "insert into memories(id,scope,kind,created_by,current_version) values
        ('round-a','allowed','fact','fixture',1),('round-b','allowed','fact','fixture',1);
        insert into memory_versions(memory_id,version,text,source,author,content_hash,recorded_at)
        select id,1,'alpha beta beta gamma gamma gamma','fixture','fixture','hash','2025-01-01'
            from memories where id in ('round-a','round-b')",
    )
    .await?;
    consumer.update(&store.pool).await?;
    for limit in [1, 24] {
        let query = "gamma beta alpha alpha";
        let mut read = Read::open(&store.pool, "reader", query, None).await?;
        let actual = read.search(&consumer.reader(), limit).await?;
        let expected = store.bm25(&["allowed".into()], query, limit, None).await?;
        let signature = |hits: &[Hit]| {
            hits.iter()
                .map(|h| (h.id.clone(), h.score.to_bits(), h.at.to_bits()))
                .collect::<Vec<_>>()
        };
        anyhow::ensure!(
            !read.used_tantivy && signature(&actual) == signature(&expected),
            "rounding-ambiguous ranks must use whole SQL fallback: limit={limit}"
        );
        read.close().await?;
    }
    Ok(())
}

async fn concurrent_vectors(store: Store) -> anyhow::Result<()> {
    use axum::{Json, Router, routing::post};
    use std::sync::Arc;
    use tokio::{sync::Barrier, task::JoinSet};
    let barrier = Arc::new(Barrier::new(8));
    let app = Router::new().route(
        "/v1/embeddings",
        post(move || {
            let barrier = barrier.clone();
            async move {
                // All eight request snapshots own a connection before any embedding completes.
                barrier.wait().await;
                Json(serde_json::json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let spec = format!(
        "http:http://{}?model=fixture&dims=3",
        listener.local_addr()?
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result = async {
        let embedder = crate::models::Embedder::from_spec(&spec).await?.unwrap();
        let space = store.ensure_space(embedder.describe()).await?;
        write(
            &store,
            "insert into memories(id,scope,kind,created_by,current_version)
            values('vector','allowed','fact','fixture',1);
            insert into memory_versions(memory_id,version,text,source,author,content_hash)
            values('vector',1,'alpha','fixture','fixture','hash')",
        )
        .await?;
        store.grant("reader", "allowed", "read", "fixture").await?;
        store
            .put_vectors(&space, &[("vector".into(), 1, vec![1.0, 0.0, 0.0])])
            .await?;
        let scratch = tempfile::tempdir()?;
        let mut consumer = Consumer::open(&scratch.path().join("index"))?;
        consumer.update(&store.pool).await?;
        for reader in [ReaderHandle::default(), consumer.reader()] {
            let mut attached = store.clone();
            attached.lexical_reader = reader;
            let mut config = Config::new(attached);
            config.embedder = crate::models::Embedder::from_spec(&spec).await?;
            let core = Arc::new(Hyperag::init(config).await?);
            let mut tasks = JoinSet::new();
            for _ in 0..8 {
                let core = core.clone();
                tasks.spawn(async move {
                    core.retrieve(
                        "reader",
                        "alpha",
                        &RetrieveOptions {
                            channels: Some(Channels {
                                graph: Some(false),
                                rerank: Some(false),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                });
            }
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while let Some(result) = tasks.join_next().await {
                    let retrieval = result??;
                    anyhow::ensure!(
                        retrieval.paths_used.iter().any(|p| p == "vector")
                            && retrieval.items.len() == 1
                            && retrieval.items[0].channels.contains_key("vector"),
                        "vector channel lost at pool capacity: {:?}",
                        retrieval.limitations
                    );
                }
                Ok::<_, anyhow::Error>(())
            })
            .await??;
        }
        let mut read = Read::open(&store.pool, "reader", "alpha", None).await?;
        anyhow::ensure!(
            read.vector(&space, &[1.0, 0.0], 24).await.is_err(),
            "dimension mismatch must fail vector SQL"
        );
        anyhow::ensure!(
            read.search(&consumer.reader(), 24).await?.len() == 1
                && read.used_tantivy
                && read.load(&["vector".into()]).await?.len() == 1,
            "failed vector SQL poisoned the request snapshot"
        );
        read.close().await?;
        Ok(())
    }
    .await;
    server.abort();
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
    let sql = store.bm25(&["allowed".into()], "alpha", 24, None).await?;
    anyhow::ensure!(
        hits[0].score.to_bits() == sql[0].score.to_bits(),
        "fallback retained a native score"
    );
    mismatched.close().await?;
    let indexed = consumer.update(&store.pool).await?;
    let mut fresh = Read::open(&store.pool, "reader", "beta", None).await?;
    anyhow::ensure!(fresh.search(&reader, 24).await?.len() == 1 && fresh.used_tantivy);
    fresh.close().await?;
    write(&store, "update memories set scope='denied' where id='a'").await?;
    let mut moved = Read::open(&store.pool, "reader", "beta", None).await?;
    moved.fence = indexed;
    anyhow::ensure!(
        moved.search(&reader, 24).await?.is_empty() && !moved.used_tantivy,
        "payload scope mismatch did not replace the whole lexical list"
    );
    moved.close().await?;
    write(&store, "update memories set scope='allowed' where id='a'").await?;
    let before_reset = consumer.update(&store.pool).await?;
    write(
        &store,
        "update lexical_index_state set fence_token=gen_random_uuid()",
    )
    .await?;
    let mut reset = Read::open(&store.pool, "reader", "beta", None).await?;
    anyhow::ensure!(
        reset.fence.epoch == before_reset.epoch
            && reset.fence.fence_token != before_reset.fence_token
    );
    anyhow::ensure!(
        reset.search(&reader, 24).await?.len() == 1 && !reset.used_tantivy,
        "same-epoch divergent token did not use SQL fallback"
    );
    reset.close().await?;
    let at = crate::store::parse_time("2000-01-01T00:00:00Z").unwrap();
    let mut bounded = Read::open(&store.pool, "reader", "alpha", Some(at)).await?;
    anyhow::ensure!(
        bounded.search(&reader, 24).await?.is_empty(),
        "valid_to boundary included"
    );
    bounded.close().await?;
    let mut attached = store.clone();
    attached.lexical_reader = reader.clone();
    let core = Hyperag::init(Config::new(attached)).await?;
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
    write(&store, "delete from memories where id='a'; update memory_versions set status='invalidated' where memory_id='b'").await?;
    write(
        &store,
        "select lexical_index_prune((select epoch from lexical_index_state))",
    )
    .await?;
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
