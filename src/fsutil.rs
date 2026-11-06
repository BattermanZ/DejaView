use anyhow::{Result, anyhow};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `path` resolved and absolute, which must be a folder. An error names the
/// path, since the OS error alone ("No such file or directory") does not.
pub fn normalize_runtime_dir(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path).map_err(|err| {
        anyhow!(
            "cannot use {} as a folder: {err}; check the path, and that its drive is mounted",
            path.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(anyhow!(
            "{} is a file, not a folder; give the folder that holds it",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// A path typed by a person, with a leading `~` standing for `home`, as a
/// shell would read it. Only `~` and `~/...`; `~user` is left alone.
pub fn expand_home(input: &str, home: Option<&Path>) -> PathBuf {
    match (input.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(input),
    }
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
    use crate::test_common::{TempDir, unique_temp_path};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    fn a_folder_that_cannot_be_used_is_named_in_the_error() {
        let missing = unique_temp_path("missing").join("nope");
        let err = normalize_runtime_dir(&missing).expect_err("missing folder");
        let message = format!("{err:#}");
        assert!(
            message.contains(&missing.display().to_string()),
            "{message}"
        );
        assert!(message.contains("No such file or directory"), "{message}");
    }

    #[test]
    fn a_leading_tilde_is_the_home_folder() {
        let home = Path::new("/home/someone");
        assert_eq!(expand_home("~", Some(home)), PathBuf::from("/home/someone"));
        assert_eq!(
            expand_home("~/media/TV", Some(home)),
            PathBuf::from("/home/someone/media/TV")
        );
        assert_eq!(
            expand_home("~other/x", Some(home)),
            PathBuf::from("~other/x")
        );
        assert_eq!(
            expand_home("/srv/media", Some(home)),
            PathBuf::from("/srv/media")
        );
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn safe_write_bytes_rejects_symlink_target() {
        #[cfg(not(unix))]
        {
            return;
        }

        #[cfg(unix)]
        {
            let root = TempDir::new("symlink_target");
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
        }
    }
}
