use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::cancel::CancelToken;
use crate::protocol::TuiEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkMode {
    #[default]
    Best,
    SymlinkOnly,
}

#[derive(Debug, Clone)]
pub(super) struct FileInfo {
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
    /// Full BLAKE3, hex, once known.
    pub(super) hash: Option<String>,
    /// Quick hash of the first and last MiB, hex, for a file bigger than
    /// [`super::hashing::SAMPLE_BYTES`]; a smaller file's quick hash is its
    /// full hash and lives in `hash`.
    pub(super) sample: Option<String>,
    pub(super) in_downloads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileSnapshot {
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
}

impl FileSnapshot {
    pub(super) fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        FileSnapshot {
            size: meta.len(),
            mtime_ns: meta
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            ctime_ns: (meta.ctime() as i128 * 1_000_000_000) + meta.ctime_nsec() as i128,
            dev: meta.dev(),
            inode: meta.ino(),
        }
    }

    /// Whether two paths are one file: one inode, or one file seen through
    /// two mounts. A bind mount or a mergerfs branch shows a library file on a
    /// second device, often under another inode number, but with the same
    /// size and timestamps; two real copies almost never share a ctime to the
    /// nanosecond.
    pub(super) fn is_same_physical_file(&self, other: &Self) -> bool {
        if self.dev == other.dev {
            self.inode == other.inode
        } else {
            self.size == other.size
                && self.mtime_ns == other.mtime_ns
                && self.ctime_ns == other.ctime_ns
        }
    }

    pub(super) fn matches_source(&self, current: &Self) -> bool {
        current == self
    }

    pub(super) fn matches_target(&self, current: &Self) -> bool {
        current.size == self.size
            && current.mtime_ns == self.mtime_ns
            && current.dev == self.dev
            && current.inode == self.inode
    }
}

/// What the cache knows of one path, valid while the file's size, times,
/// device and inode are the ones recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CacheEntry {
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
    /// Full BLAKE3, hex. Absent when only the quick hash is known. A version
    /// that required it reads such a cache as unreadable and starts empty,
    /// which costs a rehash but can never pair two different files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) hash: Option<String>,
    /// Quick hash, hex, for a file bigger than the sample: kept so a file
    /// whose quick hash matched no other is not read again next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sample: Option<String>,
}

pub(super) type Cache = HashMap<String, CacheEntry>;

#[derive(Default, Clone)]
pub(super) struct RunWarnings {
    pub(super) walk_errors: u64,
    pub(super) metadata_errors: u64,
    pub(super) sample_hash_errors: u64,
    pub(super) full_hash_errors: u64,
    pub(super) link_errors: u64,
}

#[derive(Default, Clone)]
pub(super) struct RunMetrics {
    pub(super) skipped_by_min_age: u64,
    pub(super) skipped_by_min_size: u64,
    /// Downloads left alone as a library file seen through a second mount.
    pub(super) skipped_as_library_file: u64,
    /// Symlinks in downloads that point into the library and no longer resolve.
    pub(super) dangling_symlinks: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub(super) struct LinkAction {
    pub(super) src: PathBuf,
    pub(super) tgt: PathBuf,
    pub(super) size: u64,
    pub(super) src_snapshot: FileSnapshot,
    pub(super) tgt_snapshot: FileSnapshot,
}

pub struct ScanRunOptions<'a> {
    pub dry_run: bool,
    pub strict: bool,
    pub report_path: Option<&'a Path>,
    pub json_output: bool,
    pub show_progress: bool,
    pub require_confirm: bool,
    pub tui_events: bool,
    pub link_mode: LinkMode,
    pub only_sources: &'a [PathBuf],
    /// Reviewed rows: when not empty, a link run links only these (download,
    /// library file) pairs, and only to that library file.
    pub only_pairs: &'a [(PathBuf, PathBuf)],
    pub cancel: CancelToken,
}

impl ScanRunOptions<'_> {
    /// Whether a planned action is one this run was asked to take.
    pub(super) fn selects(&self, action: &LinkAction) -> bool {
        (self.only_sources.is_empty() || self.only_sources.contains(&action.src))
            && (self.only_pairs.is_empty()
                || self
                    .only_pairs
                    .iter()
                    .any(|(src, tgt)| src == &action.src && tgt == &action.tgt))
    }
}

pub(super) fn emit_tui_event(event: &TuiEvent) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, event)?;
    lock.write_all(b"\n")?;
    lock.flush()
}
