use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

impl LinkReportEntry {
    /// This run turned the download into a symlink, which breaks if its
    /// library file is upgraded, renamed or deleted (ADR 0001).
    pub fn is_applied_symlink(&self) -> bool {
        self.status == "applied" && self.method.as_deref() == Some("symlink")
    }
}

/// How a run ended. Fields added after the first release default when a
/// report written before them is read.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RunReport {
    pub mode: String,
    /// `complete` (the run reached its end, whatever failed on the way, which
    /// `warnings` counts), `cancelled` (stopped on request; pairs not reached
    /// stay `planned`) or `failed` (stopped by `--strict` on a failure).
    #[serde(default)]
    pub outcome: String,
    /// The DejaView version that wrote the report.
    #[serde(default)]
    pub version: String,
    /// When the run started and ended, in UTC, as RFC 3339
    /// (`2026-11-06T03:17:00Z`).
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
    /// The downloads and media folders the run used.
    #[serde(default)]
    pub downloads: String,
    #[serde(default)]
    pub media: String,
    /// `best` (hardlink, else symlink) or `symlink-only`.
    #[serde(default)]
    pub link_mode: String,
    pub dry_run: bool,
    pub strict: bool,
    pub files_considered: usize,
    pub planned_actions: usize,
    /// Pairs this run replaced with a link. Pairs found to be one file
    /// already are counted in `already_linked`, not here.
    pub applied_actions: usize,
    /// Applied pairs whose download became a symlink, which breaks if its
    /// library file is upgraded, renamed or deleted.
    #[serde(default)]
    pub symlinked: usize,
    /// Pairs found at link time to be one file already; nothing changed.
    #[serde(default)]
    pub already_linked: usize,
    /// The bytes those pairs hold.
    #[serde(default)]
    pub already_linked_bytes: u64,
    /// What the planned pairs would free, each download file counted once
    /// however many of its paths are planned (cross-seeded downloads are
    /// hardlinks of each other).
    #[serde(default)]
    pub planned_bytes: u64,
    /// What this run freed: a download's bytes come back only when its last
    /// path is replaced. 0 for a scan.
    #[serde(default)]
    pub freed_bytes: u64,
    /// `planned_bytes` for a scan, `freed_bytes` for a link run.
    pub total_saved_bytes: u64,
    pub skipped_by_min_age: u64,
    /// Files under the size floor, empty files included. Missing in reports
    /// written before the floor existed.
    #[serde(default)]
    pub skipped_by_min_size: u64,
    /// Files and folders left out because their name is on the exclude list.
    /// A folder counts once, whatever it holds, since it is never read.
    #[serde(default)]
    pub skipped_by_exclude: u64,
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

/// The version of the `--tui-events` stream. The dashboard refuses a child
/// that speaks another, such as a binary an upgrade replaced on disk while
/// the dashboard was open. Raise it whenever an event changes shape.
pub const PROTOCOL_VERSION: u32 = 1;

/// One line of the `--tui-events` stream. It opens with `hello` and ends with
/// exactly one `done`, `error` or `cancelled`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TuiEvent {
    /// The first event: which stream this is and which DejaView sends it.
    Hello {
        protocol: u32,
        version: String,
    },
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
        report: Box<RunReport>,
    },
    /// The run stopped on an error. `report` is there when the run got far
    /// enough to build one, as a link `--strict` stopped partway does: its
    /// entries say which pairs were linked before the failure.
    Error {
        message: String,
        #[serde(default)]
        report: Option<Box<RunReport>>,
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
