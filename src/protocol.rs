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
    // from_str_radix alone would take "+f" as 0x0f.
    if digits.len() % 2 != 0 || !digits.iter().all(u8::is_ascii_hexdigit) {
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

/// Files a run could not read, hash or link, by step. The engine counts
/// into it as the run goes and the report carries it as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct WarningSummary {
    pub walk_errors: u64,
    pub metadata_errors: u64,
    pub sample_hash_errors: u64,
    pub full_hash_errors: u64,
    pub link_errors: u64,
}

impl WarningSummary {
    /// Every failure, whatever the step: what `--strict` stops on.
    pub fn total(&self) -> u64 {
        self.walk_errors
            + self.metadata_errors
            + self.sample_hash_errors
            + self.full_hash_errors
            + self.link_errors
    }
}

/// What kind of run wrote a report: a scan changes nothing, a link run
/// replaces downloads.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Scan,
    Link,
}

/// Where a report row's pair stands.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryStatus {
    /// Still to do: the plan's row, not yet linked.
    #[default]
    Planned,
    /// Linked by a run.
    Applied,
    /// A link that failed; `error` says why.
    Failed,
    /// Left alone on purpose; `error` says why.
    Skipped,
    /// Found at link time to be one file already; nothing changed.
    AlreadyLinked,
}

/// How a row's download is, or will be, linked to its library file.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkMethod {
    /// Planned: a hardlink will be tried first, with a symlink to fall back
    /// on (ADR 0001).
    TryHardlink,
    Hardlink,
    Symlink,
}

/// A step of a run, as the event stream names it.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Walking both folders and grouping files by size.
    Collecting,
    /// Quick hashes of the first and last MiB.
    HashingSample,
    /// Full hashes of files whose quick hashes matched.
    HashingFull,
    /// Pairing each download with its library file.
    Planning,
    /// Replacing downloads with links.
    Applying,
}

impl Mode {
    /// The name the report gives it.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Scan => "scan",
            Mode::Link => "link",
        }
    }
}

impl LinkMethod {
    /// The name the report gives it.
    pub fn as_str(self) -> &'static str {
        match self {
            LinkMethod::TryHardlink => "try-hardlink",
            LinkMethod::Hardlink => "hardlink",
            LinkMethod::Symlink => "symlink",
        }
    }
}

impl Phase {
    /// Hashing, quick or full: the phases whose progress counts files read.
    pub fn is_hashing(self) -> bool {
        matches!(self, Phase::HashingSample | Phase::HashingFull)
    }
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
    pub status: EntryStatus,
    pub method: Option<LinkMethod>,
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
    /// Still to do: the plan's row, not yet linked.
    pub fn is_planned(&self) -> bool {
        self.status == EntryStatus::Planned
    }

    /// Linked by a run.
    pub fn is_applied(&self) -> bool {
        self.status == EntryStatus::Applied
    }

    /// A link that failed; `error` says why.
    pub fn is_failed(&self) -> bool {
        self.status == EntryStatus::Failed
    }

    /// Left alone on purpose; `error` says why.
    pub fn is_skipped(&self) -> bool {
        self.status == EntryStatus::Skipped
    }

    /// Found at link time to be one file already.
    pub fn is_already_linked(&self) -> bool {
        self.status == EntryStatus::AlreadyLinked
    }

    /// Where the row sorts: by download path, then library path, compared
    /// as paths, as the engine orders every plan.
    pub fn row_order(&self) -> (PathBuf, PathBuf) {
        let path = |text: &str, raw: Option<&str>| {
            path_from(text, raw).unwrap_or_else(|_| PathBuf::from(text))
        };
        (
            path(&self.source, self.source_raw.as_deref()),
            path(&self.target, self.target_raw.as_deref()),
        )
    }

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
        self.is_applied() && self.method == Some(LinkMethod::Symlink)
    }
}

/// How links are made: `best` hardlinks and falls back to a symlink,
/// `symlink-only` always symlinks. The config and the report both say it.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkMode {
    #[default]
    Best,
    SymlinkOnly,
}

/// How a run that built its report ended.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// It reached its end, whatever failed on the way, which `warnings`
    /// counts.
    #[default]
    Complete,
    /// Stopped on request; pairs not reached stay `planned`.
    Cancelled,
    /// `--strict` stopped it on a failure.
    Failed,
}

