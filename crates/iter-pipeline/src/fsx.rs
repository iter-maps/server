//! Atomic artifact writes: write to a temp sibling, then rename over the
//! target. A reader (or a crashed re-run) never sees a partial artifact.

use std::path::Path;

pub async fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);

    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_parent_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c.json");
        write_atomic(&target, b"hello").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        // The temp sibling is renamed away, never left behind.
        assert!(!dir.path().join("a/b/c.json.tmp").exists());
    }

    #[tokio::test]
    async fn overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.txt");
        write_atomic(&target, b"one").await.unwrap();
        write_atomic(&target, b"two").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
    }
}
