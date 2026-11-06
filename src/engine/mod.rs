// Printing goes through report::print_out, which keeps stdout for the
// dashboard's event stream in --tui-events mode, and report::print_err.
#![deny(clippy::print_stdout, clippy::print_stderr)]

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
mod reviewed;
mod types;

pub use cancel::{CancelToken, Cancelled};
pub use lock::LockHeld;
pub use types::LinkMode;
pub use types::ScanRunOptions;

use crate::protocol::Outcome;
use crate::protocol::WarningSummary;
use crate::protocol::{Counts, Phase, TuiEvent};
use cache::{CacheKeys, load_cache, prune_cache, save_cache};
use collect::{CollectOptions, collect_run_files, group_files_by_size};
use hashing::{CHECKPOINT_EVERY, Checkpoint, HashOptions, Hashed, hash_groups_and_update_cache};
use linking::{ApplyOptions, ApplyOutcome, apply_actions, plan_actions};
use report::{
    RunReportInput, build_report_entries, build_run_report, maybe_confirm_apply, planned_bytes,
    print_run_summary, resolve_report_path, strict_failure, try_write_run_report,
};
use types::{Audience, Cache, LinkAction, RunMetrics};

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
    exclude: Vec<String>,
) -> Result<()> {
    let downloads = collect::RootId::of("downloads", downloads)?;
    let media = collect::RootId::of("media", media)?;
    let exclude = collect::Exclude::new(exclude);
    collect::ensure_root_not_reached(&downloads, &media, &exclude)?;
    collect::ensure_root_not_reached(&media, &downloads, &exclude)
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

/// How a scan or link that returned ended, for its exit code (ADR 0003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// It finished and nothing failed, including a run with nothing to do.
    Clean,
    /// It finished, but a file could not be read, hashed or linked: what
    /// `--strict` would have stopped on.
    WithFailures,
    /// A link stopped on request partway; the pairs it linked stay linked.
    Cancelled,
}

/// A run `--strict` stopped after building its report, which the error
/// carries so the dashboard can show which pairs were linked before it.
#[derive(Debug)]
pub struct StoppedWithReport {
    report: Box<crate::protocol::RunReport>,
    error: anyhow::Error,
}

impl std::fmt::Display for StoppedWithReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl std::error::Error for StoppedWithReport {}

/// Open a `--tui-events` stream. The first event names the protocol, so the
/// dashboard can refuse a binary that speaks another.
pub fn open_tui_stream() {
    let _ = types::emit_tui_event(&TuiEvent::Hello {
        protocol: crate::protocol::PROTOCOL_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
    });
}

/// Close a `--tui-events` stream for a run that returned an error, from a
/// config error before it started to a strict failure partway: exactly one
/// final event, `cancelled` for a cancel and `error` for anything else, with
/// the whole cause and the report when the run built one.
pub fn close_tui_stream_on(err: &anyhow::Error) {
    let event = if err.is::<Cancelled>() {
        TuiEvent::Cancelled
    } else {
        TuiEvent::Error {
            message: format!("{err:#}"),
            report: err
                .downcast_ref::<StoppedWithReport>()
                .map(|stopped| stopped.report.clone()),
        }
    };
    let _ = types::emit_tui_event(&event);
}

impl RunEnd {
    /// The exit of a run that returned, from how it ended and whether
    /// anything failed on the way. A run `--strict` stopped returns an
    /// error instead, so `Failed` only reaches here with failures.
    fn of(outcome: Outcome, failures: bool) -> Self {
        match outcome {
            Outcome::Cancelled => RunEnd::Cancelled,
            Outcome::Complete | Outcome::Failed if failures => RunEnd::WithFailures,
            Outcome::Complete | Outcome::Failed => RunEnd::Clean,
        }
    }
}

