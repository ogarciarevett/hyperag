use hyperag::{core::{Config, Hyperag}, server::{App, TokenFile, router}, store::Store};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{str::FromStr, sync::Arc, time::{Duration, Instant}};

#[tokio::test]
async fn health_tracks_database_availability_with_a_bounded_wait() {
    let url = std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_|
        "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new().max_connections(1).connect_with(options.clone()).await.unwrap();
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name = format!("health_{}", hex::encode(nonce));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}"))).execute(&admin).await.unwrap();
    let pool = PgPoolOptions::new().max_connections(1).connect_with(options.database(&name)).await.unwrap();
    let hyperag = Arc::new(Hyperag::init(Config::new(Store::new(pool.clone()))).await.unwrap());
    let temporary = tempfile::tempdir().unwrap();
    let tokens = Arc::new(TokenFile::open(temporary.path().join("tokens.json")).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/healthz", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router(App { hyperag, tokens })).await.unwrap() });
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
    let response = client.get(&endpoint).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<serde_json::Value>().await.unwrap(), serde_json::json!({"ok": true}));

    // A live process with an exhausted pool must not report healthy or wait for the 30 s pool deadline.
    let held = pool.acquire().await.unwrap();
    let started = Instant::now();
    let response = client.get(&endpoint).send().await.unwrap();
    let saturated_status = response.status();
    let saturated_body = response.json::<serde_json::Value>().await.unwrap();
    let saturated_elapsed = started.elapsed();
    drop(held);

    // Disable only this fixture database and terminate its connections to reproduce a database outage.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("alter database {name} allow_connections false"))).execute(&admin).await.unwrap();
    sqlx::query("select pg_terminate_backend(pid) from pg_stat_activity where datname=$1")
        .bind(&name).execute(&admin).await.unwrap();
    let started = Instant::now();
    let outage_status = client.get(&endpoint).send().await.unwrap().status();
    let outage_elapsed = started.elapsed();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("alter database {name} allow_connections true"))).execute(&admin).await.unwrap();
    let recovered = client.get(&endpoint).send().await.unwrap().status();

    server.abort();
    let _ = server.await;
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop database {name} with (force)"))).execute(&admin).await.unwrap();
    admin.close().await;
    assert_eq!(saturated_status, 503, "pool exhaustion must fail readiness");
    assert_eq!(saturated_body, serde_json::json!({"ok": false}));
    assert!(saturated_elapsed < Duration::from_secs(2));
    assert_eq!(outage_status, 503, "database loss must fail readiness");
    assert!(outage_elapsed < Duration::from_secs(2));
    assert_eq!(recovered, 200, "readiness must recover without restarting the service");
}
