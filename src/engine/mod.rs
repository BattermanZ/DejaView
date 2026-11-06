use anyhow::{Result, anyhow};
use colored::*;
use std::collections::HashSet;

mod cache;
mod cancel;
mod collect;
mod hashing;
mod linking;
mod lock;
mod report;
mod types;

pub use cancel::{CancelToken, Cancelled};
pub use lock::LockHeld;
pub use types::LinkMode;
pub use types::ScanRunOptions;

use crate::protocol::TuiEvent;
use cache::{cache_key, load_cache, prune_cache, save_cache};
use collect::{CollectOptions, collect_run_files, group_files_by_size};
use hashing::{HashOptions, hash_groups_and_update_cache};
use linking::{ApplyOptions, ApplyOutcome, apply_actions, plan_actions_and_table};
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
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    let _lock = lock::acquire(downloads)?;
    cache::cleanup_temp_artifacts(config)
}

/// Refuse downloads and media that reach each other through a second mount,
/// which a comparison of their paths cannot see. Walks the directories of both.
pub fn ensure_roots_not_mounted_inside(
    downloads: &std::path::Path,
    media: &std::path::Path,
) -> Result<()> {
    let downloads = collect::RootId::of("downloads", downloads)?;
    let media = collect::RootId::of("media", media)?;
    collect::ensure_root_not_reached(&downloads, &media)?;
    collect::ensure_root_not_reached(&media, &downloads)
}

/// What one real hardlink from the library into downloads did.
pub enum HardlinkProbe {
    /// The library holds no file to try with.
    NoFile,
    Works {
        file: std::path::PathBuf,
        /// Why the probe link could not be removed again, if it could not.
        leftover: Option<String>,
    },
    Fails {
        file: std::path::PathBuf,
        /// What a person can act on, from the OS error.
        cause: &'static str,
        error: String,
    },
}

/// Hardlink the first library file found into downloads under a temp name, then
/// remove it. This changes nothing in the library but that file's ctime, so
/// its cache entry is read once more on the next scan.
pub fn probe_hardlink(downloads: &std::path::Path, media: &std::path::Path) -> HardlinkProbe {
    use std::time::{SystemTime, UNIX_EPOCH};
    let file = walkdir::WalkDir::new(media)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file()
                && !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".dejaview_"))
        })
        .map(walkdir::DirEntry::into_path);
    let Some(file) = file else {
        return HardlinkProbe::NoFile;
    };
    let probe = downloads.join(format!(
        ".dejaview_validate_link.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    match std::fs::hard_link(&file, &probe) {
        Ok(()) => {
            let leftover = std::fs::remove_file(&probe).err().map(|err| {
                format!(
                    "the test link {} could not be removed ({err}); `dejaview cache cleanup-temp` removes it",
                    probe.display()
                )
            });
            HardlinkProbe::Works { file, leftover }
        }
        Err(err) => {
            let error = err.to_string();
            let cause = linking::hardlink_failure_cause(&anyhow::Error::new(err))
                .unwrap_or("the filesystem refused the hardlink");
            HardlinkProbe::Fails { file, cause, error }
        }
    }
}

pub fn run_scan_with_options(config: &Config, options: ScanRunOptions<'_>) -> Result<()> {
    let tui_events = options.tui_events;
    let result = run(config, options);
    if let Err(err) = &result
        && err.is::<Cancelled>()
    {
        logger::info("run", "cancelled by user before any change was made");
        if tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Cancelled);
        }
    }
    result
}