pub fn run_scan_with_options(config: &Config, options: ScanRunOptions<'_>) -> Result<RunEnd> {
    let result = run(config, options);
    if let Err(err) = &result
        && err.is::<Cancelled>()
    {
        logger::info("run", "cancelled by user before any change was made");
    }
    result
}

fn run(config: &Config, options: ScanRunOptions<'_>) -> Result<RunEnd> {
    let started_at = std::time::SystemTime::now();
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
    let audience = options.audience();
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
    let exclude = collect::Exclude::new(config.effective_exclude());
    let quick_pool = build_pool("quick", config.effective_threads())?;
    let full_pool = build_pool("full", config.effective_full_threads())?;
    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();
    let cache_path = cache_path(config)?;
    logger::info("run", format!("using cache path {}", cache_path.display()));
    let roots = Roots {
        downloads: collect::RootId::of("downloads", downloads)?,
        media: collect::RootId::of("media", media)?,
    };
    let filters = reviewed::Filters {
        min_size: config.effective_min_size(),
        deadline: std::time::SystemTime::now() - min_age,
        exclude: &exclude,
    };
    let from_cache = if options.only_pairs.is_empty() {
        None
    } else {
        plan_reviewed_rows(&options, &roots, &cache_path, &filters)?
    };
    let Planned {
        files_seen,
        groups_found,
        mut actions,
    } = match from_cache {
        Some(planned) => planned,
        None => match plan_by_walking(
            config,
            &options,
            &roots,
            &cache_path,
            &filters,
            Pools {
                quick: &quick_pool,
                full: &full_pool,
            },
            &mut warnings,
            &mut metrics,
        ) {
            Ok(planned) => planned,
            Err(err) => {
                return Err(stopped_before_planning(
                    err,
                    &options,
                    started_at,
                    (downloads, media),
                    metrics,
                    warnings,
                ));
            }
        },
    };
    linking::sort_actions(&mut actions);
    if actions.len() < options.only_pairs.len() {
        let message = format!(
            "{} of the {} reviewed row(s) are no longer planned as shown, because a file changed since the scan; they are left alone. Scan again to review them.",
            options.only_pairs.len() - actions.len(),
            options.only_pairs.len()
        );
        logger::warn("run", &message);
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Log { message });
        } else if audience.is_chatty() {
            report::print_warning(message);
        }
    }
    let bytes_to_free = planned_bytes(&actions);
    let planned_actions = actions.len();
    logger::info(
        "run",
        format!(
            "planned {} action(s), potential_saved_bytes={}",
            planned_actions, bytes_to_free
        ),
    );
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: Phase::Planning,
        });
        let _ = types::emit_tui_event(&TuiEvent::Count(Counts {
            files_seen: Some(files_seen),
            groups_found: Some(groups_found),
            planned_actions: Some(planned_actions),
            applied_actions: Some(0),
            ..Counts::default()
        }));
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: format!("Planning complete. {planned_actions} action(s) queued."),
        });
    }

    if audience.is_chatty() {
        report::print_plan(audience, &actions, downloads, media, bytes_to_free);
    }
    let out_path = resolve_report_path(downloads, &options, planned_actions);
    let mut report_entries = build_report_entries(&actions, options.link_mode);

    let ApplyOutcome {
        counts: applied,
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
                phase: Phase::Applying,
            });
            let _ = types::emit_tui_event(&TuiEvent::Log {
                message: "Applying filesystem changes".to_string(),
            });
        }
        maybe_confirm_apply(options.require_confirm, planned_actions)?;
        logger::info("run", "starting apply phase");
        if audience.is_chatty() && planned_actions > 0 {
            report::print_out(audience, format_args!("{} Linking…", "▶".cyan()));
        }
        apply_actions(
            &actions,
            media,
            &ApplyOptions {
                downloads,
                strict: options.strict,
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
            "Link cancelled after {} of {planned_actions} action(s); the rest stay planned.",
            applied.applied
        );
        logger::info("run", &message);
        if options.tui_events {
            let _ = types::emit_tui_event(&TuiEvent::Log {
                message: message.clone(),
            });
        } else if audience.is_person() {
            report::print_warning(message);
        }
    }

    if audience.is_chatty() {
        // Each pair left alone on purpose, with why: the run decided it, so a
        // person should see it, though nothing failed.
        for reason in report_entries
            .iter()
            .filter(|entry| entry.is_skipped())
            .filter_map(|entry| entry.error.as_deref())
        {
            report::print_warning(reason);
        }
        if applied.already_linked > 0 {
            report::print_out(
                audience,
                format_args!(
                    "{} {} pair(s) already were one file; nothing changed. The filesystem gives one file different inode numbers: on mergerfs, mount with inodecalc=hybrid-hash, or use_ino on older versions.",
                    "ℹ".cyan(),
                    applied.already_linked
                ),
            );
        }
        report::print_dangling_symlinks(&metrics.dangling_symlinks);
    }
    if audience.is_person() {
        print_run_summary(
            files_seen,
            &metrics,
            report::symlinked_count(&report_entries),
            &warnings,
            audience,
        );
    }

    let strict_error = strict_failure(strict_link_error, options.strict, &warnings);
    let outcome = if strict_error.is_some() {
        Outcome::Failed
    } else if apply_cancelled {
        Outcome::Cancelled
    } else {
        Outcome::Complete
    };
    let report = build_run_report(
        &options,
        RunReportInput {
            outcome,
            started_at,
            downloads,
            media,
            files_seen,
            planned_actions,
            planned_bytes: bytes_to_free,
            applied,
            metrics: metrics.clone(),
            warnings: warnings.clone(),
            entries: report_entries,
        },
    );
    if let Some(path) = out_path.as_deref() {
        logger::info("run", format!("writing report to {}", path.display()));
        try_write_run_report(path, &report);
    }
    match audience {
        // A script reading --json gets the report of a run --strict
        // stopped too.
        Audience::Script => report::print_out(audience, serde_json::to_string_pretty(&report)?),
        Audience::Person | Audience::Quiet if !options.dry_run => report::Verdict {
            planned: planned_actions,
            applied,
            failed: warnings.link_errors,
            symlinked: report.symlinked,
            cancelled: apply_cancelled,
            report: out_path.as_deref(),
        }
        .print(audience),
        Audience::Person if planned_actions > 0 => report::print_out(
            audience,
            format_args!(
                "{} Scan only: nothing was changed. `dejaview link` links them.",
                "ℹ".cyan()
            ),
        ),
        _ => {}
    }
    logger::info(
        "run",
        format!(
            "finished mode={} outcome={:?} planned={} applied={} warnings={}",
            report.mode.as_str(),
            report.outcome,
            report.planned_actions,
            report.applied_actions,
            report.warnings.total()
        ),
    );

    if let Some(error) = strict_error {
        return Err(anyhow::Error::new(StoppedWithReport {
            report: Box::new(report),
            error,
        }));
    }

    let end = RunEnd::of(outcome, report.warnings.total() > 0);
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Done {
            report: Box::new(report),
        });
    }
    Ok(end)
}

