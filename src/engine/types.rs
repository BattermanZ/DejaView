use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::cancel::CancelToken;
use crate::protocol::TuiEvent;
use serde::{Deserialize, Serialize};

/// It lives with the report, which names it; the engine and the config use
/// it from here.
pub use crate::protocol::LinkMode;

/// One file a walk found, with what the hashes found out about it.
#[derive(Debug, Clone)]
pub(super) struct FileInfo {
    pub(super) path: PathBuf,
    /// The file as the walk saw it.
    pub(super) snapshot: FileSnapshot,
    /// Full BLAKE3, hex, once known.
    pub(super) hash: Option<String>,
    /// Quick hash of the first and last MiB, hex, for a file bigger than
    /// [`super::hashing::SAMPLE_BYTES`]; a smaller file's quick hash is its
    /// full hash and lives in `hash`.
    pub(super) sample: Option<String>,
    pub(super) in_downloads: bool,
}

/// A file as `lstat` describes it at one moment: what tells whether a path
/// is still the file a run saw, and whether two paths are one file. Every
/// comparison of files goes through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct FileSnapshot {
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
}

impl FileSnapshot {
    /// The snapshot of a file's metadata, from `symlink_metadata` for the
    /// file itself. An mtime before 1970 counts as 1970.
    pub(super) fn from_metadata(meta: &std::fs::Metadata) -> Self {
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

    /// Whether a download is still the file the plan saw: nothing about it
    /// changed, its ctime included.
    pub(super) fn matches_source(&self, current: &Self) -> bool {
        current == self
    }

    /// Whether a file is still the one seen, its ctime aside: a link to it,
    /// even one this run made, moves its ctime and nothing else.
    pub(super) fn matches_target(&self, current: &Self) -> bool {
        current.size == self.size
            && current.mtime_ns == self.mtime_ns
            && current.dev == self.dev
            && current.inode == self.inode
    }

    /// Whether `current` holds this file's contents by size and mtime alone:
    /// what a new hardlink is checked by, since mergerfs without `use_ino`
    /// gives one file a second inode number and reports it on another device.
    pub(super) fn matches_contents(&self, current: &Self) -> bool {
        current.size == self.size && current.mtime_ns == self.mtime_ns
    }
}

/// What the cache knows of one path, valid while the file's size, times,
/// device and inode are the ones recorded.
///
/// On disk it is one flat object, `{"size", "mtime_ns", "ctime_ns", "dev",
/// "inode", "hash", "sample"}`, through [`CacheRecord`]: serde's flatten
/// cannot carry the 128-bit times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CacheEntry {
    /// The file the hashes are of.
    pub(super) snapshot: FileSnapshot,
    /// Full BLAKE3, hex. Absent when only the quick hash is known. A version
    /// that required it reads such a cache as unreadable and starts empty,
    /// which costs a rehash but can never pair two different files.
    pub(super) hash: Option<String>,
    /// Quick hash, hex, for a file bigger than the sample: kept so a file
    /// whose quick hash matched no other is not read again next run.
    pub(super) sample: Option<String>,
}

/// A cache entry as the cache file holds it.
#[derive(Serialize, Deserialize)]
struct CacheRecord<'a> {
    size: u64,
    mtime_ns: u128,
    ctime_ns: i128,
    dev: u64,
    inode: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hash: Option<std::borrow::Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample: Option<std::borrow::Cow<'a, str>>,
}

impl Serialize for CacheEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let FileSnapshot {
            size,
            mtime_ns,
            ctime_ns,
            dev,
            inode,
        } = self.snapshot;
        CacheRecord {
            size,
            mtime_ns,
            ctime_ns,
            dev,
            inode,
            hash: self.hash.as_deref().map(std::borrow::Cow::Borrowed),
            sample: self.sample.as_deref().map(std::borrow::Cow::Borrowed),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CacheEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = CacheRecord::deserialize(deserializer)?;
        Ok(CacheEntry {
            snapshot: FileSnapshot {
                size: record.size,
                mtime_ns: record.mtime_ns,
                ctime_ns: record.ctime_ns,
                dev: record.dev,
                inode: record.inode,
            },
            hash: record.hash.map(std::borrow::Cow::into_owned),
            sample: record.sample.map(std::borrow::Cow::into_owned),
        })
    }
}

pub(super) type Cache = HashMap<String, CacheEntry>;

#[derive(Default, Clone)]
pub(super) struct RunMetrics {
    /// Files the walk found and kept, before hashing.
    pub(super) files_seen: usize,
    pub(super) skipped_by_min_age: u64,
    pub(super) skipped_by_min_size: u64,
    /// Files and folders left out by name; a folder counts once.
    pub(super) skipped_by_exclude: u64,
    /// Downloads left alone as a library file seen through a second mount.
    pub(super) skipped_as_library_file: u64,
    /// Symlinks in downloads that point into the library and no longer resolve.
    pub(super) dangling_symlinks: Vec<PathBuf>,
    /// Paths the walk could not read, each with whether it is in downloads:
    /// a folder, or a file whose metadata failed. The cache keeps entries
    /// under them, which this run could not see.
    pub(super) unreadable: Vec<(PathBuf, bool)>,
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
    /// Print nothing unless a link was made or something failed: a cron
    /// job's mail then means something happened.
    pub quiet: bool,
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

/// Who reads what a run prints, which decides what it prints. Worked out
/// once from the options, so no print site combines the flags itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Audience {
    /// A person at a terminal: the plan, a summary, warnings and a verdict.
    Person,
    /// `--quiet`: a person or a cron job, told only of links made and
    /// failures.
    Quiet,
    /// `--json`: a script reading one report on stdout.
    Script,
    /// `--tui-events`: the dashboard, reading events on stdout.
    Dashboard,
}

impl Audience {
    /// A person reads the terminal, quiet or not.
    pub(super) fn is_person(self) -> bool {
        matches!(self, Audience::Person | Audience::Quiet)
    }

    /// Everything a person may want is printed: the plan, the summary and
    /// warnings that come back every run.
    pub(super) fn is_chatty(self) -> bool {
        self == Audience::Person
    }
}

impl ScanRunOptions<'_> {
    pub(super) fn audience(&self) -> Audience {
        if self.tui_events {
            Audience::Dashboard
        } else if self.json_output {
            Audience::Script
        } else if self.quiet {
            Audience::Quiet
        } else {
            Audience::Person
        }
    }

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

/// Per-file progress events (a file found, a count) go at most once per this
/// interval. A walk of a hundred thousand files would otherwise send the TUI
/// one line per file, each flushed, faster than anyone can read them.
const PROGRESS_EVERY_MS: u64 = 100;
static PROGRESS_CLOCK: OnceLock<Instant> = OnceLock::new();
/// When the last progress event went, in milliseconds on PROGRESS_CLOCK;
/// u64::MAX before the first.
static LAST_PROGRESS_MS: AtomicU64 = AtomicU64::new(u64::MAX);

/// Whether a per-file progress event may go now. Workers on every thread ask;
/// at most one of them gets a yes per interval. Events that close a phase
/// (its last count, a phase change, a log line) are not paced.
pub(super) fn progress_due() -> bool {
    let now = PROGRESS_CLOCK
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis() as u64;
    let last = LAST_PROGRESS_MS.load(Ordering::Relaxed);
    if last != u64::MAX && now < last.saturating_add(PROGRESS_EVERY_MS) {
        return false;
    }
    LAST_PROGRESS_MS
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

pub(super) fn emit_tui_event(event: &TuiEvent) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, event)?;
    lock.write_all(b"\n")?;
    lock.flush()
}
