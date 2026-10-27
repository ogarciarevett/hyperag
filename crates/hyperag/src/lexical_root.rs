use anyhow::Context;
use std::path::{Path, PathBuf};

pub(super) fn resolve(
    configured: Option<&Path>,
    home: Option<&Path>,
    database_id: &str,
) -> anyhow::Result<PathBuf> {
    if let Some(root) = configured {
        return Ok(root.to_path_buf());
    }
    let home = home.ok_or_else(|| anyhow::anyhow!("missing HOME for lexical index root"))?;
    anyhow::ensure!(home.is_absolute(), "invalid HOME path: {}", home.display());
    let home = home
        .canonicalize()
        .with_context(|| format!("invalid HOME path: {}", home.display()))?;
    let metadata = home
        .metadata()
        .with_context(|| format!("invalid HOME path: {}", home.display()))?;
    anyhow::ensure!(
        metadata.is_dir(),
        "invalid HOME directory: {}",
        home.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        anyhow::ensure!(
            metadata.uid() == crate::lexical_consumer::current_owner(),
            "invalid HOME owner: {}",
            home.display()
        );
    }
    anyhow::ensure!(
        database_id.len() == 36
            && database_id
                .bytes()
                .enumerate()
                .all(|(i, byte)| if [8, 13, 18, 23].contains(&i) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }),
        "invalid lexical database UUID"
    );
    Ok(home.join(format!(".hyperag-tantivy-{database_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "12345678-1234-1234-1234-123456789abc";

    #[test]
    fn default_uses_canonical_home_and_database_namespace() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let root = resolve(None, Some(home.path()), ID)?;
        assert_eq!(
            root,
            home.path()
                .canonicalize()?
                .join(format!(".hyperag-tantivy-{ID}"))
        );
        assert_ne!(root.parent(), Some(std::env::current_dir()?.as_path()));
        assert!(!root.exists());
        assert!(resolve(None, Some(home.path()), "../escape").is_err());
        Ok(())
    }

    #[test]
    fn invalid_home_names_its_path_and_override_needs_no_home() -> anyhow::Result<()> {
        assert!(resolve(None, None, ID).is_err());
        for path in [Path::new(""), Path::new("relative-home")] {
            assert!(
                resolve(None, Some(path), ID)
                    .unwrap_err()
                    .to_string()
                    .contains(&path.display().to_string())
            );
        }
        let scratch = tempfile::tempdir()?;
        for path in [
            scratch.path().join("missing-home"),
            scratch.path().join("file-home"),
        ] {
            if path.ends_with("file-home") {
                std::fs::write(&path, b"fixture")?;
            }
            assert!(
                resolve(None, Some(&path), ID)
                    .unwrap_err()
                    .to_string()
                    .contains(&path.display().to_string())
            );
        }
        assert_eq!(
            resolve(Some(Path::new("explicit-root")), None, ID)?,
            Path::new("explicit-root")
        );
        Ok(())
    }
}