fn run(config: &Config, options: ScanRunOptions<'_>) -> Result<()> {
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
            "starting mode={} downloads={} media={} dry_run={} strict={} min_size={}",
            if options.dry_run { "scan" } else { "link" },
            downloads.display(),
            media.display(),
            options.dry_run,
            options.strict,
            config.effective_min_size()
        ),
    );

    // Held until the run returns, cache save and report included.
    let _lock = lock::acquire(downloads)?;
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
            min_size: config.effective_min_size(),
            strict: options.strict,
            show_progress: options.show_progress,
            tui_events: options.tui_events,
            cancel: &options.cancel,
        },
        &mut warnings,
        &mut metrics,
    )?;
    let files_seen = files.len();
    logger::info("run", format!("collected {} candidate files", files_seen));
    for path in &metrics.dangling_symlinks {
        logger::warn(
            "scan",
            format!(
                "{} points into the library and no longer resolves",
                path.display()
            ),
        );
    }
    if options.tui_events && !metrics.dangling_symlinks.is_empty() {
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: format!(
                "{} symlink(s) in downloads point into the library and no longer resolve, such as {}. The report lists them.",
                metrics.dangling_symlinks.len(),
                metrics.dangling_symlinks[0].display()
            ),
        });
    }
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
    if !options.only_sources.is_empty() {
        // Linking selected rows: only the size groups holding those sources can
        // produce their actions, so leave every other group unread.
        groups.retain(|group| {
            group
                .iter()
                .any(|file| file.in_downloads && options.only_sources.contains(&file.path))
        });
    }
    let hash_result = hash_groups_and_update_cache(
        &mut groups,
        &mut cache,
        &HashOptions {
            strict: options.strict,
            show_progress: options.show_progress,
            tui_events: options.tui_events,
            link_mode: options.link_mode,
            selection: &options,
            cancel: &options.cancel,
        },
        &mut warnings,
    );
    if hash_result.is_ok() && warnings.walk_errors == 0 && warnings.metadata_errors == 0 {
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
    // Hashes finished before a cancel or a strict failure are valid: keep them.
    save_cache(&cache_path, &cache)?;
    hash_result?;

    let linking::Plan {
        table,
        actions,
        seen_through_second_mount,
    } = plan_actions_and_table(&groups);
    report_second_mount_skips(&seen_through_second_mount, &options, &mut metrics);
    let actions = actions
        .into_iter()
        .filter(|action| options.selects(action))
        .collect::<Vec<_>>();
    if actions.len() < options.only_pairs.len() {
        let message = format!(
            "{} of the {} reviewed row(s) are no longer planned as shown, because a file changed since the scan; they are left alone. Scan again to review them.",
            options.only_pairs.len() - actions.len(),
            options.only_pairs.len()
        );
        logger::warn("run", &message);
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Log { message });
        } else if !options.json_output {
            eprintln!("{} {message}", "⚠".yellow());
        }
    }
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

    let ApplyOutcome {
        applied: applied_actions,
        already_linked,
        skipped,
        strict_error: strict_link_error,
        cancelled: apply_cancelled,
    } = if options.dry_run {
        ApplyOutcome::default()
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
            &ApplyOptions {
                downloads,
                strict: options.strict,
                json_output: options.json_output,
                tui_events: options.tui_events,
                link_mode: options.link_mode,
                cancel: &options.cancel,
            },
            &mut warnings,
            &mut report_entries,
        )
    };
    if apply_cancelled {
        let message = format!(
            "Link cancelled after {applied_actions} of {planned_actions} action(s); the rest stay planned."
        );
        logger::info("run", &message);
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Log {
                message: message.clone(),
            });
        } else if !options.json_output {
            eprintln!("{} {message}", "⚠".yellow());
        }
    }

    if skipped > 0 && !machine_output {
        println!(
            "{} Left alone {skipped} pair(s) on purpose; each entry's `error` in the report says why.",
            "ℹ".cyan()
        );
    }
    if already_linked > 0 && !machine_output {
        println!(
            "{} {already_linked} pair(s) already were one file; nothing changed. The filesystem gives one file different inode numbers: on mergerfs, mount with inodecalc=hybrid-hash, or use_ino on older versions.",
            "ℹ".cyan()
        );
    }
    if !machine_output {
        report::print_dangling_symlinks(&metrics.dangling_symlinks);
        print_run_summary(
            files_seen,
            &metrics,
            planned_actions,
            applied_actions,
            report::symlinked_count(&report_entries),
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

/// A download that is a library file seen through a second mount means the
/// downloads folder reaches into the library, through a bind mount or a mergerfs
/// branch. Such downloads are never planned; say so, since the configuration
/// is what needs fixing.
fn report_second_mount_skips(
    skipped: &[(std::path::PathBuf, std::path::PathBuf)],
    options: &ScanRunOptions<'_>,
    metrics: &mut RunMetrics,
) {
    metrics.skipped_as_library_file = skipped.len() as u64;
    for (download, library) in skipped {
        logger::warn(
            "plan",
            format!(
                "left alone {}: it is the library file {} seen through a second mount (same size and timestamps on another device)",
                download.display(),
                library.display()
            ),
        );
    }
    if skipped.is_empty() {
        return;
    }
    let message = format!(
        "Left alone {} download(s) that are library files seen through a second mount, such as {}. The downloads folder reaches into the library through a bind mount or a mergerfs branch: point downloads and media at the same view of the disks. The log lists every one.",
        skipped.len(),
        skipped[0].0.display()
    );
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Log { message });
    } else if !options.json_output {
        eprintln!("{} {message}", "⚠".yellow());
    }
}

#[cfg(test)]
mod tests;
