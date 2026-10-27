//! Owner-only hook credential files; no policy cache or runtime state is persisted.
use anyhow::{Result, anyhow, ensure};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

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
fn open_private(path: &Path) -> Result<File> {
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
