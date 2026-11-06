use anyhow::{Result, anyhow};
use colored::*;
use std::collections::HashSet;

mod cache;
mod collect;
mod hashing;
mod linking;
mod report;
mod types;

pub use types::LinkMode;
pub use types::ScanRunOptions;

use crate::protocol::TuiEvent;
use cache::{cache_key, load_cache, prune_cache, save_cache};
use collect::{CollectOptions, collect_run_files, group_files_by_size};
use hashing::{HashOptions, hash_groups_and_update_cache};
use linking::{apply_actions, plan_actions_and_table};
use report::{
    RunReportInput, build_report_entries, build_run_report, maybe_confirm_apply,
    maybe_return_strict_errors, print_run_summary, print_space_summary, resolve_report_path,
    try_write_run_report,
};
use types::{RunMetrics, RunWarnings};

use crate::config::{Config, cache_path};
use crate::logger;

pub fn cache_info(config: &Config) -> Result<(std::path::PathBuf, usize)> {
    cache::cache_info(config)
}

pub fn clear_cache(config: &Config) -> Result<()> {
    cache::clear_cache(config)
}

pub fn audit_temp_artifacts(config: &Config) -> Result<Vec<std::path::PathBuf>> {
    cache::audit_temp_artifacts(config)
}

pub fn cleanup_temp_artifacts(config: &Config) -> Result<Vec<std::path::PathBuf>> {
    cache::cleanup_temp_artifacts(config)
}

pub fn run_scan_with_options(config: &Config, options: ScanRunOptions<'_>) -> Result<()> {
    if options.json_output && options.tui_events {
        return Err(anyhow!("--json cannot be combined with --tui-events"));
    }

    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    let media = config
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("Media path not set"))?;
    let min_age = std::time::Duration::from_secs(config.min_age.unwrap_or(0));
    let machine_output = options.json_output || options.tui_events;
    logger::info(
        "run",
        format!(
            "starting mode={} downloads={} media={} dry_run={} strict={}",
            if options.dry_run { "scan" } else { "link" },
            downloads.display(),
            media.display(),
            options.dry_run,
            options.strict
        ),
    );

    let mut warnings = RunWarnings::default();
    let mut metrics = RunMetrics::default();
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: "collecting".to_string(),
        });
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: "Collecting candidate files".to_string(),
        });
    }
    let files = collect_run_files(
        downloads,
        media,
        min_age,
        CollectOptions {
            strict: options.strict,
            show_progress: options.show_progress,
            tui_events: options.tui_events,
        },
        &mut warnings,
        &mut metrics,
    )?;
    let files_seen = files.len();
    logger::info("run", format!("collected {} candidate files", files_seen));
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Count {
            files_seen: Some(files_seen),
            groups_found: None,
            planned_actions: None,
            applied_actions: None,
            hash_progress_done: None,
            hash_progress_total: None,
        });
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: "hashing_sample".to_string(),
        });
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: "Hashing files to identify duplicate candidates".to_string(),
        });
    }

    let cache_path = cache_path(config)?;
    logger::info("run", format!("using cache path {}", cache_path.display()));
    let mut cache = load_cache(&cache_path);

    let seen_paths: HashSet<String> = files.iter().map(|file| cache_key(&file.path)).collect();
    let mut groups = group_files_by_size(files);
    hash_groups_and_update_cache(
        &mut groups,
        &mut cache,
        &HashOptions {
            strict: options.strict,
            show_progress: options.show_progress,
            tui_events: options.tui_events,
            link_mode: options.link_mode,
            only_sources: options.only_sources,
        },
        &mut warnings,
    )?;
    if warnings.walk_errors == 0 && warnings.metadata_errors == 0 {
        let pruned = prune_cache(&mut cache, &seen_paths);
        if pruned > 0 {
            logger::info(
                "cache",
                format!("pruned {pruned} entries for paths no longer present"),
            );
        }
    } else {
        logger::info(
            "cache",
            "walk was incomplete; keeping entries for paths not seen this run",
        );
    }
    save_cache(&cache_path, &cache)?;

    let (table, actions, _total_saved_space) = plan_actions_and_table(&groups);
    let actions = if options.only_sources.is_empty() {
        actions
    } else {
        actions
            .into_iter()
            .filter(|action| options.only_sources.iter().any(|path| path == &action.src))
            .collect::<Vec<_>>()
    };
    let total_saved_space = actions.iter().map(|action| action.size).sum();
    let planned_actions = actions.len();
    logger::info(
        "run",
        format!(
            "planned {} action(s), potential_saved_bytes={}",
            planned_actions, total_saved_space
        ),
    );
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: "planning".to_string(),
        });
        let _ = types::emit_tui_event(&TuiEvent::Count {
            files_seen: Some(files_seen),
            groups_found: Some(groups.len()),
            planned_actions: Some(planned_actions),
            applied_actions: Some(0),
            hash_progress_done: None,
            hash_progress_total: None,
        });
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: format!("Planning complete. {planned_actions} action(s) queued."),
        });
    }

    if !machine_output {
        println!("\n{} Found duplicates:", "▶".cyan());
        println!("{}", table);
        print_space_summary(actions.is_empty(), total_saved_space);
    }
    let out_path = resolve_report_path(downloads, &options);
    let mut report_entries = build_report_entries(&actions, options.link_mode);

    let (applied_actions, strict_link_error) = if options.dry_run {
        (0usize, None)
    } else {
        if options.json_output && options.require_confirm {
            return Err(anyhow!(
                "--confirm cannot be combined with --json; automation runs must stay non-interactive"
            ));
        }
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Phase {
                phase: "applying".to_string(),
            });
            let _ = types::emit_tui_event(&TuiEvent::Log {
                message: "Applying filesystem changes".to_string(),
            });
        }
        maybe_confirm_apply(options.require_confirm, planned_actions)?;
        logger::info("run", "starting apply phase");
        apply_actions(
            &actions,
            media,
            options.strict,
            options.json_output,
            options.tui_events,
            options.link_mode,
            &mut warnings,
            &mut report_entries,
        )
    };

    if !machine_output {
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
        logger::info("run", format!("writing report to {}", path.display()));
        try_write_run_report(path, &report);
    }

    if let Err(err) = maybe_return_strict_errors(strict_link_error, options.strict, &warnings) {
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Error {
                message: err.to_string(),
            });
        }
        return Err(err);
    }

    let finished_mode = report.mode.clone();
    let finished_planned = report.planned_actions;
    let finished_applied = report.applied_actions;
    let finished_warnings = report.warning_total();

    if options.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Done { report });
    } else if options.dry_run {
        println!("{} Dry-run; no changes.", "ℹ".yellow());
    }

    if !machine_output {
        println!("{} Done.", "✔".green());
    }
    logger::info(
        "run",
        format!(
            "finished mode={} planned={} applied={} warnings={}",
            finished_mode, finished_planned, finished_applied, finished_warnings
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests;
