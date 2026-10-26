//! Explicit local updates; query integration must supply a fresh source fence.
use crate::lexical_generation::{self, Generation};
use crate::lexical_snapshot::{Checkpoint, SourceSnapshot};
use sqlx::PgPool;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tantivy::{
    Searcher,
    directory::{Directory, DirectoryLock, Lock, MmapDirectory},
};

#[derive(Clone)]
struct Active {
    name: String,
    checkpoint: Checkpoint,
    searcher: Searcher,
}

#[derive(Clone, Default)]
pub(super) struct ReaderHandle(Arc<RwLock<Option<Active>>>);

impl ReaderHandle {
    pub fn at(&self, source: &Checkpoint) -> Option<Searcher> {
        let active = self.0.read().ok()?;
        let active = active.as_ref()?;
        (active.checkpoint == *source).then(|| active.searcher.clone())
    }
}

pub(super) struct Consumer {
    root: PathBuf,
    reader: ReaderHandle,
    _owner: DirectoryLock,
}

struct Prepared {
    name: String,
    checkpoint: Checkpoint,
    generation: Generation,
}

fn valid_name(name: &str) -> bool {
    name.len() == 36
        && name.starts_with("gen-")
        && name.as_bytes()[4..].iter().all(u8::is_ascii_hexdigit)
}

fn generation_name() -> anyhow::Result<String> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)?;
    Ok(format!("gen-{}", hex::encode(nonce)))
}

fn current(root: &Path) -> anyhow::Result<Option<Active>> {
    if !fs::symlink_metadata(root.join("CURRENT"))?
        .file_type()
        .is_file()
    {
        return Ok(None);
    }
    let name: String = serde_json::from_slice(&fs::read(root.join("CURRENT"))?)?;
    if !valid_name(&name) || !fs::symlink_metadata(root.join(&name))?.file_type().is_dir() {
        return Ok(None);
    }
    Ok(
        lexical_generation::recover_complete(&root.join(&name), None)?.map(
            |(checkpoint, searcher)| Active {
                name,
                checkpoint,
                searcher,
            },
        ),
    )
}

impl Consumer {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let mut directory = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        match directory.create(root) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(
            fs::symlink_metadata(root)?.file_type().is_dir(),
            "consumer root must be a real directory"
        );
        let owner = MmapDirectory::open(root)?.acquire_lock(&Lock {
            filepath: ".consumer.lock".into(),
            is_blocking: false,
        })?;
        // Missing, malformed or incompatible pointers require an explicit rebuild.
        let reader = ReaderHandle(Arc::new(RwLock::new(current(root).unwrap_or(None))));
        Ok(Self {
            root: root.into(),
            reader,
            _owner: owner,
        })
    }

    pub fn reader(&self) -> ReaderHandle {
        self.reader.clone()
    }

    pub fn at(&self, source: &Checkpoint) -> Option<Searcher> {
        self.reader.at(source)
    }

    async fn prepare(&self, pool: &PgPool) -> anyhow::Result<Option<Prepared>> {
        let active = self
            .reader
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("published reader lock poisoned"))?
            .clone();
        if let Some(active) = active {
            if let Ok(source) = SourceSnapshot::open(pool, Some(&active.checkpoint)).await {
                if source.target_checkpoint() == &active.checkpoint {
                    return Ok(None);
                }
                let name = generation_name()?;
                let checkpoint = source.target_checkpoint().clone();
                if let Ok(generation) = Generation::replay(
                    &self.root.join(&name),
                    &self.root.join(&active.name),
                    source,
                ) {
                    return Ok(Some(Prepared {
                        name,
                        checkpoint,
                        generation,
                    }));
                }
            }
        }
        // Refused lineage or base requires a full source in a different fresh directory.
        let source = SourceSnapshot::open(pool, None).await?;
        let name = generation_name()?;
        let checkpoint = source.target_checkpoint().clone();
        let generation = Generation::create(&self.root.join(&name), source)?;
        Ok(Some(Prepared {
            name,
            checkpoint,
            generation,
        }))
    }

    pub async fn update(&mut self, pool: &PgPool) -> anyhow::Result<Checkpoint> {
        let Some(mut pending) = self.prepare(pool).await? else {
            return Ok(self
                .reader
                .0
                .read()
                .map_err(|_| anyhow::anyhow!("published reader lock poisoned"))?
                .as_ref()
                .expect("no-op requires an active generation")
                .checkpoint
                .clone());
        };
        while pending.generation.advance().await? {}
        let searcher = pending.generation.publish()?;
        drop(pending.generation);
        self.activate(pending.name, pending.checkpoint.clone(), searcher)?;
        Ok(pending.checkpoint)
    }

    fn activate(
        &mut self,
        name: String,
        checkpoint: Checkpoint,
        searcher: Searcher,
    ) -> anyhow::Result<()> {
        let mut pointer = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer(&mut pointer, &name)?;
        pointer.as_file().sync_all()?;
        pointer.persist(self.root.join("CURRENT"))?;
        fs::File::open(&self.root)?.sync_all()?;
        *self
            .reader
            .0
            .write()
            .map_err(|_| anyhow::anyhow!("published reader lock poisoned"))? = Some(Active {
            name,
            checkpoint,
            searcher,
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "lexical_consumer_tests.rs"]
mod tests;
