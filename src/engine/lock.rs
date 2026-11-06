//! One run at a time per downloads folder. Two link runs racing each other
//! fail each other's pairs with "source changed", the last to save the cache
//! drops the other's hashes, and `cache cleanup-temp` can delete a temp link
//! another run is about to rename. Scan, link and cleanup-temp take an
//! exclusive lock on `<downloads>/.dejaview_lock` without waiting; a run that
//! cannot take it does nothing and exits with code 3 (ADR 0003).

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::logger;

const LOCK_FILE: &str = ".dejaview_lock";

/// Held for as long as the run lasts; the lock is released when it drops.
pub struct RunLock {
    _file: Option<File>,
}

/// The error a run returns when another run holds the lock.
#[derive(Debug)]
pub struct LockHeld {
    path: PathBuf,
}

impl fmt::Display for LockHeld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "another DejaView run holds the lock {}, so this run did nothing; it can run once that one ends",
            self.path.display()
        )
    }
}

impl std::error::Error for LockHeld {}

/// Take the lock, retrying for a moment before calling it held. A process that
/// forks shares its open files with the child until the child's exec closes
/// them, so a lock this process just released can stay held for that instant
/// when another thread is starting a process.
fn try_lock_briefly(file: &File) -> std::result::Result<(), TryLockError> {
    const ATTEMPTS: u32 = 10;
    for _ in 1..ATTEMPTS {
        match file.try_lock() {
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            other => return other,
        }
    }
    file.try_lock()
}

pub fn acquire(downloads: &Path) -> Result<RunLock> {
    let path = downloads.join(LOCK_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(anyhow!(
            "refusing to use the lock file {}: it is a symlink; delete it and run again",
            path.display()
        ));
    }
    // flock needs only a handle, not write access: open an existing lock file
    // read-only, so one made by root on an earlier run works for every user.
    let file = match File::open(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path),
        opened => opened,
    }
    .map_err(|err| {
        anyhow!(
            "cannot open the lock file {}: {err}; check that this user can read the downloads folder",
            path.display()
        )
    })?;
    match try_lock_briefly(&file) {
        Ok(()) => Ok(RunLock { _file: Some(file) }),
        Err(TryLockError::WouldBlock) => Err(LockHeld { path }.into()),
        // Some network filesystems cannot lock at all. Refusing every run there
        // would make DejaView unusable, so run unlocked and say so.
        Err(TryLockError::Error(err)) => {
            let message = format!(
                "cannot lock {} ({err}); running without protection against a second run at the same time",
                path.display()
            );
            logger::warn("lock", &message);
            eprintln!("warning: {message}");
            Ok(RunLock { _file: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_downloads(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dejaview_lock_{prefix}_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    fn hold(dir: &Path) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE))
            .expect("open lock");
        file.try_lock().expect("take lock");
        file
    }

    #[test]
    fn a_lock_released_a_moment_later_is_taken() {
        let dir = temp_downloads("brief");
        let held = hold(&dir);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(held);
        });
        assert!(acquire(&dir).is_ok(), "waits out a release in progress");
        releaser.join().expect("releaser");
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_lock_still_held_is_reported_as_held() {
        let dir = temp_downloads("held");
        let _held = hold(&dir);
        let err = acquire(&dir).err().expect("held lock refuses");
        assert!(err.is::<LockHeld>(), "{err}");
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_lock_file_this_user_cannot_write_still_locks() {
        // Root made it 0644 on an earlier run; a run as another user can still
        // read it, which is all a lock needs.
        let dir = temp_downloads("read_only");
        let path = dir.join(LOCK_FILE);
        fs::write(&path, b"").expect("create lock file");
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).expect("chmod 0444");
        assert!(acquire(&dir).is_ok());
        fs::remove_dir_all(&dir).expect("cleanup");
    }
}
