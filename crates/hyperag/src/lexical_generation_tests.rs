use super::*;
use crate::store::Store;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{future::Future, str::FromStr};
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
        select id,1,(case id when 'id-010' then repeat('alpha ',101) || 'rare' else 'ALPHA alpha rare!' end) || case id when 'id-005' then ' replacedlexeme' when 'id-006' then ' deletedlexeme' else '' end,'fixture','fixture','hash' from memories; commit").execute(pool).await?;
    let source = SourceSnapshot::open(pool, None).await?;
    let target = source.target_checkpoint().clone();
    let replay = SourceSnapshot::open(pool, Some(&target)).await?;
    anyhow::ensure!(
        Generation::create(&root.path().join("replay"), replay).is_err(),
        "a fresh index must reject a replay that omits the 130 unchanged documents"
    );
    let mut consumed = SourceSnapshot::open(pool, None).await?;
    assert!(consumed.next_batch().await?.is_some());
    anyhow::ensure!(
        Generation::create(&root.path().join("consumed"), consumed).is_err(),
        "a fresh index must reject a previously consumed source page"
    );
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
    sqlx::raw_sql(
        "begin; select pg_advisory_xact_lock(1213811052,1);
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        values('id-005',2,'incrementallexeme','fixture','fixture','changed');
        update memories set current_version=2 where id='id-005';
        delete from memories where id='id-006';
        update memory_versions set status='invalidated' where memory_id='id-007'; commit",
    )
    .execute(pool)
    .await?;
    let replay_source = SourceSnapshot::open(pool, Some(&target)).await?;
    let replay_target = replay_source.target_checkpoint().clone();
    let full_source = SourceSnapshot::open(pool, None).await?;
    assert_eq!(full_source.target_checkpoint(), &replay_target);
    let replay_path = root.path().join("replayed");
    let base_metadata = std::fs::read(&meta_path)?;
    let cancelled_path = root.path().join("cancelled-copy");
    let cancelled_source = SourceSnapshot::open(pool, Some(&target)).await?;
    let mut cancelled = Box::pin(Generation::replay(
        &cancelled_path, &complete_path, cancelled_source,
    ));
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(cancelled.as_mut().poll(cx)))
        .await.is_pending());
    drop(cancelled);
    assert!(recover(&cancelled_path, &target)?.is_none());
    assert!(recover(&cancelled_path, &replay_target)?.is_none());
    assert_eq!(recover(&complete_path, &target)?.unwrap().num_docs(), 130);
    let mut replay = Generation::replay(&replay_path, &complete_path, replay_source).await?;
    assert!(
        recover(&replay_path, &target).is_err(),
        "the copied index has a single owner"
    );
    assert!(replay.advance().await?);
    assert!(
        replay.publish().is_err(),
        "a replay page is not source exhaustion"
    );
    sqlx::raw_sql(
        "begin; select pg_advisory_xact_lock(1213811052,1);
        update memory_versions set text='futurelexeme' where memory_id='id-009'; commit",
    )
    .execute(pool)
    .await?;
    assert!(
        replay.advance().await?,
        "deleted postings require a fenced rebuild"
    );
    assert!(
        replay.publish().is_err(),
        "rewinding cannot publish an empty index"
    );
    while replay.advance().await? {}
    let replayed = replay.publish()?;
    assert_eq!(replayed.num_docs(), 128);
    assert_eq!(
        replayed.search(
            &tantivy::query::TermQuery::new(alpha.clone(), IndexRecordOption::Basic),
            &tantivy::collector::Count
        )?,
        127
    );
    let mut rebuilt = Generation::create(&root.path().join("rebuilt"), full_source)?;
    while rebuilt.advance().await? {}
    let rebuilt = rebuilt.publish()?;
    assert_eq!(replayed.num_docs(), rebuilt.num_docs());
    for text in [
        "alpha",
        "rare",
        "incrementallexeme",
        "deletedlexeme",
        "replacedlexeme",
        "futurelexeme",
    ] {
        let term = Term::from_field_text(terms, text);
        println!(
            "replay-vs-rebuild term={text} replay_df={} rebuild_df={}",
            replayed.doc_freq(&term)?,
            rebuilt.doc_freq(&term)?
        );
        anyhow::ensure!(
            replayed.doc_freq(&term)? == rebuilt.doc_freq(&term)?,
            "replay/rebuild doc_freq mismatch for {text}"
        );
        let query = tantivy::query::TermQuery::new(term, IndexRecordOption::WithFreqs);
        let scored = |searcher: &Searcher| -> anyhow::Result<Vec<(String, f32)>> {
            let mut hits = Vec::new();
            for (score, address) in searcher.search(
                &query,
                &tantivy::collector::TopDocs::with_limit(200).order_by_score(),
            )? {
                let doc: tantivy::TantivyDocument = searcher.doc(address)?;
                hits.push((
                    doc.get_first(searcher.schema().get_field("id")?)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    score,
                ));
            }
            hits.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(hits)
        };
        anyhow::ensure!(
            scored(&replayed)? == scored(&rebuilt)?,
            "BM25 scores differ for {text}"
        );
    }
    let statistics = |searcher: &Searcher| -> anyhow::Result<(u64, u64)> {
        searcher
            .segment_readers()
            .iter()
            .try_fold((0, 0), |(docs, tokens), segment| {
                Ok((
                    docs + u64::from(segment.max_doc()),
                    tokens + segment.inverted_index(terms)?.total_num_tokens(),
                ))
            })
    };
    println!(
        "replay-vs-rebuild raw stats replay={:?} rebuild={:?}",
        statistics(&replayed)?,
        statistics(&rebuilt)?
    );
    assert_eq!(statistics(&replayed)?, statistics(&rebuilt)?);
    assert_eq!(statistics(&replayed)?.0, replayed.num_docs());
    assert_eq!(recovered.num_docs(), 130);
    drop(replay);
    assert!(recover(&replay_path, &target)?.is_none());
    assert_eq!(
        recover(&replay_path, &replay_target)?.unwrap().num_docs(),
        128
    );
    assert_eq!(std::fs::read(&meta_path)?, base_metadata);
    let full = SourceSnapshot::open(pool, None).await?;
    assert!(Generation::replay(&root.path().join("bad-full"), &complete_path, full).await.is_err());
    let mut consumed = SourceSnapshot::open(pool, Some(&target)).await?;
    assert!(consumed.next_batch().await?.is_some());
    assert!(
        Generation::replay(&root.path().join("bad-consumed"), &complete_path, consumed).await.is_err()
    );
    let source = SourceSnapshot::open(pool, Some(&target)).await?;
    assert!(Generation::replay(&root.path().join("bad-base"), &old_path, source).await.is_err());
    let source = SourceSnapshot::open(pool, Some(&target)).await?;
    assert!(Generation::replay(&root.path().join("bad-pending"), &pending_path, source).await.is_err());
    Ok(())
}
