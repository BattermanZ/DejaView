use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A path's bytes in hex when they are not valid UTF-8, for the `*_raw`
/// fields beside a path's string, which then stands in U+FFFD for each
/// byte that is not. None for a path whose string is exact.
pub fn raw_path_hex(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if std::str::from_utf8(bytes).is_ok() {
        return None;
    }
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The path a string field and its `*_raw` field name: the raw bytes when
/// there are any, else the string. Raw bytes that are not hex are an error,
/// never a fall back to the string, which names another path.
pub fn path_from(text: &str, raw: Option<&str>) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    let Some(raw) = raw else {
        return Ok(PathBuf::from(text));
    };
    let digits = raw.as_bytes();
    if digits.len() % 2 != 0 {
        return Err(format!("the raw bytes of {text} are not hex: {raw}"));
    }
    digits
        .chunks(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()
        .map(|bytes| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        .ok_or_else(|| format!("the raw bytes of {text} are not hex: {raw}"))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WarningSummary {
    pub walk_errors: u64,
    pub metadata_errors: u64,
    pub sample_hash_errors: u64,
    pub full_hash_errors: u64,
    pub link_errors: u64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LinkReportEntry {
    pub source: String,
    pub target: String,
    /// The download's and the library file's bytes in hex, present only for
    /// a path that is not valid UTF-8, where `source` or `target` stands in
    /// U+FFFD for the bytes that are not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_raw: Option<String>,
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
    /// As in [`LinkReportEntry`]: the bytes of a path that is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_raw: Option<String>,
}

impl PlannedPair {
    /// The pair a report row names, bytes and all.
    pub fn of(entry: &LinkReportEntry) -> Self {
        PlannedPair {
            source: entry.source.clone(),
            target: entry.target.clone(),
            source_raw: entry.source_raw.clone(),
            target_raw: entry.target_raw.clone(),
        }
    }
}

impl LinkReportEntry {
    /// Whether two rows are about the same pair, bytes and all: two paths
    /// that differ only in bytes that are not UTF-8 share their strings.
    pub fn same_pair(&self, other: &Self) -> bool {
        self.source == other.source
            && self.target == other.target
            && self.source_raw == other.source_raw
            && self.target_raw == other.target_raw
    }

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
    /// Bytes read in full so far, of all the full pass will read: what a
    /// 60 GB file's progress is measured in while the file count stands
    /// still.
    HashBytes {
        done: u64,
        total: u64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn a_path_that_is_not_utf8_comes_back_whole_from_its_raw_bytes() {
        let path = PathBuf::from(OsStr::from_bytes(b"/dl/a\xff.mkv"));
        let raw = raw_path_hex(&path).expect("not UTF-8");
        assert_eq!(raw, "2f646c2f61ff2e6d6b76");
        let text = path.display().to_string();
        assert_eq!(path_from(&text, Some(&raw)), Ok(path));
        // A UTF-8 path needs none, and its string is the path.
        assert_eq!(raw_path_hex(Path::new("/dl/é.mkv")), None);
        assert_eq!(path_from("/dl/é.mkv", None), Ok(PathBuf::from("/dl/é.mkv")));
        assert!(path_from("/dl/a", Some("2f6")).is_err());
        assert!(path_from("/dl/a", Some("zz")).is_err());
    }
}
