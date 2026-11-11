//! Real Rust Store mutations on a fresh synthetic database; no models or live memories.
use hyperag::store::{CreateInput, Store, VersionInput};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::str::FromStr;

async fn epoch(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select epoch from lexical_index_state")
        .fetch_one(pool)
        .await
        .unwrap()
}
fn version(text: &str) -> VersionInput {
    VersionInput {
        text: text.into(),
        source: "fixture".into(),
        valid_from: None,
        valid_to: None,
        metadata: None,
    }
}
async fn create(store: &Store, id: &str, supersedes: &[String]) {
    store
        .create(
            "fixture",
            &CreateInput {
                id,
                scope: "allowed",
                kind: "fact",
                supersedes,
                idempotency_key: &None,
                version: version("synthetic-private-text-marker"),
            },
            id,
            None,
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn rust_migration_and_every_source_write_publish_committed_fences() {
    let url = std::env::var("HYPERAG_PG_URL")
        .unwrap_or_else(|_| "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let name = format!(
        "lexical_outbox_{}",
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
        .max_connections(2)
        .connect_with(options.database(&name))
        .await
        .unwrap();
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    // Check the actual migrator before doing any writes, then exercise every Rust source write path.
    let exists: bool = sqlx::query_scalar("select to_regclass('lexical_index_state') is not null")
        .fetch_one(&pool)
        .await
        .unwrap();
    if !exists {
        pool.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database {name} with(force)"
        )))
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;
        panic!("Rust migrator did not install the lexical source fence");
    }
    let mut previous = epoch(&pool).await;
    assert_eq!(previous, 0);
    create(&store, "a", &[]).await;
    let next = epoch(&pool).await;
    assert!(next > previous);
    previous = next;
    store
        .update(
            "fixture",
            "a",
            &version("updated-marker"),
            1,
            &None,
            "new-hash",
            None,
        )
        .await
        .unwrap();
    let next = epoch(&pool).await;
    assert!(next > previous);
    previous = next;
    assert!(
        store
            .update(
                "fixture",
                "a",
                &version("rejected-marker"),
                1,
                &None,
                "conflicting-hash",
                None
            )
            .await
            .is_err()
    );
    assert_eq!(epoch(&pool).await, previous);
    create(&store, "b", &["a".into()]).await;
    let next = epoch(&pool).await;
    assert!(next > previous);
    previous = next;
    store.invalidate("b", "fixture", "fixture").await.unwrap();
    let next = epoch(&pool).await;
    assert!(next > previous);
    previous = next;
    store.delete("b", "fixture", "fixture").await.unwrap();
    let next = epoch(&pool).await;
    assert!(next > previous);
    previous = next;
    store
        .grant("reader", "allowed", "read", "fixture")
        .await
        .unwrap();
    let next = epoch(&pool).await;
    assert_eq!(next, previous + 1);
    previous = next;
    store
        .revoke("reader", "allowed", "read", "fixture")
        .await
        .unwrap();
    assert_eq!(epoch(&pool).await, previous + 1);
    let identity: String = sqlx::query_scalar("select database_id::text from lexical_index_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    store.migrate().await.unwrap();
    assert_eq!(epoch(&pool).await, previous + 1);
    let after: String = sqlx::query_scalar("select database_id::text from lexical_index_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(identity, after);
    let maximum: i64 = sqlx::query_scalar("select max(epoch) from lexical_index_changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(maximum, previous + 1);
    let serialized: String =
        sqlx::query_scalar("select json_agg(e)::text from lexical_index_changes e")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!serialized.contains("private-text-marker") && !serialized.contains("updated-marker"));
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database {name} with(force)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
}
