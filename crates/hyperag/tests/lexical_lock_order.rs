//! Real Store calls, coordinated after update's row lock and before its first version DML.
use hyperag::store::{CreateInput, Store, VersionInput};
use sqlx::{postgres::{PgConnectOptions, PgPoolOptions}, Executor, PgPool};
use std::{str::FromStr, time::Duration};

fn input(text: &str) -> VersionInput {
    VersionInput { text: text.into(), source: "fixture".into(), valid_from: None, valid_to: None, metadata: None }
}
async fn blocked(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("select cardinality(pg_blocking_pids($1))>0")
                .bind(pid).fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("writer reached coordinated wait");
}
async fn schedule(correction: bool) {
    let url = std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_| "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new().max_connections(1).connect_with(options.clone()).await.unwrap();
    let name = format!("lexical_lock_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    admin.execute(sqlx::AssertSqlSafe(format!("create database {name}"))).await.unwrap();
    let pool = PgPoolOptions::new().max_connections(6).connect_with(options.database(&name)).await.unwrap();
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    let none = None;
    store.create("fixture", &CreateInput { id: "a", scope: "fixture", kind: "fact", supersedes: &[], idempotency_key: &none, version: input("old quartz") }, "old", None).await.unwrap();
    // Alphabetically before lexical_version_guard: pause AFTER update's explicit FOR UPDATE.
    pool.execute("create function fixture_barrier() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(9876,1); return null; end $$; create trigger a_fixture_barrier before update on memory_versions for each statement execute function fixture_barrier()").await.unwrap();
    let mut barrier = pool.acquire().await.unwrap();
    sqlx::query("select pg_advisory_lock(9876,1)").execute(&mut *barrier).await.unwrap();
    // Dedicated one-connection pools make the observed backend identities deterministic.
    let opts = pool.connect_options();
    let update_pool = PgPoolOptions::new().max_connections(1).connect_with((*opts).clone()).await.unwrap();
    let update_pid: i32 = sqlx::query_scalar("select pg_backend_pid()").fetch_one(&update_pool).await.unwrap();
    let other_pool = PgPoolOptions::new().max_connections(1).connect_with((*opts).clone()).await.unwrap();
    let other_pid: i32 = sqlx::query_scalar("select pg_backend_pid()").fetch_one(&other_pool).await.unwrap();
    let a = tokio::spawn(async move { Store::new(update_pool.clone()).update("fixture", "a", &input("new quartz"), 1, &None, "new", None).await.map(|_| ()) });
    blocked(&pool, update_pid).await;
    let b = tokio::spawn(async move {
        let other = Store::new(other_pool.clone());
        if correction {
            other.create("fixture", &CreateInput { id: "correction", scope: "fixture", kind: "fact", supersedes: &["a".into()], idempotency_key: &None, version: input("correct quartz") }, "correct", None).await.map(|_| ())
        } else { other.delete("a", "fixture", "fixture").await }
    });
    blocked(&pool, other_pid).await;
    sqlx::query("select pg_advisory_unlock(9876,1)").execute(&mut *barrier).await.unwrap();
    drop(barrier);
    let results = tokio::time::timeout(Duration::from_secs(10), async { (a.await.unwrap(), b.await.unwrap()) }).await.unwrap();
    pool.close().await;
    admin.execute(sqlx::AssertSqlSafe(format!("drop database {name} with (force)"))).await.unwrap();
    admin.close().await;
    assert!(results.0.is_ok(), "update: {:?}", results.0);
    assert!(results.1.is_ok(), "competing write: {:?}", results.1);
}
#[tokio::test]
async fn update_versus_delete_has_no_lock_inversion() { schedule(false).await; }
#[tokio::test]
async fn update_versus_correction_has_no_lock_inversion() { schedule(true).await; }
