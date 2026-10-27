use super::*;

#[tokio::test]
async fn stopping_pending_work_releases_snapshot_and_root() -> anyhow::Result<()> {
    crate::lexical_read::tests::isolated(
        "lexical_worker::tests::stopping_pending_work_releases_snapshot_and_root",
        |store: crate::store::Store| async move {
            let mut tx = store.begin_write().await?;
            sqlx::raw_sql(
                "insert into memories(id,scope,kind,created_by,current_version)
                select 'id-'||n,'allowed','fact','fixture',1 from generate_series(1,130) n;
                insert into memory_versions(memory_id,version,text,source,author,content_hash)
                select id,1,'quartz','fixture','fixture','hash' from memories",
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            let scratch = tempfile::tempdir()?;
            let root = scratch.path().join("index");
            let mut consumer = Consumer::open(&root)?;
            assert!(consumer.step(&store.pool).await?);
            assert!(!root.join("CURRENT").exists());
            let writer = Worker::start(consumer, store.pool.clone());
            writer.close().await?;
            assert!(
                !root.join("CURRENT").exists(),
                "interrupted work was published"
            );
            drop(Consumer::open(&root)?);
            tokio::time::timeout(Duration::from_secs(5), store.pool.close()).await?;
            Ok(())
        },
    )
    .await
}
