//! Explicit local updates; query integration must supply a fresh source fence.
use crate::lexical_generation::{self, Generation};
use crate::lexical_snapshot::{Checkpoint, SourceSnapshot};
use anyhow::Context;
use sqlx::PgPool;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tantivy::Searcher;

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
    pending: Option<Prepared>,
    _owner: fs::File,
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

pub(super) fn current_owner() -> u32 {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn geteuid() -> std::os::raw::c_uint;
        }
        // SAFETY: geteuid takes no arguments and returns the effective Unix UID.
        unsafe { geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn check_private(path: &Path, directory: bool, _owner: u32) -> anyhow::Result<()> {
    check_private_metadata(&fs::symlink_metadata(path)?, path, directory, _owner)
}

fn check_private_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    directory: bool,
    _owner: u32,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "unsafe consumer path type: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = if directory { 0o700 } else { 0o600 };
        anyhow::ensure!(
            metadata.uid() == _owner && metadata.mode() & 0o777 == mode,
            "unsafe consumer path owner/mode: {}",
            path.display()
        );
    }
    Ok(())
}

fn check_ancestors(parent: &Path, _owner: u32) -> anyhow::Result<()> {
    // Validate top-down so a trusted parent protects each subsequent pathname.
    for path in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir(),
            "unsafe consumer ancestor type: {}",
            path.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            anyhow::ensure!(
                (metadata.uid() == 0 || metadata.uid() == _owner)
                    && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
                "unsafe consumer ancestor owner/mode: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn current(root: &Path, owner: u32) -> anyhow::Result<Option<Active>> {
    let pointer = root.join("CURRENT");
    if let Err(error) = fs::symlink_metadata(&pointer) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    check_private(&pointer, false, owner)?;
    let mut file = fs::File::open(&pointer)?;
    check_private_metadata(&file.metadata()?, &pointer, false, owner)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let Ok(name) = serde_json::from_slice::<String>(&bytes) else {
        return Ok(None);
    };
    if !valid_name(&name) {
        return Ok(None);
    }
    let generation = root.join(&name);
    if let Err(error) = fs::symlink_metadata(&generation) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error.into());
    }
    check_private(&generation, true, owner)?;
    for entry in fs::read_dir(&generation)? {
        let metadata = fs::symlink_metadata(entry?.path())?;
        anyhow::ensure!(metadata.is_file(), "nonregular recovered index entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Index files may be readable outside the private directory, never writable.
            anyhow::ensure!(
                metadata.uid() == owner && metadata.mode() & 0o022 == 0,
                "unsafe recovered index file owner/mode"
            );
        }
    }
    Ok(lexical_generation::recover_complete(&generation, None)
        .ok()
        .flatten()
        .map(|(checkpoint, searcher)| Active {
            name,
            checkpoint,
            searcher,
        }))
}

impl Consumer {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        Self::open_with(root, |parent| fs::File::open(parent)?.sync_all())
    }

    fn open_with(
        root: &Path,
        sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> anyhow::Result<Self> {
        let uid = current_owner();
        let parent = root
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = parent
            .canonicalize()
            .with_context(|| format!("cannot resolve consumer ancestor: {}", parent.display()))?;
        check_ancestors(&parent, uid)?;
        // Retain the resolved path: later operations must not follow a retargeted alias.
        let root = parent.join(
            root.file_name()
                .ok_or_else(|| anyhow::anyhow!("consumer root must have a basename"))?,
        );
        let mut directory = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        let created = match directory.create(&root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        };
        check_private(&root, true, uid)?;
        if created {
            if let Err(error) = sync_parent(&parent) {
                fs::remove_dir(&root)?; // Retry must create and sync the new root again.
                return Err(error.into());
            }
        }
        let lock = root.join(".consumer.lock");
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&lock) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        check_private(&lock, false, uid)?;
        let owner = fs::OpenOptions::new().write(true).open(&lock)?;
        check_private_metadata(&owner.metadata()?, &lock, false, uid)?;
        owner.try_lock()?;
        // Missing/malformed/incompatible state rebuilds; unsafe ownership or modes refuse open.
        let reader = ReaderHandle(Arc::new(RwLock::new(current(&root, uid)?)));
        Ok(Self {
            root,
            reader,
            pending: None,
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
                )
                .await
                {
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

    /// Advance one source page; cancellation discards its snapshot without replacing the reader.
    pub async fn step(&mut self, pool: &PgPool) -> anyhow::Result<bool> {
        let mut pending = match self.pending.take() {
            Some(pending) => pending,
            None => match self.prepare(pool).await? {
                Some(pending) => pending,
                None => return Ok(false),
            },
        };
        if pending.generation.advance().await? {
            self.pending = Some(pending);
            return Ok(true);
        }
        let searcher = pending.generation.publish()?;
        drop(pending.generation);
        self.activate(pending.name, pending.checkpoint, searcher)?;
        Ok(false)
    }

    pub async fn update(&mut self, pool: &PgPool) -> anyhow::Result<Checkpoint> {
        while self.step(pool).await? {}
        Ok(self
            .reader
            .0
            .read()
            .map_err(|_| anyhow::anyhow!("published reader lock poisoned"))?
            .as_ref()
            .expect("completed update requires an active generation")
            .checkpoint
            .clone())
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
