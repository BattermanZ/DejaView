use anyhow::{Result, anyhow};
use colored::*;
use std::path::{Path, PathBuf};

use crate::fsutil::safe_write_bytes;
use crate::human::format_size;
use crate::logger;
use crate::protocol::{
    EntryStatus, LinkMethod, LinkReportEntry, Mode, Outcome, RunReport, WarningSummary,
};

use super::linking::Applied;
use super::types::{Audience, LinkAction, LinkMode, RunMetrics, ScanRunOptions};

/// A line on stdout: for a person, or the report a script reads with
/// `--json`. With `--tui-events` stdout is the dashboard's event stream, so
/// nothing goes there. The engine denies printing anywhere else, so every
/// line it writes to stdout passes this check.
#[allow(clippy::print_stdout)]
pub(super) fn print_out(audience: Audience, line: impl std::fmt::Display) {
    if audience != Audience::Dashboard {
        println!("{line}");
    }
}

/// A line on stderr, which no audience parses: warnings and the paths
/// listed under them.
#[allow(clippy::print_stderr)]
pub(super) fn print_err(line: impl std::fmt::Display) {
    eprintln!("{line}");
}

/// A warning for a person on stderr: `⚠ message`.
pub(super) fn print_warning(message: impl std::fmt::Display) {
    print_err(format_args!("{} {message}", "⚠".yellow()));
}

/// The plan for a person: the folders once, then one row per pair with
/// paths below them, or that there is nothing to do.
pub(super) fn print_plan(
    audience: Audience,
    actions: &[LinkAction],
    downloads: &Path,
    media: &[PathBuf],
    bytes: u64,
) {
    let say = |line: String| print_out(audience, line);
    say(format!("Downloads: {}", downloads.display()));
    for (i, folder) in media.iter().enumerate() {
        let label = if i == 0 { "Media:" } else { "" };
        say(format!("{label:<10} {}", folder.display()));
    }
    if actions.is_empty() {
        say(format!("{} No duplicates found.", "✔".green()));
        return;
    }
    say(super::linking::action_table(actions, downloads, media).to_string());
    say(format!(
        "Found {} duplicate(s): {} could be freed.",
        actions.len(),
        format_size(bytes)
    ));
}

/// What the run looked at and what went wrong, for a person. A quiet run
/// prints only what went wrong.
pub(super) fn print_run_summary(
    files_seen: usize,
    metrics: &RunMetrics,
    symlinked: usize,
    warnings: &WarningSummary,
    audience: Audience,
) {
    if audience.is_chatty() {
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
            print_out(
                audience,
                format!("{} Considered {files_seen} file(s).", "ℹ".cyan()),
            );
        } else {
            print_out(
                audience,
                format!(
                    "{} Considered {files_seen} file(s); left out {}.",
                    "ℹ".cyan(),
                    left_out.join(", ")
                ),
            );
        }
        if symlinked > 0 {
            print_out(
                audience,
                format!(
                    "{} {symlinked} download(s) became symlinks: each points at its library file and breaks if Sonarr or Radarr upgrades, renames or deletes that file. `fallback_reason` in the report says why each is not a hardlink.",
                    "⚠".yellow()
                ),
            );
        }
    }

    let total = warnings.total();
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
        print_warning(format_args!(
            "{total} file(s) could not be read, hashed or linked ({counts}); the log names each."
        ));
    }
}

/// How a link run ended, in the one line a person or a cron mail reads.
pub(super) struct Verdict<'a> {
    pub(super) planned: usize,
    pub(super) applied: Applied,
    pub(super) failed: u64,
    /// Downloads that became symlinks: a quiet run says so too.
    pub(super) symlinked: usize,
    pub(super) cancelled: bool,
    pub(super) report: Option<&'a Path>,
}

