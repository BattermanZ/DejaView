use anyhow::{anyhow, Result};
use colored::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod cache;
mod collect;
mod hashing;
mod linking;
mod report;
mod types;

pub use types::ScanRunOptions;

use cache::{load_cache, save_cache};
use collect::{collect_run_files, group_files_by_size};
use hashing::hash_groups_and_update_cache;
use linking::{apply_actions, plan_actions_and_table};
use report::{
    build_report_entries, build_run_report, maybe_confirm_apply, maybe_return_strict_errors,
    print_run_summary, print_space_summary, resolve_report_path, try_write_run_report,
    RunReportInput,
};
use types::{RunMetrics, RunWarnings};

use crate::config::{cache_path, Config};

pub fn cache_info(config: &Config) -> Result<(std::path::PathBuf, usize)> {
    cache::cache_info(config)
}

pub fn clear_cache(config: &Config) -> Result<()> {
    cache::clear_cache(config)
}

pub fn run_scan_with_options(config: &Config, options: ScanRunOptions<'_>) -> Result<()> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    let media = config
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("Media path not set"))?;
    let min_age = std::time::Duration::from_secs(config.min_age.unwrap_or(300));

    let mut warnings = RunWarnings::default();
    let mut metrics = RunMetrics::default();
    let files = collect_run_files(
        downloads,
        media,
        min_age,
        options.strict,
        options.show_progress,
        &mut warnings,
        &mut metrics,
    )?;
    let files_seen = files.len();

    let cache_path = cache_path(config)?;
    let cache = Arc::new(Mutex::new(load_cache(&cache_path)));
    let inode_hash: Arc<Mutex<HashMap<(u64, u64), String>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut groups = group_files_by_size(files);
    hash_groups_and_update_cache(
        &mut groups,
        &cache,
        &inode_hash,
        options.strict,
        options.show_progress,
        &mut warnings,
    )?;

    if let Ok(c) = Arc::try_unwrap(cache) {
        save_cache(&cache_path, &c.into_inner().unwrap())?;
    }

    let (table, actions, total_saved_space) = plan_actions_and_table(&groups);
    let planned_actions = actions.len();

    if !options.json_output {
        println!("\n{} Found duplicates:", "▶".cyan());
        println!("{}", table);
        print_space_summary(actions.is_empty(), total_saved_space);
    }
    let out_path = resolve_report_path(downloads, &options);
    let mut report_entries = build_report_entries(&actions);

    let (applied_actions, strict_link_error) = if options.dry_run {
        (0usize, None)
    } else {
        if options.json_output && options.require_confirm {
            return Err(anyhow!(
                "--confirm cannot be combined with --json; automation runs must stay non-interactive"
            ));
        }
        maybe_confirm_apply(options.require_confirm, planned_actions)?;
        apply_actions(
            &actions,
            media,
            options.strict,
            options.json_output,
            &mut warnings,
            &mut report_entries,
        )
    };

    if !options.json_output {
        print_run_summary(
            files_seen,
            metrics.skipped_by_min_age,
            planned_actions,
            applied_actions,
            &warnings,
        );
    }

    let report = build_run_report(
        &options,
        RunReportInput {
            files_seen,
            planned_actions,
            applied_actions,
            total_saved_space,
            metrics: metrics.clone(),
            warnings: warnings.clone(),
            entries: report_entries,
        },
    );
    if let Some(path) = out_path.as_deref() {
        try_write_run_report(path, &report);
    }
    if options.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if options.dry_run {
        println!("{} Dry-run; no changes.", "ℹ".yellow());
    }

    maybe_return_strict_errors(strict_link_error, options.strict, &warnings)?;

    if !options.json_output {
        println!("{} Done.", "✔".green());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
