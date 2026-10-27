//! Private hook credentials and bounded, short-lived project eligibility metadata.
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const CACHE_MS: u128 = 5_000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScopeCache {
    scope: String,
    eligible: bool,
    expires_ms: u128,
}

pub(crate) fn owner() -> u32 {
    crate::lexical_consumer::current_owner()
}
pub(crate) fn ancestors(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "hook path must be absolute");
    for part in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let metadata = fs::symlink_metadata(part)?;
        ensure!(
            metadata.is_dir()
                && (metadata.uid() == 0 || metadata.uid() == owner())
                && (metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0),
            "unsafe hook path ancestry"
        );
    }
    Ok(())
}
pub(crate) fn private(path: &Path, directory: bool) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    let expected_mode = if directory { 0o700 } else { 0o600 };
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    ensure!(
        metadata.uid() == owner() && metadata.mode() & 0o777 == expected_mode && expected_type,
        "unsafe hook file owner, mode or type"
    );
    Ok(metadata)
}
pub(crate) fn mkdir(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    private(path, true)?;
    Ok(())
}
pub(crate) fn open_private(path: &Path) -> Result<File> {
    let before = private(path, false)?;
    let file = File::open(path)?;
    let opened = file.metadata()?;
    let after = private(path, false)?;
    ensure!(
        (before.dev(), before.ino()) == (opened.dev(), opened.ino())
            && (after.dev(), after.ino()) == (opened.dev(), opened.ino()),
        "hook file changed while opening"
    );
    Ok(file)
}
pub(crate) fn lock(path: &Path) -> Result<File> {
    if let Err(error) = fs::symlink_metadata(path) {
        ensure!(
            error.kind() == std::io::ErrorKind::NotFound,
            "hook lock unavailable"
        );
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
    }
    let before = private(path, false)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let opened = file.metadata()?;
    let after = private(path, false)?;
    ensure!(
        (before.dev(), before.ino()) == (opened.dev(), opened.ino())
            && (after.dev(), after.ino()) == (opened.dev(), opened.ino()),
        "hook lock changed while opening"
    );
    Ok(file)
}
pub(crate) fn json_file<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<Option<T>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => (),
    }
    let file = match open_private(path) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(4_097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4_096, "hook metadata too large");
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| anyhow!("invalid hook metadata"))
}
pub(crate) fn save<T: Serialize>(root: &Path, path: &Path, value: &T) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        private(path, false)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|_| anyhow!("hook metadata save failed"))?;
    Ok(())
}
pub(crate) fn key(base: &str, token: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(base.trim_end_matches('/').as_bytes());
    hash.update([0]);
    hash.update(token.as_bytes());
    hex::encode(&hash.finalize()[..16])
}
pub(crate) fn root(base: &str, token: &str) -> Result<PathBuf> {
    let home = crate::hook_setup::home()?.canonicalize()?;
    ancestors(&home)?;
    ensure!(
        fs::metadata(&home)?.uid() == owner(),
        "unsafe hook home owner"
    );
    let parent = home.join(".hyperag-hooks");
    mkdir(&parent)?;
    let root = parent.join(key(base, token));
    mkdir(&root)?;
    Ok(root)
}
fn cache_path(root: &Path, scope: &str) -> PathBuf {
    root.join(format!(
        "scope-{}.json",
        hex::encode(&Sha256::digest(scope.as_bytes())[..8])
    ))
}
pub(crate) fn now_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}
pub(crate) fn cached_scope(base: &str, token: &str, scope: &str) -> Result<Option<bool>> {
    let root = root(base, token)?;
    let access = lock(&root.join("scopes.lock"))?;
    // Atomic cache replacement is legitimate. Read under the writer's lock, or
    // use the authoritative endpoint without waiting for an in-progress update.
    if access.try_lock().is_err() {
        return Ok(None);
    }
    let value: Option<ScopeCache> = json_file(&cache_path(&root, scope))?;
    let now = now_ms()?;
    Ok(value
        .filter(|value| {
            value.scope == scope && value.expires_ms > now && value.expires_ms <= now + CACHE_MS
        })
        .map(|value| value.eligible))
}

pub(crate) fn cache_scope(base: &str, token: &str, scope: &str, eligible: bool) -> Result<()> {
    ensure!(scope.len() <= 1_024, "hook project scope too long");
    let root = root(base, token)?;
    let path = cache_path(&root, scope);
    let admission = lock(&root.join("scopes.lock"))?;
    // A contended cache is optional: use the fresh permission result without waiting.
    if admission.try_lock().is_err() {
        return Ok(());
    }
    let mut cached = 0;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("scope-") || !name.ends_with(".json") {
            continue;
        }
        let value: Option<ScopeCache> = json_file(&entry.path())?;
        if value.is_some_and(|value| value.expires_ms <= now_ms().unwrap_or(0)) {
            match fs::remove_file(entry.path()) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        } else {
            cached += 1;
        }
    }
    if cached >= 64 && !path.exists() {
        return Ok(());
    }
    save(
        &root,
        &path,
        &ScopeCache {
            scope: scope.into(),
            eligible,
            expires_ms: now_ms()? + CACHE_MS,
        },
    )
}
pub(crate) fn invalidate(root: &Path, scope: &str) -> Result<()> {
    let path = cache_path(root, scope);
    match private(&path, false) {
        Ok(_) => (),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn invalidate_scope(base: &str, token: &str, scope: &str) -> Result<()> {
    invalidate(&root(base, token)?, scope)
}
pub(crate) fn read_token_file(path: &Path) -> Result<String> {
    ancestors(
        path.parent()
            .ok_or_else(|| anyhow!("invalid hook token file"))?,
    )?;
    let mut bytes = Vec::new();
    open_private(path)?.take(65_537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "hook credential file too large");
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid hook credential file"))?;
    value["token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing hook file token"))
}
