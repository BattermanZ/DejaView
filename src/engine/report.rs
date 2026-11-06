use anyhow::{Result, anyhow};
use colored::*;
use std::path::{Path, PathBuf};

use crate::fsutil::safe_write_bytes;
use crate::logger;
use crate::protocol::{LinkReportEntry, RunReport, WarningSummary};

use super::types::{LinkAction, LinkMode, RunMetrics, RunWarnings, ScanRunOptions};

/// A size for a person, in binary units: `8.0 MiB`.
pub(super) fn format_size(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < units.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    if unit_index == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", units[unit_index])
    }
}

/// The plan for a person: the two folders once, then one row per pair with
/// paths below them, or that there is nothing to do.
pub(super) fn print_plan(actions: &[LinkAction], downloads: &Path, media: &Path, bytes: u64) {
    println!("Downloads: {}", downloads.display());
    println!("Media:     {}", media.display());
    if actions.is_empty() {
        println!("{} No duplicates found.", "✔".green());
        return;
    }
    println!(
        "{}",
        super::linking::action_table(actions, downloads, media)
    );
    println!(
        "Found {} duplicate(s): {} could be freed.",
        actions.len(),
        format_size(bytes)
    );
}

/// What the run looked at and what went wrong, for a person. A quiet run
/// prints only what went wrong.
pub(super) fn print_run_summary(
    files_seen: usize,
    metrics: &RunMetrics,
    symlinked: usize,
    warnings: &RunWarnings,
    quiet: bool,
) {
    if !quiet {
        let mut left_out = Vec::new();
        if metrics.skipped_by_min_size > 0 {
            left_out.push(format!(
                "{} under the size floor",
                metrics.skipped_by_min_size
            ));
        }
        if metrics.skipped_by_min_age > 0 {
            left_out.push(format!(
                "{} younger than the minimum age",
                metrics.skipped_by_min_age
            ));
        }
        if metrics.skipped_by_exclude > 0 {
            left_out.push(format!(
                "{} by the exclude list (the log names each)",
                metrics.skipped_by_exclude
            ));
        }
        if left_out.is_empty() {
            println!("{} Considered {files_seen} file(s).", "ℹ".cyan());
        } else {
            println!(
                "{} Considered {files_seen} file(s); left out {}.",
                "ℹ".cyan(),
                left_out.join(", ")
            );
        }
        if symlinked > 0 {
            println!(
                "{} {symlinked} download(s) became symlinks: each points at its library file and breaks if Sonarr or Radarr upgrades, renames or deletes that file. `fallback_reason` in the report says why each is not a hardlink.",
                "⚠".yellow()
            );
        }
    }

    let total = warning_total(warnings);
    if total > 0 {
        let counts = [
            ("walk", warnings.walk_errors),
            ("metadata", warnings.metadata_errors),
            ("quick hash", warnings.sample_hash_errors),
            ("full hash", warnings.full_hash_errors),
            ("link", warnings.link_errors),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(what, count)| format!("{what} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
        eprintln!(
            "{} {total} file(s) could not be read, hashed or linked ({counts}); the log names each.",
            "⚠".yellow()
        );
    }
}

/// How a link run ended, in the one line a person or a cron mail reads.
pub(super) struct Verdict<'a> {
    pub(super) planned: usize,
    pub(super) applied: usize,
    pub(super) already_linked: usize,
    pub(super) skipped: usize,
    pub(super) failed: u64,
    /// Downloads that became symlinks: a quiet run says so too.
    pub(super) symlinked: usize,
    pub(super) freed_bytes: u64,
    pub(super) cancelled: bool,
    pub(super) report: Option<&'a Path>,
}

impl Verdict<'_> {
    /// `Linked 7 of 8, 8.0 MiB freed, 1 failed: see <report>`.
    pub(super) fn line(&self) -> String {
        let mut parts = vec![format!(
            "Linked {} of {}, {} freed",
            self.applied,
            self.planned,
            format_size(self.freed_bytes)
        )];
        if self.symlinked > 0 {
            parts.push(format!("{} as symlinks", self.symlinked));
        }
        if self.already_linked > 0 {
            parts.push(format!("{} already linked", self.already_linked));
        }
        if self.skipped > 0 {
            parts.push(format!("{} left alone", self.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        let mut line = parts.join(", ");
        if self.cancelled {
            line.push_str("; stopped on request, the rest stay planned");
        }
        if (self.failed > 0 || self.skipped > 0 || self.symlinked > 0)
            && let Some(report) = self.report
        {
            line.push_str(&format!(": see {}", report.display()));
        }
        line
    }

    /// Print the line, unless a quiet run linked nothing and nothing failed.
    pub(super) fn print(&self, quiet: bool) {
        if self.planned == 0 || (quiet && self.applied == 0 && self.failed == 0) {
            return;
        }
        let mark = if self.failed > 0 || self.cancelled {
            "⚠".yellow()
        } else {
            "✔".green()
        };
        println!("{mark} {}", self.line());
    }
}

/// Name the symlinks in downloads that point into the library and no longer
/// resolve, a few at a time; the report lists every one.
pub(super) fn print_dangling_symlinks(dangling: &[PathBuf]) {
    if dangling.is_empty() {
        return;
    }
    eprintln!(
        "{} {} symlink(s) in downloads point into the library and no longer resolve, so their torrents cannot seed. The library file was probably upgraded, renamed or deleted; re-download or remove them:",
        "⚠".yellow(),
        dangling.len()
    );
    const SHOWN: usize = 10;
    for path in dangling.iter().take(SHOWN) {
        eprintln!("    {}", path.display());
    }
    if dangling.len() > SHOWN {
        eprintln!(
            "    and {} more; `dangling_symlinks` in the --json report lists them all",
            dangling.len() - SHOWN
        );
    }
}

pub(super) fn maybe_confirm_apply(require_confirm: bool, planned_actions: usize) -> Result<()> {
    if !require_confirm || planned_actions == 0 {
        return Ok(());
    }

    use std::io::{self, BufRead, IsTerminal, Write};
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(
            "--confirm requires an interactive terminal; rerun without --confirm for non-interactive runs"
        ));
    }

    print!("Apply {planned_actions} planned action(s)? [y/N]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    let mut stdin = io::stdin().lock();
    stdin.read_line(&mut input)?;
    let answer = input.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        Ok(())
    } else {
        Err(anyhow!("operation cancelled by user"))
    }
}

/// Downloads this run turned into symlinks, which depend on their library file
/// staying where it is (ADR 0001).
pub(super) fn symlinked_count(entries: &[LinkReportEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.is_applied_symlink())
        .count()
}

pub(super) fn warning_total(warnings: &RunWarnings) -> u64 {
    warnings.walk_errors
        + warnings.metadata_errors
        + warnings.sample_hash_errors
        + warnings.full_hash_errors
        + warnings.link_errors
}

fn write_run_report(path: &Path, report: &RunReport) -> Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    safe_write_bytes(path, &json)?;
    logger::info("report", format!("wrote run report to {}", path.display()));
    Ok(())
}

