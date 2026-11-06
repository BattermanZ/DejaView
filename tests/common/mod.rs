//! Helpers every test shares: the integration tests take this module with
//! `mod common;`, and the unit tests through `#[path]` in `src/main.rs`, so
//! there is one copy of each.
#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A fresh folder under the system's temp folder, removed with everything in
/// it when the guard is dropped, so a test that fails partway leaves nothing
/// behind. It reads as the folder's path wherever a `&Path` is taken.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A new, empty folder named after `prefix`, the process and a counter,
    /// so tests running at once never share one.
    pub fn new(prefix: &str) -> Self {
        Self::at(unique_temp_path(prefix))
    }

    /// The same under `parent`, such as `/dev/shm` for a folder on another
    /// filesystem than the system's temp folder.
    pub fn new_in(parent: &Path, prefix: &str) -> Self {
        let name = unique_temp_path(prefix);
        Self::at(parent.join(name.file_name().expect("a file name")))
    }

    fn at(path: PathBuf) -> Self {
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|err| panic!("create temp dir {}: {err}", path.display()));
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A test may have left a folder unreadable or unwritable; give the
        // owner its permissions back first, or nothing under it goes.
        restore_permissions(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn restore_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
    }
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            restore_permissions(&entry.path());
        }
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// A path under the system's temp folder that nothing uses yet, for a test
/// that needs a path that does not exist. Prefer [`TempDir`], which cleans up.
pub fn unique_temp_path(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dejaview_{prefix}_{}_{nanos}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}
