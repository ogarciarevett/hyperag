use super::*;
use crate::{lexical_snapshot::SourceSnapshot, store::Store};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use tantivy::Term;

async fn write(pool: &PgPool, sql: &str) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(1213811052,1)")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn checkpoint(pool: &PgPool) -> anyhow::Result<Checkpoint> {
    Ok(SourceSnapshot::open(pool, None)
        .await?
        .target_checkpoint()
        .clone())
}

fn frequency(searcher: &Searcher, text: &str) -> u64 {
    let term = Term::from_field_text(searcher.schema().get_field("terms").unwrap(), text);
    searcher
        .search(
            &tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic),
            &tantivy::collector::Count,
        )
        .unwrap() as u64
}

#[tokio::test]
async fn replay_retains_unchanged_rows_and_publishes_only_complete_generations() {
    let options =
        PgConnectOptions::from_str(&std::env::var("HYPERAG_PG_URL").unwrap_or_else(|_| {
            "postgres://hyperag:fake-test-only@127.0.0.1:55432/hyperag".into()
        }))
        .unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await
        .unwrap();
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name = format!("lexical_consumer_{}", hex::encode(nonce));
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
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("index");
    write(pool, "insert into memories(id,scope,kind,created_by,current_version)
        select 'id-'||lpad(n::text,3,'0'),'allowed','fact','fixture',1 from generate_series(1,130) n;
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        select id,1,'baselexeme','fixture','fixture','hash' from memories").await?;
    let mut consumer = Consumer::open(&root)?;
    assert!(
        Consumer::open(&root).is_err(),
        "one local consumer owns each root"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root)?.permissions().mode() & 0o777,
            0o700
        );
    }
    let first = consumer.update(pool).await?;
    let pinned = consumer.at(&first).unwrap();
    assert_eq!(pinned.num_docs(), 130);
    assert_eq!(frequency(&pinned, "baselexeme"), 130);
    let reader = consumer.reader();
    let original_pointer = std::fs::read(root.join("CURRENT"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.join("CURRENT"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        drop(consumer);
        let generation: String = serde_json::from_slice(&original_pointer)?;
        let meta = root.join(&generation).join("meta.json");
        let meta_mode = std::fs::metadata(&meta)?.permissions().mode() & 0o777;
        for (path, unsafe_mode, private_mode) in [
            (root.join("CURRENT"), 0o644, 0o600),
            (root.join(&generation), 0o755, 0o700),
            (meta, 0o666, meta_mode),
            (root.join(".consumer.lock"), 0o666, 0o600),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(unsafe_mode))?;
            let rejected = Consumer::open(&root).is_err();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(private_mode))?;
            anyhow::ensure!(
                rejected,
                "unsafe recovered path accepted: {}",
                path.display()
            );
        }
        consumer = Consumer::open(&root)?;
    }
    assert_eq!(consumer.update(pool).await?, first);
    assert_eq!(
        std::fs::read(root.join("CURRENT"))?,
        original_pointer,
        "no-op keeps the generation"
    );
    write(pool, "insert into memory_versions(memory_id,version,text,source,author,content_hash)
        values('id-005',2,'newlexeme','fixture','fixture','new');
        update memories set current_version=2 where id='id-005';
        delete from memories where id='id-006';
        update memory_versions set status='invalidated' where memory_id='id-007';
        update memories set scope='denied' where id='id-008';
        insert into grants(principal,scope,permission,granted_by) values('reader','allowed','read','fixture')").await?;
    write(pool, "update memory_versions v set text=case when m.id='id-005' then 'finallexeme' else 'updatedlexeme' end
        from memories m where v.memory_id=m.id and v.version=m.current_version and m.id<='id-100';
        insert into memories(id,scope,kind,created_by,current_version) values('id-131','allowed','fact','fixture',1);
        insert into memory_versions(memory_id,version,text,source,author,content_hash)
        values('id-131',1,'arrivallexeme','fixture','fixture','arrival')").await?;
    let next = checkpoint(pool).await?;
    assert!(consumer.at(&next).is_none(), "lag must use SQL fallback");
    assert!(consumer.step(pool).await?);
    assert!(consumer.pending.is_some());
    assert!(reader.at(&next).is_none());
    let pending_path = root.join(&consumer.pending.as_ref().unwrap().name);
    let moved_pending = scratch.path().join("failed-step");
    std::fs::rename(&pending_path, &moved_pending)?;
    let failed_step = consumer.step(pool).await;
    std::fs::rename(&moved_pending, &pending_path)?;
    assert!(failed_step.is_err());
    assert!(
        consumer.pending.is_none(),
        "failed step must release its snapshot"
    );
    assert_eq!(reader.at(&first).unwrap().num_docs(), 130);
    assert_eq!(std::fs::read(root.join("CURRENT"))?, original_pointer);
    // Drop a partially committed replay: the pointer and independent readers stay old.
    let mut pending = consumer.prepare(pool).await?.unwrap();
    assert!(pending.generation.advance().await?);
    assert!(pending.generation.publish().is_err());
    assert_eq!(reader.at(&first).unwrap().num_docs(), 130);
    assert!(reader.at(&next).is_none());
    let pending_name = pending.name.clone();
    drop(pending);
    assert_eq!(std::fs::read(root.join("CURRENT"))?, original_pointer);
    drop(consumer);
    let mut consumer = Consumer::open(&root)?;
    assert_eq!(consumer.at(&first).unwrap().num_docs(), 130);
    assert!(
        consumer.at(&next).is_none(),
        "pending orphan cannot be selected"
    );
    let mut ready = consumer.prepare(pool).await?.unwrap();
    while ready.generation.advance().await? {}
    let complete = ready.generation.publish()?;
    drop(ready.generation);
    assert_eq!(std::fs::read(root.join("CURRENT"))?, original_pointer);
    assert!(
        consumer.at(&next).is_none(),
        "complete orphan is still inactive"
    );
    // An activation failure before rename preserves both the pointer and reader.
    let moved = scratch.path().join("moved");
    std::fs::rename(&root, &moved)?;
    let activation = consumer.activate(ready.name, ready.checkpoint, complete);
    std::fs::rename(&moved, &root)?;
    assert!(activation.is_err());
    assert_eq!(consumer.at(&first).unwrap().num_docs(), 130);
    assert_eq!(std::fs::read(root.join("CURRENT"))?, original_pointer);
    // Model interruption after durable rename but before the in-memory swap.
    let mut ready = consumer.prepare(pool).await?.unwrap();
    while ready.generation.advance().await? {}
    let complete = ready.generation.publish()?;
    drop(ready.generation);
    let mut pointer = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer(&mut pointer, &ready.name)?;
    pointer.as_file().sync_all()?;
    pointer.persist(root.join("CURRENT"))?;
    std::fs::File::open(&root)?.sync_all()?;
    assert_eq!(consumer.at(&first).unwrap().num_docs(), 130);
    assert!(consumer.at(&next).is_none());
    drop(consumer);
    let mut consumer = Consumer::open(&root)?;
    assert_eq!(consumer.at(&next).unwrap().num_docs(), complete.num_docs());
    assert_eq!(reader.at(&first).unwrap().num_docs(), 130);
    assert_eq!(consumer.update(pool).await?, next);
    let updated = consumer.at(&next).unwrap();
    assert_eq!(updated.num_docs(), 129);
    assert_eq!(
        frequency(&updated, "baselexeme"),
        30,
        "unchanged rows survive replay"
    );
    assert_eq!(frequency(&updated, "finallexeme"), 1);
    assert_eq!(frequency(&updated, "updatedlexeme"), 97);
    assert_eq!(pinned.num_docs(), 130);
    assert_eq!(frequency(&pinned, "baselexeme"), 130);
    assert_ne!(std::fs::read(root.join("CURRENT"))?, original_pointer);
    drop(consumer);
    let mut consumer = Consumer::open(&root)?;
    assert_eq!(consumer.at(&next).unwrap().num_docs(), 129);
    // Marker-only epochs still publish the new fence without dropping the copied corpus.
    write(pool, "delete from grants").await?;
    let marker = consumer.update(pool).await?;
    assert!(marker.epoch > next.epoch);
    assert_eq!(consumer.at(&marker).unwrap().num_docs(), 129);
    // An outbox floor ahead of the active checkpoint forces a fresh rebuild.
    write(
        pool,
        "update memory_versions set text='gaplexeme' where memory_id='id-001'",
    )
    .await?;
    write(
        pool,
        "select lexical_index_prune((select epoch from lexical_index_state))",
    )
    .await?;
    let rebuilt = consumer.update(pool).await?;
    assert_eq!(consumer.at(&rebuilt).unwrap().num_docs(), 129);
    assert_eq!(frequency(&consumer.at(&rebuilt).unwrap(), "gaplexeme"), 1);
    write(pool, "truncate grants").await?;
    let reset = consumer.update(pool).await?;
    assert!(reset.epoch > rebuilt.epoch);
    assert_eq!(consumer.at(&reset).unwrap().num_docs(), 129);
    // A divergent restore at the same epoch has a different token and must rebuild.
    write(
        pool,
        "update lexical_index_state set fence_token=gen_random_uuid()",
    )
    .await?;
    let restored = checkpoint(pool).await?;
    assert!(consumer.at(&restored).is_none());
    assert_eq!(consumer.update(pool).await?, restored);
    assert_eq!(consumer.at(&restored).unwrap().num_docs(), 129);
    write(
        pool,
        "update memory_versions set text='holelexeme' where memory_id='id-002';
        delete from lexical_index_changes where epoch=(select epoch from lexical_index_state)",
    )
    .await?;
    let after_hole = consumer.update(pool).await?;
    assert_eq!(
        frequency(&consumer.at(&after_hole).unwrap(), "holelexeme"),
        1
    );
    drop(consumer);
    std::fs::write(root.join("CURRENT"), b"\"../../outside\"")?;
    let mut consumer = Consumer::open(&root)?;
    assert!(consumer.at(&after_hole).is_none());
    assert_eq!(consumer.update(pool).await?, after_hole);
    assert_eq!(consumer.at(&after_hole).unwrap().num_docs(), 129);
    drop(consumer);
    std::fs::write(root.join("CURRENT"), serde_json::to_vec(&pending_name)?)?;
    let consumer = Consumer::open(&root)?;
    assert!(
        consumer.at(&next).is_none(),
        "CURRENT cannot select a pending generation"
    );
    drop(consumer);
    #[cfg(unix)]
    {
        let link = format!("gen-{}", "a".repeat(32));
        std::os::unix::fs::symlink(root.join(&ready.name), root.join(&link))?;
        std::fs::write(root.join("CURRENT"), serde_json::to_vec(&link)?)?;
        assert!(Consumer::open(&root).is_err());
        let alias = scratch.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias)?;
        assert!(Consumer::open(&alias).is_err());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_preexisting_public_root_before_creating_lock() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("public");
    std::fs::create_dir(&root)?;
    for mode in [0o777, 0o770, 0o755] {
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(mode))?;
        anyhow::ensure!(
            Consumer::open(&root).is_err(),
            "unsafe root mode {mode:o} accepted"
        );
        anyhow::ensure!(
            !root.join(".consumer.lock").exists(),
            "unsafe root was locked"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_replaceable_ancestors_before_creating_lock() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir()?;
    let ancestor = scratch.path().join("replaceable");
    let parent = ancestor.join("private-parent");
    let root = parent.join("index");
    std::fs::create_dir(&ancestor)?;
    std::fs::create_dir(&parent)?;
    std::fs::create_dir(&root)?;
    for path in [&ancestor, &parent, &root] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    for path in [&ancestor, &parent] {
        for mode in [0o777, 0o770, 0o775] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
            let rejected = Consumer::open(&root)
                .err()
                .expect("replaceable ancestor accepted");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            anyhow::ensure!(
                rejected.to_string().contains(&path.display().to_string()),
                "unsafe ancestor diagnostic must name {}: {rejected}",
                path.display()
            );
            anyhow::ensure!(
                !root.join(".consumer.lock").exists(),
                "unsafe root was locked"
            );
        }
    }
    let file = scratch.path().join("not-a-directory");
    std::fs::write(&file, b"fixture")?;
    let file = file.canonicalize()?;
    let rejected = check_ancestors(&file, current_owner()).unwrap_err();
    anyhow::ensure!(rejected.to_string().contains(&file.display().to_string()));
    let file = scratch.path().join("file-parent");
    std::fs::write(&file, b"fixture")?;
    let error = Consumer::open(&file.join("index")).err().unwrap();
    anyhow::ensure!(error.to_string().contains(&file.display().to_string()));
    // Sticky protection prevents other users from replacing our entry.
    std::fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o1777))?;
    drop(Consumer::open(&root)?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_path_policy_rejects_a_different_owner() -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let scratch = tempfile::tempdir()?;
    let directory = scratch.path().join("generation");
    std::fs::create_dir(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let pointer = scratch.path().join("CURRENT");
    std::fs::write(&pointer, b"null")?;
    std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0o600))?;
    for (path, is_directory) in [(&directory, true), (&pointer, false)] {
        let owner = std::fs::symlink_metadata(path)?.uid();
        anyhow::ensure!(check_private(path, is_directory, owner).is_ok());
        anyhow::ensure!(
            check_private(path, is_directory, owner.wrapping_add(1)).is_err(),
            "foreign owner accepted"
        );
    }
    let owner = std::fs::metadata(&directory)?.uid();
    anyhow::ensure!(check_ancestors(&directory.canonicalize()?, owner).is_ok());
    if owner != 0 {
        anyhow::ensure!(
            check_ancestors(&directory.canonicalize()?, owner.wrapping_add(1)).is_err()
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn opened_file_policy_and_resolved_root_survive_path_replacement() -> anyhow::Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let scratch = tempfile::tempdir()?;
    let pointer = scratch.path().join("CURRENT");
    std::fs::write(&pointer, b"null")?;
    std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0o666))?;
    let opened = std::fs::File::open(&pointer)?;
    std::fs::rename(&pointer, scratch.path().join("old"))?;
    std::fs::write(&pointer, b"null")?;
    std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0o600))?;
    check_private(&pointer, false, current_owner())?;
    anyhow::ensure!(
        check_private_metadata(&opened.metadata()?, &pointer, false, current_owner()).is_err(),
        "path metadata must not substitute for opened file metadata"
    );
    let alias = scratch.path().join("alias");
    symlink(scratch.path(), &alias)?;
    let consumer = Consumer::open(&alias.join("index"))?;
    std::fs::remove_file(&alias)?;
    symlink(scratch.path().join("elsewhere"), &alias)?;
    assert_eq!(consumer.root, scratch.path().canonicalize()?.join("index"));
    anyhow::ensure!(
        Consumer::open(&consumer.root).is_err(),
        "ownership lock lost"
    );
    drop(consumer);
    drop(Consumer::open(&scratch.path().join("index"))?);
    Ok(())
}

#[test]
fn new_root_syncs_parent_and_propagates_sync_failure() -> anyhow::Result<()> {
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("new");
    let calls = std::cell::Cell::new(0);
    let opened = Consumer::open_with(&root, |parent| {
        assert_eq!(parent, scratch.path().canonicalize().unwrap());
        calls.set(calls.get() + 1);
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected parent sync failure",
        ))
    });
    anyhow::ensure!(opened.is_err(), "parent sync failure was ignored");
    assert_eq!(calls.get(), 1);
    anyhow::ensure!(!root.exists(), "failed new root must be removed for retry");
    let consumer = Consumer::open_with(&root, |parent| {
        assert_eq!(parent, scratch.path().canonicalize().unwrap());
        calls.set(calls.get() + 1);
        Ok(())
    })?;
    drop(consumer);
    assert_eq!(calls.get(), 2);
    drop(Consumer::open_with(&root, |_| {
        panic!("existing root must not require parent sync")
    })?);
    Ok(())
}
