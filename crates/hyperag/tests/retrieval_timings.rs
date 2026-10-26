use hyperag::{Config, Hyperag, RetrieveOptions};
use hyperag::store::{CreateInput, Store, VersionInput};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
async fn real_retrieval_profiles_executed_stages_without_changing_results() {
    let url=std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_|"postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let opts=PgConnectOptions::from_str(&url).unwrap();
    let admin=PgPoolOptions::new().max_connections(1).connect_with(opts.clone()).await.unwrap();
    let name=format!("timing_{}",std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}"))).execute(&admin).await.unwrap();
    let pool=PgPoolOptions::new().max_connections(2).connect_with(opts.database(&name)).await.unwrap();
    let h=Hyperag::init(Config::new(Store::new(pool.clone()))).await.unwrap();
    h.store.grant("fixture","fixture","read","fixture").await.unwrap();
    h.store.create("fixture",&CreateInput{id:"fixture",scope:"fixture",kind:"fact",supersedes:&[],idempotency_key:&None,
        version:VersionInput{text:"synthetic quartz".into(),source:"fixture".into(),..Default::default()}},"fixture",None).await.unwrap();
    let options=|profile|serde_json::from_value::<RetrieveOptions>(serde_json::json!({"profile":profile,"limit":1})).unwrap();
    let plain=serde_json::to_value(h.retrieve("fixture","quartz",&options(false)).await.unwrap()).unwrap();
    let mut profiled=serde_json::to_value(h.retrieve("fixture","quartz",&options(true)).await.unwrap()).unwrap();
    let timing=profiled.as_object_mut().unwrap().remove("timingsMs");
    drop(h);pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop database {name} with (force)"))).execute(&admin).await.unwrap();admin.close().await;
    assert_eq!(plain,profiled);
    let timing=timing.expect("profile=true returns durations");
    let ms=|key:&str|timing[key].as_f64().unwrap();
    assert!(ms("lexical")>0.0 && ms("payloadLoad")>0.0 && ms("fusion")>0.0);
    for key in ["embedding","vector","graph","rerank"] {assert_eq!(ms(key),0.0);}
    let sum=["embedding","lexical","vector","graph","fusion","rerank","payloadLoad","other"].iter().map(|k|ms(k)).sum::<f64>();
    assert!((sum-ms("total")).abs()<0.000001);
}