/// How a run ended. Fields added after the first release default when a
/// report written before them is read.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RunReport {
    /// `scan` or `link`.
    pub mode: Mode,
    /// `complete`, `cancelled` or `failed`; see [`Outcome`].
    #[serde(default)]
    pub outcome: Outcome,
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
    pub link_mode: LinkMode,
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

/// The counters a `count` event carries. A run sends the ones that moved;
/// each `None` leaves the dashboard's value as it was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Counts {
    pub files_seen: Option<usize>,
    pub groups_found: Option<usize>,
    pub planned_actions: Option<usize>,
    pub applied_actions: Option<usize>,
    pub hash_progress_done: Option<usize>,
    pub hash_progress_total: Option<usize>,
}

/// The version of the `--tui-events` stream. The dashboard refuses a child
/// that speaks another, such as a binary an upgrade replaced on disk while
/// the dashboard was open. Raise it whenever an event changes shape, a new
/// [`Phase`] or [`EntryStatus`] included: the dashboard cannot read a value
/// it does not know.
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
        phase: Phase,
    },
    WorkerActivity {
        phase: Phase,
        worker: usize,
        path: Option<String>,
    },
    File {
        phase: Option<Phase>,
        path: String,
    },
    Count(Counts),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    /// Compare `actual` with the checked-in golden file `name` under
    /// `tests/golden/`. With `DEJAVIEW_BLESS=1` the file is written instead,
    /// for a format change made on purpose: the diff of the golden file is
    /// then the change a script reading the report sees.
    fn assert_golden(name: &str, actual: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var_os("DEJAVIEW_BLESS").is_some() {
            std::fs::write(&path, actual).expect("write golden file");
            return;
        }
        let expected = std::fs::read_to_string(&path).expect("read golden file");
        assert!(
            expected == actual,
            "{name} no longer matches what DejaView writes. A script reading it would see this change; if it is meant, rerun with DEJAVIEW_BLESS=1 and add an `### After upgrading` changelog line.\n--- expected\n{expected}\n--- actual\n{actual}"
        );
    }

    fn golden_entry(
        name: &str,
        status: EntryStatus,
        method: Option<LinkMethod>,
    ) -> LinkReportEntry {
        LinkReportEntry {
            source: format!("/srv/downloads/{name}.mkv"),
            target: format!("/srv/media/Movies/{name}.mkv"),
            source_raw: None,
            target_raw: None,
            size: 4_194_304,
            status,
            method,
            fallback_reason: None,
            error: None,
        }
    }

    /// A link run's report with every field set and a row of every status:
    /// what a cron script parses.
    fn golden_report() -> RunReport {
        let planned = golden_entry(
            "Planned",
            EntryStatus::Planned,
            Some(LinkMethod::TryHardlink),
        );
        let applied = golden_entry("Applied", EntryStatus::Applied, Some(LinkMethod::Hardlink));
        let symlinked = LinkReportEntry {
            fallback_reason: Some("cross-device: no hardlink is possible".to_string()),
            ..golden_entry("Symlinked", EntryStatus::Applied, Some(LinkMethod::Symlink))
        };
        let already = LinkReportEntry {
            fallback_reason: Some("already this library file; nothing changed".to_string()),
            ..golden_entry(
                "Already",
                EntryStatus::AlreadyLinked,
                Some(LinkMethod::Hardlink),
            )
        };
        let skipped = LinkReportEntry {
            error: Some("left alone: its owner could no longer read it".to_string()),
            ..golden_entry("Skipped", EntryStatus::Skipped, None)
        };
        let failed = LinkReportEntry {
            source: "/srv/downloads/Bad\u{fffd}.mkv".to_string(),
            source_raw: Some("2f7372762f646f776e6c6f6164732f426164ff2e6d6b76".to_string()),
            error: Some("rename: Read-only file system (os error 30)".to_string()),
            ..golden_entry("Failed", EntryStatus::Failed, None)
        };
        RunReport {
            mode: Mode::Link,
            outcome: Outcome::Complete,
            version: "0.1.0".to_string(),
            started_at: "2026-11-06T03:17:00Z".to_string(),
            finished_at: "2026-11-06T03:17:42Z".to_string(),
            downloads: "/srv/downloads".to_string(),
            media: "/srv/media".to_string(),
            link_mode: LinkMode::Best,
            dry_run: false,
            strict: false,
            files_considered: 120,
            planned_actions: 6,
            applied_actions: 2,
            symlinked: 1,
            already_linked: 1,
            already_linked_bytes: 4_194_304,
            planned_bytes: 25_165_824,
            freed_bytes: 8_388_608,
            total_saved_bytes: 8_388_608,
            skipped_by_min_age: 3,
            skipped_by_min_size: 40,
            skipped_by_exclude: 2,
            skipped_as_library_file: 1,
            dangling_symlinks: vec!["/srv/downloads/Gone.mkv".to_string()],
            warnings: WarningSummary {
                walk_errors: 1,
                link_errors: 1,
                ..Default::default()
            },
            entries: vec![planned, applied, symlinked, already, skipped, failed],
        }
    }

    #[test]
    fn the_names_the_log_uses_are_the_names_the_report_uses() {
        for mode in [Mode::Scan, Mode::Link] {
            assert_eq!(serde_json::to_value(mode).expect("mode"), mode.as_str());
        }
        for method in [
            LinkMethod::TryHardlink,
            LinkMethod::Hardlink,
            LinkMethod::Symlink,
        ] {
            assert_eq!(
                serde_json::to_value(method).expect("method"),
                method.as_str()
            );
        }
    }

    #[test]
    fn the_report_json_is_what_scripts_parse() {
        let json = serde_json::to_string_pretty(&golden_report()).expect("serialise report");
        assert_golden("run_report.json", &json);
        // What a script wrote back is read the same.
        let read: RunReport = serde_json::from_str(&json).expect("parse report");
        assert_eq!(serde_json::to_string_pretty(&read).expect("again"), json);
    }

    #[test]
    fn the_event_stream_is_what_the_dashboard_parses() {
        let mut scan = golden_report();
        scan.mode = Mode::Scan;
        scan.dry_run = true;
        scan.entries.truncate(1);
        let events = vec![
            TuiEvent::Hello {
                protocol: PROTOCOL_VERSION,
                version: "0.1.0".to_string(),
            },
            TuiEvent::Phase {
                phase: Phase::Collecting,
            },
            TuiEvent::File {
                phase: Some(Phase::Collecting),
                path: "/srv/downloads/Planned.mkv".to_string(),
            },
            TuiEvent::Count(Counts {
                files_seen: Some(120),
                ..Counts::default()
            }),
            TuiEvent::Phase {
                phase: Phase::HashingSample,
            },
            TuiEvent::WorkerActivity {
                phase: Phase::HashingSample,
                worker: 3,
                path: Some("/srv/media/Movies/Planned.mkv".to_string()),
            },
            TuiEvent::Phase {
                phase: Phase::HashingFull,
            },
            TuiEvent::HashBytes {
                done: 1_048_576,
                total: 8_388_608,
            },
            TuiEvent::PlannedEntries {
                entries: scan.entries.clone(),
            },
            TuiEvent::Phase {
                phase: Phase::Planning,
            },
            TuiEvent::Phase {
                phase: Phase::Applying,
            },
            TuiEvent::File {
                phase: Some(Phase::Applying),
                path: "/srv/downloads/Planned.mkv".to_string(),
            },
            TuiEvent::Log {
                message: "Planning complete. 1 action(s) queued.".to_string(),
            },
            TuiEvent::Done {
                report: Box::new(scan.clone()),
            },
            TuiEvent::Error {
                message: "--strict stopped the run".to_string(),
                report: Some(Box::new(scan)),
            },
            TuiEvent::Error {
                message: "Downloads path not set".to_string(),
                report: None,
            },
            TuiEvent::Cancelled,
        ];
        let mut stream = String::new();
        for event in &events {
            let line = serde_json::to_string(event).expect("serialise event");
            let read: TuiEvent = serde_json::from_str(&line).expect("parse event");
            assert_eq!(serde_json::to_string(&read).expect("again"), line);
            stream.push_str(&line);
            stream.push('\n');
        }
        assert_golden("tui_events.ndjson", &stream);
    }

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
        assert!(path_from("/dl/a", Some("+f")).is_err());
    }
}
