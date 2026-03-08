use anyhow::{Result, anyhow};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn normalize_runtime_dir(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_dir() {
        return Err(anyhow!("path is not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

pub fn safe_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(anyhow!(
            "refusing to overwrite symlink target: {}",
            path.display()
        ));
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;

    let tmp_path = parent.join(format!(
        ".dejaview_write.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dejaview_fsutil_{prefix}_{}_{}",
            std::process::id(),
            ts
        ))
    }

    #[test]
    fn safe_write_bytes_rejects_symlink_target() {
        #[cfg(not(unix))]
        {
            return;
        }

        #[cfg(unix)]
        {
            let root = unique_temp_dir("symlink_target");
            fs::create_dir_all(&root).expect("create root");
            let victim = root.join("victim.json");
            let link = root.join("report.json");
            fs::write(&victim, b"keep").expect("write victim");
            symlink(&victim, &link).expect("create symlink");

            let err = safe_write_bytes(&link, b"replace").expect_err("symlink write should fail");
            assert!(
                err.to_string()
                    .contains("refusing to overwrite symlink target")
            );
            assert_eq!(fs::read(&victim).expect("read victim"), b"keep");

            fs::remove_dir_all(&root).expect("cleanup temp dir");
        }
    }
}
