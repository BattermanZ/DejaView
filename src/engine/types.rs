use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct FileInfo {
    pub(super) path: PathBuf,
    pub(super) size: u64,
    pub(super) mtime_secs: u64,
    pub(super) dev: u64,
    pub(super) inode: u64,
    pub(super) hash: Option<String>,
    pub(super) in_downloads: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CacheEntry {
    pub(super) size: u64,
    pub(super) mtime: u64,
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

#[derive(Serialize)]
pub(super) struct WarningSummary {
    pub(super) walk_errors: u64,
    pub(super) metadata_errors: u64,
    pub(super) sample_hash_errors: u64,
    pub(super) full_hash_errors: u64,
    pub(super) link_errors: u64,
}

#[derive(Serialize)]
pub(super) struct LinkReportEntry {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) status: String,
    pub(super) method: Option<String>,
    pub(super) fallback_reason: Option<String>,
    pub(super) error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct RunReport {
    pub(super) mode: String,
    pub(super) dry_run: bool,
    pub(super) strict: bool,
    pub(super) files_considered: usize,
    pub(super) planned_actions: usize,
    pub(super) applied_actions: usize,
    pub(super) total_saved_bytes: u64,
    pub(super) skipped_by_min_age: u64,
    pub(super) warnings: WarningSummary,
    pub(super) entries: Vec<LinkReportEntry>,
}

pub struct ScanRunOptions<'a> {
    pub dry_run: bool,
    pub strict: bool,
    pub report_path: Option<&'a Path>,
    pub json_output: bool,
    pub show_progress: bool,
    pub require_confirm: bool,
}
