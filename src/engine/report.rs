use anyhow::{anyhow, Result};
use colored::*;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::{
    LinkAction, LinkReportEntry, RunReport, RunWarnings, ScanRunOptions, WarningSummary,
};

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
    planned_actions: usize,
    applied_actions: usize,
    warnings: &RunWarnings,
) {
    println!("{} Files considered: {}", "ℹ".cyan(), files_seen);
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

pub(super) fn warning_total(warnings: &RunWarnings) -> u64 {
    warnings.walk_errors
        + warnings.metadata_errors
        + warnings.sample_hash_errors
        + warnings.full_hash_errors
        + warnings.link_errors
}

fn write_run_report(path: &Path, report: &RunReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(report)?;
    fs::write(path, json)?;
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
            status: "planned".to_string(),
            method: None,
            fallback_reason: None,
            error: None,
        })
        .collect()
}

pub(super) fn build_run_report(
    options: &ScanRunOptions<'_>,
    files_seen: usize,
    planned_actions: usize,
    applied_actions: usize,
    total_saved_space: u64,
    warnings: &RunWarnings,
    entries: Vec<LinkReportEntry>,
) -> RunReport {
    RunReport {
        mode: if options.dry_run {
            "scan".to_string()
        } else {
            "link".to_string()
        },
        dry_run: options.dry_run,
        strict: options.strict,
        files_considered: files_seen,
        planned_actions,
        applied_actions,
        total_saved_bytes: total_saved_space,
        warnings: WarningSummary {
            walk_errors: warnings.walk_errors,
            metadata_errors: warnings.metadata_errors,
            sample_hash_errors: warnings.sample_hash_errors,
            full_hash_errors: warnings.full_hash_errors,
            link_errors: warnings.link_errors,
        },
        entries,
    }
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
