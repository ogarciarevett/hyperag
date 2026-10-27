use super::*;
use crate::core::{Channels, Config, Hyperag, RetrieveOptions};
use std::sync::{Arc, Barrier};

async fn seed_two_pages(store: &crate::store::Store) -> anyhow::Result<()> {
    let mut tx = store.begin_write().await?;
    sqlx::raw_sql(
        "insert into memories(id,scope,kind,created_by,current_version)
        select 'id-'||lpad(n::text,3,'0'),'allowed','fact','fixture',1 from generate_series(1,130) n;
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        select id,1,'quartz','fixture','fixture','hash' from memories;
        insert into grants(principal,scope,permission,granted_by)
        values('reader','allowed','read','fixture')",
    ).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

fn pause_first_step() -> (Arc<Barrier>, Arc<Barrier>, Arc<dyn Fn() + Send + Sync>) {
    let (entered, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let (hook_entered, hook_release, hooked) = (entered.clone(), release.clone(), Arc::new(AtomicBool::new(false)));
    let hook = Arc::new(move || if !hooked.swap(true, Ordering::AcqRel) { hook_entered.wait(); hook_release.wait(); });
    (entered, release, hook)
}

#[tokio::test]
async fn stopping_pending_work_releases_snapshot_and_root() -> anyhow::Result<()> {
    crate::lexical_read::tests::isolated(
        "lexical_worker::tests::stopping_pending_work_releases_snapshot_and_root",
        |store: crate::store::Store| async move {
            seed_two_pages(&store).await?;
            let scratch = tempfile::tempdir()?;
            let root = scratch.path().join("index");
            let consumer = Consumer::open(&root)?;
            let (entered, release, hook) = pause_first_step();
            let writer = Worker::start_with(consumer, store.pool.clone(), Duration::from_secs(5), Some(hook));
            tokio::task::spawn_blocking(move || entered.wait()).await?;
            assert!(!root.join("CURRENT").exists());
            let releasing = tokio::task::spawn_blocking(move || release.wait());
            writer.close().await?;
            releasing.await?;
            assert!(!root.join("CURRENT").exists(), "interrupted work was published");
            let mut next = Consumer::open(&root)?;
            next.update(&store.pool).await?;
            assert!(root.join("CURRENT").exists());
            drop(next);
            tokio::time::timeout(Duration::from_secs(5), store.pool.close()).await?;
            Ok(())
        },
    ).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn one_worker_runtime_answers_requests_and_bounds_shutdown_during_slow_commit() -> anyhow::Result<()> {
    crate::lexical_read::tests::isolated(
        "lexical_worker::tests::one_worker_runtime_answers_requests_and_bounds_shutdown_during_slow_commit",
        |mut store: crate::store::Store| async move {
            seed_two_pages(&store).await?;
            let scratch = tempfile::tempdir()?;
            let root = scratch.path().join("index");
            let consumer = Consumer::open(&root)?;
            store.lexical_reader = consumer.reader();
            let core = Hyperag::init(Config::new(store.clone())).await?;
            let (entered, release, hook) = pause_first_step();
            let writer = Worker::start_with(consumer, store.pool.clone(), Duration::from_millis(100), Some(hook));
            tokio::task::spawn_blocking(move || entered.wait()).await?;
            let options = RetrieveOptions {
                channels: Some(Channels { vector: Some(false), graph: Some(false), rerank: Some(false), ..Default::default() }),
                ..Default::default()
            };
            let retrieved = tokio::time::timeout(Duration::from_millis(500), core.retrieve("reader", "quartz", &options)).await??;
            assert!(!retrieved.items.is_empty());
            let started = std::time::Instant::now();
            writer.close().await?;
            store.pool.close().await;
            assert!(started.elapsed() < Duration::from_secs(1), "shutdown and pool close did not finish within deadline");
            assert!(store.pool.is_closed());
            tokio::task::spawn_blocking(move || release.wait()).await?;
            for _ in 0..20 {
                if Consumer::open(&root).is_ok() { break; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            drop(Consumer::open(&root)?);
            assert!(!root.join("CURRENT").exists(), "uncompleted work must stay pending when deadline expires");
            Ok(())
        },
    ).await
}
