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

pub fn acquire(downloads: &Path) -> Result<RunLock> {
    let path = downloads.join(LOCK_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(anyhow!(
            "refusing to use the lock file {}: it is a symlink; delete it and run again",
            path.display()
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|err| anyhow!("cannot open the lock file {}: {err}", path.display()))?;
    match file.try_lock() {
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
