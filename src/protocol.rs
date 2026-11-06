use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WarningSummary {
    pub walk_errors: u64,
    pub metadata_errors: u64,
    pub sample_hash_errors: u64,
    pub full_hash_errors: u64,
    pub link_errors: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LinkReportEntry {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub size: u64,
    pub status: String,
    pub method: Option<String>,
    pub fallback_reason: Option<String>,
    pub error: Option<String>,
}

/// One row a person reviewed: link this download to this library file and
/// nothing else. The TUI hands a list of them to `link --plan`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PlannedPair {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RunReport {
    pub mode: String,
    pub dry_run: bool,
    pub strict: bool,
    pub files_considered: usize,
    pub planned_actions: usize,
    pub applied_actions: usize,
    pub total_saved_bytes: u64,
    pub skipped_by_min_age: u64,
    /// Files under the size floor, empty files included. Missing in reports
    /// written before the floor existed.
    #[serde(default)]
    pub skipped_by_min_size: u64,
    /// Downloads left alone because they are a library file seen through a
    /// second mount (a bind mount or a mergerfs branch). Non-zero means the
    /// downloads folder reaches into the library.
    #[serde(default)]
    pub skipped_as_library_file: u64,
    /// Symlinks in downloads that point into the library and no longer
    /// resolve: their torrents cannot seed.
    #[serde(default)]
    pub dangling_symlinks: Vec<String>,
    pub warnings: WarningSummary,
    pub entries: Vec<LinkReportEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiEvent {
    Phase {
        phase: String,
    },
    WorkerActivity {
        phase: String,
        worker: usize,
        path: Option<String>,
    },
    File {
        phase: Option<String>,
        path: String,
    },
    Count {
        files_seen: Option<usize>,
        groups_found: Option<usize>,
        planned_actions: Option<usize>,
        applied_actions: Option<usize>,
        hash_progress_done: Option<usize>,
        hash_progress_total: Option<usize>,
    },
    PlannedEntries {
        entries: Vec<LinkReportEntry>,
    },
    Log {
        message: String,
    },
    Done {
        report: RunReport,
    },
    Error {
        message: String,
    },
    /// The run stopped on request before changing anything.
    Cancelled,
}

impl RunReport {
    pub fn warning_total(&self) -> u64 {
        self.warnings.walk_errors
            + self.warnings.metadata_errors
            + self.warnings.sample_hash_errors
            + self.warnings.full_hash_errors
            + self.warnings.link_errors
    }
}