/// A run `--strict` stopped while walking or hashing still answers a script
/// with a report (ADR 0003): outcome `failed`, what the walk counted, no
/// entries. It goes to `--report` when one was given, as `--json` when
/// asked, and rides on the error to the dashboard. Any other error, a
/// cancel or a refusal, is returned as it is.
fn stopped_before_planning(
    err: anyhow::Error,
    options: &ScanRunOptions<'_>,
    started_at: std::time::SystemTime,
    (downloads, media): (&std::path::Path, &std::path::Path),
    metrics: RunMetrics,
    warnings: WarningSummary,
) -> anyhow::Error {
    if !options.strict || err.is::<Cancelled>() || warnings.total() == 0 {
        return err;
    }
    let report = build_run_report(
        options,
        RunReportInput {
            outcome: Outcome::Failed,
            started_at,
            downloads,
            media,
            files_seen: metrics.files_seen,
            planned_actions: 0,
            planned_bytes: 0,
            applied: Default::default(),
            metrics,
            warnings,
            entries: Vec::new(),
        },
    );
    if let Some(path) = resolve_report_path(downloads, options, 0) {
        try_write_run_report(&path, &report);
    }
    if options.audience() == Audience::Script
        && let Ok(json) = serde_json::to_string_pretty(&report)
    {
        report::print_out(Audience::Script, json);
    }
    anyhow::Error::new(StoppedWithReport {
        report: Box::new(report),
        error: err,
    })
}