pub(super) fn try_write_run_report(path: &Path, report: &RunReport) {
    if let Err(err) = write_run_report(path, report) {
        logger::error(
            "report",
            format!("failed to write run report {}: {}", path.display(), err),
        );
        eprintln!(
            "{} Failed to write link report at {}: {}",
            "⚠".yellow(),
            path.display(),
            err
        );
    }
}

/// Where this run's report goes: the `--report` path whenever one is given,
/// else, for a link run that planned something, the one fixed report in
/// downloads. A run that planned nothing leaves that report as it was, so
/// the record of the last run that did something survives (ADR 0003).
pub(super) fn resolve_report_path(
    downloads: &Path,
    options: &ScanRunOptions<'_>,
    planned_actions: usize,
) -> Option<PathBuf> {
    options.report_path.map(PathBuf::from).or_else(|| {
        if options.dry_run || planned_actions == 0 {
            None
        } else {
            Some(downloads.join(".dejaview_link_report.json"))
        }
    })
}

/// What the planned actions would free, each download file counted once
/// however many of its paths are planned: cross-seeded downloads are
/// hardlinks of each other, and their bytes come back only once.
pub(super) fn planned_bytes(actions: &[LinkAction]) -> u64 {
    let mut seen = std::collections::HashSet::new();
    actions
        .iter()
        .filter(|action| seen.insert((action.src_snapshot.dev, action.src_snapshot.inode)))
        .map(|action| action.size)
        .sum()
}

