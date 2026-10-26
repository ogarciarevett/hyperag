use super::*;
use crate::store::Store;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::str::FromStr;
use tantivy::{
    Term,
    postings::Postings,
    schema::{IndexRecordOption, Value},
};

#[tokio::test]
async fn pending_disk_commits_never_recover_as_a_completed_generation() {
    let url = std::env::var("HYPERAG_PG_URL")
        .unwrap_or_else(|_| "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into());
    let options = PgConnectOptions::from_str(&url).unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name = format!("lexical_generation_{}", hex::encode(nonce));
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options.database(&name))
        .await
        .unwrap();
    Store::new(pool.clone()).migrate().await.unwrap();
    let result = exercise(&pool).await;
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "drop database {name} with(force)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn exercise(pool: &PgPool) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let old_path = root.path().join("old");
    let pending_path = root.path().join("pending");
    let complete_path = root.path().join("complete");
    let empty = SourceSnapshot::open(pool, None).await?;
    let mut old = Generation::create(&old_path, empty)?;
    assert!(
        old.publish().is_err(),
        "an undrained snapshot must not publish"
    );
    assert!(!old.advance().await?);
    let zero = old.target.clone();
    let pinned = old.publish()?;
    assert_eq!(pinned.num_docs(), 0);
    assert!(
        recover(&old_path, &zero).is_err(),
        "only one local owner may open a generation"
    );
    assert!(
        Generation::create(&old_path, SourceSnapshot::open(pool, None).await?).is_err(),
        "never overwrite an existing generation"
    );
    drop(old);
    assert_eq!(recover(&old_path, &zero)?.unwrap().num_docs(), 0);

    sqlx::raw_sql("begin; select pg_advisory_xact_lock(1213811052,1);
        insert into memories(id,scope,kind,created_by,current_version)
        select 'id-'||lpad(n::text,3,'0'),'allowed','fact','fixture',1 from generate_series(1,130) n;
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        select id,1,'ALPHA alpha rare!','fixture','fixture','hash' from memories; commit").execute(pool).await?;
    let source = SourceSnapshot::open(pool, None).await?;
    let target = source.target_checkpoint().clone();
    let mut partial = Generation::create(&pending_path, source)?;
    assert!(partial.advance().await?);
    assert!(partial.publish().is_err());
    drop(partial);
    // Both the target fence and a restore to the prior source fence must refuse partial disk state.
    assert!(recover(&pending_path, &target)?.is_none());
    assert!(recover(&pending_path, &zero)?.is_none());
    assert_eq!(pinned.num_docs(), 0);

    let source = SourceSnapshot::open(pool, None).await?;
    let mut complete = Generation::create(&complete_path, source)?;
    while complete.advance().await? {}
    // Simulate process loss after the final durable commit but before reader reload/publication.
    complete.commit_state(true)?;
    assert_eq!(complete.reader.searcher().num_docs(), 0);
    drop(complete);
    let recovered = recover(&complete_path, &target)?.unwrap();
    assert_eq!(recovered.num_docs(), 130);
    assert_eq!(pinned.num_docs(), 0);
    let terms = recovered.schema().get_field("terms")?;
    let alpha = Term::from_field_text(terms, "alpha");
    assert_eq!(recovered.doc_freq(&alpha)?, 130);
    let postings = recovered.segment_readers()[0]
        .inverted_index(terms)?
        .read_postings(&alpha, IndexRecordOption::WithFreqs)?
        .unwrap();
    assert_eq!(postings.term_freq(), 2);
    let doc: tantivy::TantivyDocument = recovered.doc(tantivy::DocAddress::new(0, 0))?;
    let scope = doc
        .get_first(recovered.schema().get_field("scope")?)
        .unwrap();
    assert_eq!(scope.as_str(), Some("allowed"));
    assert!(
        doc.get_first(terms).is_none(),
        "terms/full source text are not stored document fields"
    );
    for bad in [
        Checkpoint {
            database_id: "other".into(),
            ..target.clone()
        },
        Checkpoint {
            epoch: target.epoch + 1,
            ..target.clone()
        },
        Checkpoint {
            fence_token: "other".into(),
            ..target.clone()
        },
    ] {
        assert!(recover(&complete_path, &bad)?.is_none());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&complete_path)?.permissions().mode() & 0o777,
            0o700
        );
    }
    let source = SourceSnapshot::open(pool, None).await?;
    let mut published = Generation::create(&root.path().join("published"), source)?;
    while published.advance().await? {}
    assert_eq!(published.publish()?.num_docs(), 130);
    assert!(published.advance().await.is_err());
    drop(published);
    let meta_path = complete_path.join("meta.json");
    let original = std::fs::read(&meta_path)?;
    let metadata: serde_json::Value = serde_json::from_slice(&original)?;
    let payload: serde_json::Value = serde_json::from_str(metadata["payload"].as_str().unwrap())?;
    for (field, value) in [
        ("format", serde_json::json!(2)),
        ("schema_hash", serde_json::json!("other")),
    ] {
        let mut invalid = payload.clone();
        invalid[field] = value;
        let mut meta = metadata.clone();
        meta["payload"] = serde_json::to_string(&invalid)?.into();
        std::fs::write(&meta_path, serde_json::to_vec(&meta)?)?;
        assert!(recover(&complete_path, &target)?.is_none());
    }
    let mut meta = metadata;
    meta["payload"] = "not JSON".into();
    std::fs::write(&meta_path, serde_json::to_vec(&meta)?)?;
    assert!(recover(&complete_path, &target).is_err());
    std::fs::write(&meta_path, original)?;
    assert_eq!(recover(&complete_path, &target)?.unwrap().num_docs(), 130);
    let failed_path = root.path().join("failed");
    let moved_path = root.path().join("unavailable");
    let mut failed = Generation::create(&failed_path, SourceSnapshot::open(pool, None).await?)?;
    std::fs::rename(&failed_path, &moved_path)?;
    assert!(
        failed.advance().await.is_err(),
        "a failed disk commit must abort the generation"
    );
    std::fs::rename(&moved_path, &failed_path)?;
    assert!(
        failed.advance().await.is_err(),
        "retry cannot skip the consumed source page"
    );
    assert!(failed.publish().is_err());
    drop(failed);
    assert!(recover(&failed_path, &target)?.is_none());
    Ok(())
}
