use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::protocol::TuiEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub(super) struct FileInfo {
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
    pub(super) hash: Option<String>,
    pub(super) in_downloads: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CacheEntry {
    pub(super) size: u64,
    pub(super) mtime_ns: u128,
    pub(super) ctime_ns: i128,
    pub(super) dev: u64,
    pub(super) inode: u64,
    pub(super) hash: String,
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
}

#[derive(Clone)]
pub(super) struct LinkAction {
    pub(super) src: PathBuf,
    pub(super) tgt: PathBuf,
    pub(super) size: u64,
}

pub struct ScanRunOptions<'a> {
    pub dry_run: bool,
    pub strict: bool,
    pub report_path: Option<&'a Path>,
    pub json_output: bool,
    pub show_progress: bool,
    pub require_confirm: bool,
    pub tui_events: bool,
}

pub(super) fn emit_tui_event(event: &TuiEvent) -> io::Result<()> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, event)?;
    lock.write_all(b"\n")?;
    lock.flush()
}
