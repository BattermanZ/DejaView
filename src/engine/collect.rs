use anyhow::{anyhow, Result};
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, SystemTime};
use walkdir::{DirEntry, WalkDir};

use super::types::emit_tui_event;
use super::types::{FileInfo, RunMetrics, RunWarnings};
use crate::protocol::TuiEvent;

#[derive(Copy, Clone)]
pub(super) struct CollectOptions {
    pub strict: bool,
    pub show_progress: bool,
    pub tui_events: bool,
}

fn is_regular(entry: &DirEntry) -> bool {
    entry.file_type().is_file() && !entry.path_is_symlink()
}

pub(super) fn collect_files(
    root: &Path,
    in_downloads: bool,
    min_age: Duration,
    strict: bool,
    tui_events: bool,
    warnings: &mut RunWarnings,
    metrics: &mut RunMetrics,
) -> Result<Vec<FileInfo>> {
    let deadline = SystemTime::now() - min_age;
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warnings.walk_errors += 1;
                if strict {
                    return Err(anyhow!("walk error under {}: {}", root.display(), err));
                }
                continue;
            }
        };
        if !is_regular(&entry) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                warnings.metadata_errors += 1;
                if strict {
                    return Err(anyhow!(
                        "metadata error for {}: {}",
                        entry.path().display(),
                        err
                    ));
                }
                continue;
            }
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mtime_secs = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if mtime > deadline {
            metrics.skipped_by_min_age += 1;
            continue;
        }
        files.push(FileInfo {
            path: entry.into_path(),
            size: meta.len(),
            mtime_secs,
            dev: meta.dev(),
            inode: meta.ino(),
            hash: None,
            in_downloads,
        });

        if tui_events {
            let path = files
                .last()
                .expect("just pushed file")
                .path
                .display()
                .to_string();
            let _ = emit_tui_event(&TuiEvent::File {
                phase: Some("collecting".to_string()),
                path,
            });
            if files.len() == 1 || files.len() % 25 == 0 {
                let _ = emit_tui_event(&TuiEvent::Count {
                    files_seen: Some(files.len()),
                    groups_found: None,
                    planned_actions: None,
                    applied_actions: None,
                    hash_progress_done: None,
                    hash_progress_total: None,
                });
            }
        }
    }
    Ok(files)
}

pub(super) fn collect_run_files(
    downloads: &Path,
    media: &Path,
    min_age: Duration,
    options: CollectOptions,
    warnings: &mut RunWarnings,
    metrics: &mut RunMetrics,
) -> Result<Vec<FileInfo>> {
    let spinner = if options.show_progress {
        let mp = MultiProgress::new();
        let spinner = mp.add(ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .unwrap(),
        );
        spinner.enable_steady_tick(Duration::from_millis(120));
        spinner.set_message("Collecting files…".cyan().bold().to_string());
        Some(spinner)
    } else {
        None
    };

    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut files = collect_files(
        downloads,
        true,
        min_age,
        options.strict,
        options.tui_events,
        warnings,
        metrics,
    )?;
    for f in &files {
        seen.insert((f.dev, f.inode));
    }

    let mut media_files = collect_files(
        media,
        false,
        min_age,
        options.strict,
        options.tui_events,
        warnings,
        metrics,
    )?;
    media_files.retain(|f| !seen.contains(&(f.dev, f.inode)));
    files.extend(media_files);

    if let Some(spinner) = spinner {
        spinner.finish_with_message("File list complete ✔".green().bold().to_string());
    }

    Ok(files)
}

pub(super) fn group_files_by_size(files: Vec<FileInfo>) -> Vec<Vec<FileInfo>> {
    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files {
        size_map.entry(f.size).or_default().push(f);
    }
    size_map.into_values().filter(|v| v.len() > 1).collect()
}