/// `time` in UTC as RFC 3339, to the second: `2026-11-06T03:17:00Z`.
pub(super) fn rfc3339_utc(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    // Days since 1970-01-01 to a civil date, by the well-known
    // civil-from-days algorithm; every value here is non-negative.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub(super) fn build_report_entries(
    actions: &[LinkAction],
    link_mode: LinkMode,
) -> Vec<LinkReportEntry> {
    actions
        .iter()
        .map(|action| LinkReportEntry {
            source: action.src.display().to_string(),
            target: action.tgt.display().to_string(),
            source_raw: crate::protocol::raw_path_hex(&action.src),
            target_raw: crate::protocol::raw_path_hex(&action.tgt),
            size: action.size,
            status: "planned".to_string(),
            method: Some(
                if link_mode == LinkMode::SymlinkOnly {
                    "symlink"
                } else if action.src_snapshot.dev == action.tgt_snapshot.dev {
                    "try-hardlink"
                } else {
                    "symlink"
                }
                .to_string(),
            ),
            fallback_reason: if link_mode == LinkMode::SymlinkOnly {
                Some("symlink-only mode enabled".to_string())
            } else if action.src_snapshot.dev == action.tgt_snapshot.dev {
                Some("tries hardlink first; may fall back to symlink".to_string())
            } else {
                Some(super::linking::CROSS_DEVICE_REASON.to_string())
            },
            error: None,
        })
        .collect()
}

pub(super) fn build_run_report(options: &ScanRunOptions<'_>, input: RunReportInput) -> RunReport {
    RunReport {
        mode: if options.dry_run {
            "scan".to_string()
        } else {
            "link".to_string()
        },
        outcome: input.outcome.as_str().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: rfc3339_utc(input.started_at),
        finished_at: rfc3339_utc(std::time::SystemTime::now()),
        downloads: input.downloads.display().to_string(),
        media: input.media.display().to_string(),
        link_mode: match options.link_mode {
            LinkMode::Best => "best",
            LinkMode::SymlinkOnly => "symlink-only",
        }
        .to_string(),
        dry_run: options.dry_run,
        strict: options.strict,
        files_considered: input.files_seen,
        planned_actions: input.planned_actions,
        applied_actions: input.applied_actions,
        symlinked: symlinked_count(&input.entries),
        already_linked: input.already_linked,
        already_linked_bytes: input.already_linked_bytes,
        planned_bytes: input.planned_bytes,
        freed_bytes: input.freed_bytes,
        total_saved_bytes: if options.dry_run {
            input.planned_bytes
        } else {
            input.freed_bytes
        },
        skipped_by_min_age: input.metrics.skipped_by_min_age,
        skipped_by_min_size: input.metrics.skipped_by_min_size,
        skipped_by_exclude: input.metrics.skipped_by_exclude,
        skipped_as_library_file: input.metrics.skipped_as_library_file,
        dangling_symlinks: input
            .metrics
            .dangling_symlinks
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        warnings: WarningSummary {
            walk_errors: input.warnings.walk_errors,
            metadata_errors: input.warnings.metadata_errors,
            sample_hash_errors: input.warnings.sample_hash_errors,
            full_hash_errors: input.warnings.full_hash_errors,
            link_errors: input.warnings.link_errors,
        },
        entries: input.entries,
    }
}

/// How a run that built its report ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// It reached its end, whatever failed on the way.
    Complete,
    /// Stopped on request between pairs.
    Cancelled,
    /// `--strict` stopped it on a failure.
    Failed,
}

impl Outcome {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Outcome::Complete => "complete",
            Outcome::Cancelled => "cancelled",
            Outcome::Failed => "failed",
        }
    }
}

pub(super) struct RunReportInput<'a> {
    pub(super) outcome: Outcome,
    pub(super) started_at: std::time::SystemTime,
    pub(super) downloads: &'a Path,
    pub(super) media: &'a Path,
    pub(super) files_seen: usize,
    pub(super) planned_actions: usize,
    pub(super) applied_actions: usize,
    pub(super) already_linked: usize,
    pub(super) already_linked_bytes: u64,
    pub(super) planned_bytes: u64,
    pub(super) freed_bytes: u64,
    pub(super) metrics: RunMetrics,
    pub(super) warnings: RunWarnings,
    pub(super) entries: Vec<LinkReportEntry>,
}

/// The error `--strict` turns this run's failures into, if any.
pub(super) fn strict_failure(
    strict_link_error: Option<anyhow::Error>,
    strict: bool,
    warnings: &RunWarnings,
) -> Option<anyhow::Error> {
    if strict_link_error.is_some() {
        return strict_link_error;
    }
    if strict && warning_total(warnings) > 0 {
        return Some(anyhow!(
            "--strict stopped the run: {} file(s) could not be read, hashed or linked; the log names each. Make them readable, leave their folder out with `dejaview config set-exclude`, or run without --strict",
            warning_total(warnings)
        ));
    }
    None
}
