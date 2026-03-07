use anyhow::{Result, anyhow};
use colored::*;
use std::path::{Path, PathBuf};

use crate::fsutil::safe_write_bytes;
use crate::protocol::{LinkReportEntry, RunReport, WarningSummary};

use super::types::{LinkAction, RunMetrics, RunWarnings, ScanRunOptions};

fn format_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < units.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    format!("{:.2} {}", size, units[unit_index])
}

pub(super) fn print_run_summary(
    files_seen: usize,
    skipped_by_min_age: u64,
    planned_actions: usize,
    applied_actions: usize,
    warnings: &RunWarnings,
) {
    println!("{} Files considered: {}", "ℹ".cyan(), files_seen);
    println!("{} Skipped by min-age: {}", "ℹ".cyan(), skipped_by_min_age);
    println!("{} Planned actions: {}", "ℹ".cyan(), planned_actions);
    if applied_actions > 0 {
        println!("{} Applied actions: {}", "ℹ".cyan(), applied_actions);
    }

    let total = warning_total(warnings);
    if total > 0 {
        println!(
            "{} Warnings: walk={} metadata={} sample-hash={} full-hash={} link={}",
            "⚠".yellow(),
            warnings.walk_errors,
            warnings.metadata_errors,
            warnings.sample_hash_errors,
            warnings.full_hash_errors,
            warnings.link_errors
        );
    }
}

pub(super) fn print_space_summary(actions_empty: bool, total_saved_space: u64) {
    if actions_empty {
        println!("{} No actions needed.", "✔".green());
    } else {
        println!(
            "{} Potential space saved: {}",
            "💾".cyan(),
            format_size(total_saved_space)
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
    Ok(())
}

pub(super) fn try_write_run_report(path: &Path, report: &RunReport) {
    if let Err(err) = write_run_report(path, report) {
        eprintln!(
            "{} Failed to write link report at {}: {}",
            "⚠".yellow(),
            path.display(),
            err
        );
    }
}

pub(super) fn resolve_report_path(
    downloads: &Path,
    options: &ScanRunOptions<'_>,
) -> Option<PathBuf> {
    options.report_path.map(PathBuf::from).or_else(|| {
        if options.dry_run {
            None
        } else {
            Some(downloads.join(".dejaview_link_report.json"))
        }
    })
}

pub(super) fn build_report_entries(actions: &[LinkAction]) -> Vec<LinkReportEntry> {
    actions
        .iter()
        .map(|action| LinkReportEntry {
            source: action.src.display().to_string(),
            target: action.tgt.display().to_string(),
            size: action.size,
            status: "planned".to_string(),
            method: Some(
                if action.src_snapshot.dev == action.tgt_snapshot.dev {
                    "hardlink"
                } else {
                    "symlink"
                }
                .to_string(),
            ),
            fallback_reason: if action.src_snapshot.dev == action.tgt_snapshot.dev {
                None
            } else {
                Some("cross-device; hardlink not possible".to_string())
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
        dry_run: options.dry_run,
        strict: options.strict,
        files_considered: input.files_seen,
        planned_actions: input.planned_actions,
        applied_actions: input.applied_actions,
        total_saved_bytes: input.total_saved_space,
        skipped_by_min_age: input.metrics.skipped_by_min_age,
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

pub(super) struct RunReportInput {
    pub(super) files_seen: usize,
    pub(super) planned_actions: usize,
    pub(super) applied_actions: usize,
    pub(super) total_saved_space: u64,
    pub(super) metrics: RunMetrics,
    pub(super) warnings: RunWarnings,
    pub(super) entries: Vec<LinkReportEntry>,
}

pub(super) fn maybe_return_strict_errors(
    strict_link_error: Option<anyhow::Error>,
    strict: bool,
    warnings: &RunWarnings,
) -> Result<()> {
    if let Some(err) = strict_link_error {
        return Err(err);
    }
    if strict && warning_total(warnings) > 0 {
        return Err(anyhow!("strict mode failed due to scan/hash warnings"));
    }
    Ok(())
}