impl Verdict<'_> {
    /// `Linked 7 of 8, 8.0 MiB freed, 1 failed: see <report>`.
    pub(super) fn line(&self) -> String {
        let applied = &self.applied;
        let mut parts = vec![format!(
            "Linked {} of {}, {} freed",
            applied.applied,
            self.planned,
            format_size(applied.freed_bytes)
        )];
        if self.symlinked > 0 {
            parts.push(format!("{} as symlinks", self.symlinked));
        }
        if applied.already_linked > 0 {
            parts.push(format!("{} already linked", applied.already_linked));
        }
        if applied.skipped > 0 {
            parts.push(format!("{} left alone", applied.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        let mut line = parts.join(", ");
        if self.cancelled {
            line.push_str("; stopped on request, the rest stay planned");
        }
        if (self.failed > 0 || applied.skipped > 0 || self.symlinked > 0)
            && let Some(report) = self.report
        {
            line.push_str(&format!(": see {}", report.display()));
        }
        line
    }

    /// Print the line, unless a quiet run linked nothing and nothing failed.
    pub(super) fn print(&self, audience: Audience) {
        let quiet = audience == Audience::Quiet;
        if self.planned == 0 || (quiet && self.applied.applied == 0 && self.failed == 0) {
            return;
        }
        let mark = if self.failed > 0 || self.cancelled {
            "⚠".yellow()
        } else {
            "✔".green()
        };
        print_out(audience, format_args!("{mark} {}", self.line()));
    }
}

/// Name the symlinks in downloads that point into the library and no longer
/// resolve, a few at a time; the report lists every one.
pub(super) fn print_dangling_symlinks(dangling: &[PathBuf]) {
    if dangling.is_empty() {
        return;
    }
    print_warning(format_args!(
        "{} symlink(s) in downloads point into the library and no longer resolve, so their torrents cannot seed. The library file was probably upgraded, renamed or deleted; re-download or remove them:",
        dangling.len()
    ));
    const SHOWN: usize = 10;
    for path in dangling.iter().take(SHOWN) {
        print_err(format_args!("    {}", path.display()));
    }
    if dangling.len() > SHOWN {
        print_err(format_args!(
            "    and {} more; `dangling_symlinks` in the --json report lists them all",
            dangling.len() - SHOWN
        ));
    }
}

/// Ask a person at a terminal before linking. Only `--confirm` asks, which
/// refuses a run that is not interactive and is never given with
/// `--tui-events`, so the prompt cannot reach the dashboard's stream.
#[allow(clippy::print_stdout)]
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
        print_warning(format_args!(
            "Failed to write link report at {}: {err}",
            path.display()
        ));
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
            status: EntryStatus::Planned,
            method: Some(
                if link_mode == LinkMode::SymlinkOnly
                    || action.src_snapshot.dev != action.tgt_snapshot.dev
                {
                    LinkMethod::Symlink
                } else {
                    LinkMethod::TryHardlink
                },
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
            Mode::Scan
        } else {
            Mode::Link
        },
        outcome: input.outcome,
        version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: crate::human::rfc3339_utc(input.started_at),
        finished_at: crate::human::rfc3339_utc(std::time::SystemTime::now()),
        downloads: input.downloads.display().to_string(),
        media: input
            .media
            .first()
            .map(|folder| folder.display().to_string())
            .unwrap_or_default(),
        media_folders: input
            .media
            .iter()
            .map(|folder| folder.display().to_string())
            .collect(),
        link_mode: options.link_mode,
        dry_run: options.dry_run,
        strict: options.strict,
        files_considered: input.files_seen,
        planned_actions: input.planned_actions,
        applied_actions: input.applied.applied,
        symlinked: symlinked_count(&input.entries),
        already_linked: input.applied.already_linked,
        already_linked_bytes: input.applied.already_linked_bytes,
        planned_bytes: input.planned_bytes,
        freed_bytes: input.applied.freed_bytes,
        total_saved_bytes: if options.dry_run {
            input.planned_bytes
        } else {
            input.applied.freed_bytes
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
        warnings: input.warnings,
        entries: input.entries,
    }
}

pub(super) struct RunReportInput<'a> {
    pub(super) outcome: Outcome,
    pub(super) started_at: std::time::SystemTime,
    pub(super) downloads: &'a Path,
    pub(super) media: &'a [PathBuf],
    pub(super) files_seen: usize,
    pub(super) planned_actions: usize,
    pub(super) planned_bytes: u64,
    pub(super) applied: Applied,
    pub(super) metrics: RunMetrics,
    pub(super) warnings: WarningSummary,
    pub(super) entries: Vec<LinkReportEntry>,
}

/// The error `--strict` turns this run's failures into, if any.
pub(super) fn strict_failure(
    strict_link_error: Option<anyhow::Error>,
    strict: bool,
    warnings: &WarningSummary,
) -> Option<anyhow::Error> {
    if strict_link_error.is_some() {
        return strict_link_error;
    }
    if strict && warnings.total() > 0 {
        return Some(anyhow!(
            "--strict stopped the run: {} file(s) could not be read, hashed or linked; the log names each. Make them readable, leave their folder out with `dejaview config set-exclude`, or run without --strict",
            warnings.total()
        ));
    }
    None
}
