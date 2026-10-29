//! The document frequency lookup must read one contiguous index range per query term, and the
//! restructured indexed BM25 query must rank exactly like the legacy corpus scorer.
use hyperag::store::{CreateInput, Store, VersionInput};
use sqlx::{postgres::{PgConnectOptions, PgPoolOptions}, PgPool};
use std::str::FromStr;

const DF_LOOKUP: &str = "explain select s.lexeme, sum(s.df) from lexical_term_stats s
  where s.lexeme in ('alpha','beta','gamma') and s.scope = any($1::text[]) group by s.lexeme";

async fn fresh() -> (PgPool, PgPool, String) {
    let url = std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_| "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new().max_connections(1).connect_with(options.clone()).await.unwrap();
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let name = format!("lexical_stats_{}_{}_{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}"))).execute(&admin).await.unwrap();
    let pool = PgPoolOptions::new().max_connections(2).connect_with(options.database(&name)).await.unwrap();
    (admin, pool, name)
}
async fn drop_db(admin: PgPool, pool: PgPool, name: String) {
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop database {name} with(force)"))).execute(&admin).await.unwrap();
    admin.close().await;
}
fn version(text: &str) -> VersionInput {
    VersionInput { text: text.into(), source: "fixture".into(), valid_from: None, valid_to: None, metadata: None }
}

#[tokio::test]
async fn migration_adds_the_lexeme_first_index_and_planner_uses_it() {
    let (admin, pool, name) = fresh().await;
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    store.migrate().await.unwrap(); // idempotent
    let def: String = sqlx::query_scalar("select indexdef from pg_indexes where indexname = 'lexical_term_stats_lexeme'")
        .fetch_one(&pool).await.unwrap();
    assert!(def.contains("(lexeme, scope)") && def.contains("INCLUDE (df, valid_from, valid_to)"), "{def}");
    // Synthetic statistics shaped like the live cache: many scopes, many lexemes, few query terms.
    sqlx::raw_sql(
        "insert into lexical_term_stats(scope, lexeme, valid_from, valid_to, df)
         select 's' || s, case when l < 3 then (array['alpha','beta','gamma'])[l + 1] else 'w' || l end,
                '-infinity', 'infinity', 1 + (s + l) % 7
           from generate_series(1, 300) s, generate_series(0, 1500) l;",
    ).execute(&pool).await.unwrap();
    // A vacuumed table lets the planner price an index-only scan, as on the live cache.
    sqlx::raw_sql("vacuum analyze lexical_term_stats").execute(&pool).await.unwrap();
    let scopes: Vec<String> = (1..=300).map(|s| format!("s{s}")).collect();
    let plan: Vec<String> = sqlx::query_scalar(DF_LOOKUP).bind(&scopes).fetch_all(&pool).await.unwrap();
    let plan = plan.join("\n");
    assert!(plan.contains("lexical_term_stats_lexeme"), "df lookup must use the lexeme-first index:\n{plan}");
    drop_db(admin, pool, name).await;
}

#[tokio::test]
async fn indexed_bm25_ranks_exactly_like_the_legacy_scorer() {
    let (admin, pool, name) = fresh().await;
    let store = Store::new(pool.clone());
    store.migrate().await.unwrap();
    let texts = [
        "the coordinator decision about the merge", "deploy of the engine to testnet", "the the the filler words of the about",
        "merge deploy merge deploy engine", "unrelated note on gardening", "coordinator decision decision decision",
        "engine about merge", "a long document mentioning the engine the coordinator and the decision and a deploy of the merge about it",
    ];
    for (i, t) in texts.iter().enumerate() {
        let id = format!("m{i}");
        let scope = if i % 2 == 0 { "one" } else { "two" };
        store.create("fixture", &CreateInput { id: &id, scope, kind: "fact", supersedes: &[], idempotency_key: &None, version: version(t) }, &id, None)
            .await.unwrap();
    }
    let scopes = vec!["one".to_string(), "two".to_string()];
    let ready: bool = sqlx::query_scalar("select (select value from settings where key='lexical_cache_ready') = 'true' and (select value from settings where key='lexical_postings_ready') = 'true'")
        .fetch_one(&pool).await.unwrap();
    assert!(ready, "the indexed scorer must be ready so this compares the indexed path");
    let query = "coordinator decision about the merge deploy of the engine";
    // SAFETY: this file holds a single test that touches the variable at a time.
    unsafe { std::env::set_var("HYPERAG_LEXICAL_SCORER", "bm25") };
    let legacy = store.bm25(&scopes, query, 24, None).await.unwrap();
    unsafe { std::env::set_var("HYPERAG_LEXICAL_SCORER", "indexed-bm25") };
    let indexed = store.bm25(&scopes, query, 24, None).await.unwrap();
    assert_eq!(legacy.len(), indexed.len());
    assert!(indexed.len() >= 6);
    for (a, b) in legacy.iter().zip(&indexed) {
        assert_eq!(a.id, b.id);
        assert!((a.score - b.score).abs() < 1e-9, "{} {} vs {}", a.id, a.score, b.score);
    }
    drop_db(admin, pool, name).await;
}