/// The two roots of a run, as the filesystem knows them.
struct Roots {
    downloads: collect::RootId,
    media: collect::RootId,
}

/// The run's two worker pools.
#[derive(Clone, Copy)]
struct Pools<'a> {
    quick: &'a rayon::ThreadPool,
    full: &'a rayon::ThreadPool,
}

/// What planning found, however it got there.
struct Planned {
    files_seen: usize,
    groups_found: usize,
    actions: Vec<LinkAction>,
}

/// The cache with its keys in this version's form, and how many keys were
/// renamed from an earlier version's.
fn load_keyed_cache(cache_path: &std::path::Path, roots: &Roots) -> (Cache, CacheKeys, usize) {
    let mut cache = load_cache(cache_path);
    let keys = CacheKeys::new(&roots.downloads.path, &roots.media.path);
    let migrated = keys.migrate(&mut cache);
    if migrated > 0 {
        logger::info(
            "cache",
            format!("renamed {migrated} entries to keys relative to their folder"),
        );
    }
    (cache, keys, migrated)
}

/// Plan a dashboard link of reviewed rows from the cache, without a walk,
/// when the cache vouches for every row; None sends the run to the walk.
fn plan_reviewed_rows(
    options: &ScanRunOptions<'_>,
    roots: &Roots,
    cache_path: &std::path::Path,
    filters: &reviewed::Filters<'_>,
) -> Result<Option<Planned>> {
    let (cache, keys, _) = load_keyed_cache(cache_path, roots);
    let checked = reviewed::plan_from_cache(
        options.only_pairs,
        &roots.downloads,
        &roots.media,
        &cache,
        &keys,
        filters,
    )?;
    let actions = match checked {
        Ok(actions) => actions,
        Err(why) => {
            let message =
                format!("Checking the reviewed rows by walking both folders, because {why}.");
            logger::info("run", &message);
            if options.tui_events {
                let _ = types::emit_tui_event(&TuiEvent::Log { message });
            }
            return Ok(None);
        }
    };
    let message = format!(
        "The cache vouches for all {} reviewed row(s): both files of each are unchanged since the scan hashed them, so neither folder is walked again.",
        actions.len()
    );
    logger::info("run", &message);
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Log { message });
    }
    let files_seen = actions
        .iter()
        .flat_map(|action| [&action.src, &action.tgt])
        .collect::<HashSet<_>>()
        .len();
    Ok(Some(Planned {
        files_seen,
        groups_found: actions.len(),
        actions,
    }))
}

