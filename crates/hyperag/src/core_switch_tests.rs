use super::*;
use axum::{Json, Router, routing::post};
use tokio::{
    sync::{Barrier, Notify, Semaphore},
    task::JoinSet,
};

#[tokio::test]
async fn opt_in_requests_wait_before_taking_the_switch_pool() -> anyhow::Result<()> {
    crate::lexical_read::tests::isolated(
        "core::switch_tests::opt_in_requests_wait_before_taking_the_switch_pool",
        overlap_switch,
    )
    .await
}

async fn overlap_switch(store: Store) -> anyhow::Result<()> {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Semaphore::new(0));
    let app = Router::new().route(
        "/v1/embeddings",
        post({
            let (entered, release) = (entered.clone(), release.clone());
            move |Json(body): Json<serde_json::Value>| {
                let (entered, release) = (entered.clone(), release.clone());
                async move {
                    assert_eq!(body["input"], serde_json::json!(["late alpha"]));
                    entered.notify_one();
                    release.acquire().await.unwrap().forget();
                    Json(serde_json::json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let spec = format!(
        "http:http://{}?model=switch-fixture&dims=3",
        listener.local_addr()?
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut config = Config::new(store.clone());
        config.admins = vec!["admin".into()];
        let core = Arc::new(Hyperag::init(config).await?);
        let embedding = Embedder::from_spec(&spec).await?.unwrap();
        let space = store.ensure_space(embedding.describe()).await?;
        insert(&store, "first", "alpha").await?;
        store.grant("reader", "allowed", "read", "fixture").await?;
        store
            .put_vectors(&space, &[("first".into(), 1, vec![1.0, 0.0, 0.0])])
            .await?;

        let guard = core.models.read().await;
        let mut switching = JoinSet::new();
        switching.spawn({
            let core = core.clone();
            async move { core.set_models("admin", Some(embedding), None).await }
        });
        // The sole writer queues only after its unlocked pass finds no missing vectors.
        while core.models.try_read().is_ok() {
            tokio::task::yield_now().await;
        }
        insert(&store, "late", "late alpha").await?;
        drop(guard);
        entered.notified().await;
        anyhow::ensure!(
            core.models.try_read().is_err(),
            "final pass did not hold the model write lock"
        );

        let mut connections = Vec::new();
        for _ in 0..8 {
            connections.push(store.pool.acquire().await?);
        }
        drop(connections);
        while store.pool.num_idle() != 8 {
            tokio::task::yield_now().await;
        }
        let polled = Arc::new(Barrier::new(9));
        let mut requests = JoinSet::new();
        for _ in 0..8 {
            let (core, polled) = (core.clone(), polled.clone());
            requests.spawn(async move {
                let options = RetrieveOptions {
                    channels: Some(Channels {
                        vector: Some(false),
                        graph: Some(false),
                        rerank: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let mut request = Box::pin(core.retrieve("reader", "alpha", &options));
                std::future::poll_fn(|cx| {
                    assert!(std::future::Future::poll(request.as_mut(), cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                polled.wait().await;
                request.await
            });
        }
        polled.wait().await;
        eprintln!(
            "locked final pass, eight pending retrievals, idle pool connections={}",
            store.pool.num_idle()
        );
        release.add_permits(1);
        let switched = switching.join_next().await.unwrap()?;
        anyhow::ensure!(
            switched.is_ok(),
            "model switch blocked by opt-in snapshots: {switched:?}"
        );
        while let Some(retrieval) = requests.join_next().await {
            let retrieval = retrieval??;
            anyhow::ensure!(
                retrieval.embedding_space.as_deref() == Some(space.as_str())
                    && retrieval.items.len() == 2,
                "retrieval did not resume in the new model space"
            );
        }
        anyhow::ensure!(store.setting("embedding").await?.as_deref() == Some(spec.as_str()));
        anyhow::ensure!(store.missing_vectors(&space, 8).await?.is_empty());
        Ok::<_, anyhow::Error>(())
    })
    .await;
    server.abort();
    result?
}

async fn insert(store: &Store, id: &str, text: &str) -> anyhow::Result<()> {
    let mut tx = store.begin_write().await?;
    sqlx::query("insert into memories(id,scope,kind,created_by,current_version) values($1,'allowed','fact','fixture',1)")
        .bind(id).execute(&mut *tx).await?;
    sqlx::query("insert into memory_versions(memory_id,version,text,source,author,content_hash) values($1,1,$2,'fixture','fixture','hash')")
        .bind(id).bind(text).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