/// Walk both roots, hash what the cache does not know, and plan.
#[allow(clippy::too_many_arguments)]
fn plan_by_walking(
    config: &Config,
    options: &ScanRunOptions<'_>,
    roots: &Roots,
    cache_path: &std::path::Path,
    filters: &reviewed::Filters<'_>,
    pools: Pools<'_>,
    warnings: &mut WarningSummary,
    metrics: &mut RunMetrics,
) -> Result<Planned> {
    if options.tui_events {
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: Phase::Collecting,
        });
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: "Collecting candidate files".to_string(),
        });
    }
    let files = collect_run_files(
        &roots.downloads.path,
        &roots.media.path,
        std::time::Duration::from_secs(config.min_age.unwrap_or(0)),
        CollectOptions {
            min_size: filters.min_size,
            exclude: filters.exclude,
            strict: options.strict,
            show_progress: options.show_progress,
            tui_events: options.tui_events,
            cancel: &options.cancel,
        },
        pools.quick,
        warnings,
        metrics,
    )?;
    let files_seen = files.len();
    metrics.files_seen = files_seen;
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
        let _ = types::emit_tui_event(&TuiEvent::Count(Counts {
            files_seen: Some(files_seen),
            ..Counts::default()
        }));
        let _ = types::emit_tui_event(&TuiEvent::Phase {
            phase: Phase::HashingSample,
        });
        let _ = types::emit_tui_event(&TuiEvent::Log {
            message: "Hashing files to identify duplicate candidates".to_string(),
        });
    }

    let (mut cache, keys, migrated) = load_keyed_cache(cache_path, roots);

    let seen_paths: HashSet<String> = files
        .iter()
        .map(|file| keys.key(&file.path, file.in_downloads))
        .collect();
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
    let Hashed {
        cache_writes,
        result: hash_result,
    } = pools.quick.install(|| {
        hash_groups_and_update_cache(
            &mut groups,
            &mut cache,
            &HashOptions {
                strict: options.strict,
                show_progress: options.show_progress,
                tui_events: options.tui_events,
                link_mode: options.link_mode,
                selection: options,
                cancel: &options.cancel,
                full_pool: pools.full,
                keys: &keys,
                checkpoint: Checkpoint {
                    path: cache_path,
                    every: CHECKPOINT_EVERY,
                },
            },
            warnings,
        )
    });
    let mut pruned = 0;
    if hash_result.is_ok() {
        // A folder that could not be read keeps its entries; the rest of
        // the cache still follows the trees, so one unreadable folder (a
        // NAS's recycle bin) does not stop pruning for good.
        let unreadable: Vec<String> = metrics
            .unreadable
            .iter()
            .map(|(path, in_downloads)| keys.key(path, *in_downloads))
            .collect();
        pruned = prune_cache(&mut cache, &seen_paths, &unreadable);
        if pruned > 0 {
            logger::info(
                "cache",
                format!("pruned {pruned} entries for paths no longer present"),
            );
        }
    } else {
        logger::info(
            "cache",
            "hashing stopped early; keeping entries for paths not seen this run",
        );
    }
    // Hashes finished before a cancel or a strict failure are valid: keep
    // them. An unchanged cache is not written again: on a large library that
    // is tens of megabytes and an fsync every run.
    if migrated + cache_writes + pruned > 0 {
        save_cache(cache_path, &cache)?;
    } else {
        logger::info("cache", "cache unchanged; not saved");
    }
    hash_result?;

    let linking::Plan {
        actions,
        seen_through_second_mount,
    } = plan_actions(&groups);
    report_second_mount_skips(&seen_through_second_mount, options, metrics);
    let actions = actions
        .into_iter()
        .filter(|action| options.selects(action))
        .collect::<Vec<_>>();
    Ok(Planned {
        files_seen,
        groups_found: groups.len(),
        actions,
    })
}

/// A pool of `threads` workers for one kind of I/O: `quick` walks and
/// quick-hashes, `full` reads files whole.
fn build_pool(kind: &str, threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name({
            let kind = kind.to_string();
            move |index| format!("dejaview-{kind}-{index}")
        })
        .build()
        .map_err(|err| {
            anyhow!(
                "cannot start {threads} {kind} worker thread(s): {err}; lower the count with `dejaview config {}`",
                if kind == "full" { "set-full-threads" } else { "set-threads" }
            )
        })
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
    match options.audience() {
        Audience::Dashboard => {
            let _ = types::emit_tui_event(&TuiEvent::Log { message });
        }
        Audience::Person => report::print_warning(message),
        Audience::Quiet | Audience::Script => {}
    }
}

#[cfg(test)]
#[allow(clippy::print_stderr)]
mod tests;
