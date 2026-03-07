use anyhow::{Context, Result, anyhow};
use blake3::Hasher;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::current_thread_index;
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::types::emit_tui_event;
use super::linking::plan_hashed_group_actions;
use super::report::build_report_entries;
use super::types::{Cache, CacheEntry, FileInfo, LinkMode, RunWarnings};
use crate::protocol::TuiEvent;

fn sample_digest(path: &Path, size: u64) -> Result<[u8; 32]> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let n = file.read(&mut buf)?;
    hasher.update(&buf[..n]);
    if size > buf.len() as u64 {
        let offset = size - buf.len() as u64;
        file.seek(SeekFrom::Start(offset))?;
        let n = file.read(&mut buf)?;
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

fn worker_id() -> usize {
    current_thread_index().map_or(1, |idx| idx + 1)
}

fn emit_worker_activity(phase: &str, worker: usize, path: Option<String>, tui_events: bool) {
    if tui_events {
        let _ = emit_tui_event(&TuiEvent::WorkerActivity {
            phase: phase.to_string(),
            worker,
            path,
        });
    }
}

fn emit_hash_progress(done: usize, total: usize, tui_events: bool) {
    if tui_events {
        let _ = emit_tui_event(&TuiEvent::Count {
            files_seen: None,
            groups_found: None,
            planned_actions: None,
            applied_actions: None,
            hash_progress_done: Some(done),
            hash_progress_total: Some(total),
        });
    }
}

pub(super) fn full_digest(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Hasher::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn hash_groups_and_update_cache(
    groups: &mut [Vec<FileInfo>],
    cache: &Arc<Mutex<Cache>>,
    inode_hash: &Arc<Mutex<HashMap<(u64, u64), String>>>,
    strict: bool,
    show_progress: bool,
    tui_events: bool,
    link_mode: LinkMode,
    warnings: &mut RunWarnings,
) -> Result<()> {
    let total_sample_files = groups.iter().map(Vec::len).sum::<usize>();
    let total_sample_u64 = total_sample_files as u64;
    let bar = if show_progress {
        let bar = ProgressBar::new(total_sample_u64);
        bar.set_style(
            ProgressStyle::with_template("{msg} {pos}/{len} [{wide_bar:.cyan/blue}] {elapsed}")
                .unwrap(),
        );
        bar.set_message("Hashing files".bold().to_string());
        Some(bar)
    } else {
        None
    };

    let sample_hash_errors = Arc::new(AtomicU64::new(0));
    let full_hash_errors = Arc::new(AtomicU64::new(0));
    let sample_done = Arc::new(AtomicU64::new(0));
    let full_done = Arc::new(AtomicU64::new(0));
    let bar_for_samples = bar.clone();
    let sample_errors = Arc::clone(&sample_hash_errors);
    let full_errors = Arc::clone(&full_hash_errors);
    let sample_done_for_threads = Arc::clone(&sample_done);
    let cache_for_threads = Arc::clone(cache);
    let inode_hash_for_threads = Arc::clone(inode_hash);

    let full_candidates = groups
        .par_iter_mut()
        .map(|group| -> Result<Vec<usize>> {
            let worker = worker_id();
            let mut sample_hashes: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
            for (idx, file) in group.iter().enumerate() {
                emit_worker_activity(
                    "hashing_sample",
                    worker,
                    Some(file.path.display().to_string()),
                    tui_events,
                );

                let digest_result = sample_digest(&file.path, file.size);
                let done = sample_done_for_threads.fetch_add(1, Ordering::Relaxed) + 1;
                emit_hash_progress(done as usize, total_sample_files, tui_events);
                if let Some(progress_bar) = &bar_for_samples {
                    progress_bar.inc(1);
                }
                emit_worker_activity("hashing_sample", worker, None, tui_events);

                match digest_result {
                    Ok(digest) => {
                        sample_hashes.entry(digest).or_default().push(idx);
                    }
                    Err(err) => {
                        sample_errors.fetch_add(1, Ordering::Relaxed);
                        if strict {
                            return Err(anyhow!(
                                "sample hash error for {}: {}",
                                file.path.display(),
                                err
                            ));
                        }
                    }
                }
            }

            Ok(sample_hashes
                .values()
                .filter(|indices| indices.len() > 1)
                .flat_map(|indices| indices.iter().copied())
                .collect())
        })
        .collect::<Result<Vec<_>>>()?;

    let total_full_files = full_candidates.iter().map(Vec::len).sum::<usize>();
    if tui_events && total_full_files > 0 {
        let _ = emit_tui_event(&TuiEvent::Phase {
            phase: "hashing_full".to_string(),
        });
        let _ = emit_tui_event(&TuiEvent::Log {
            message: format!("Verifying {total_full_files} duplicate candidate file(s)"),
        });
        emit_hash_progress(0, total_full_files, true);
    }
    if let Some(progress_bar) = &bar {
        progress_bar.set_length(total_sample_u64 + total_full_files as u64);
        if total_full_files > 0 {
            progress_bar.set_message("Verifying duplicates".bold().to_string());
        }
    }

    let full_done_for_threads = Arc::clone(&full_done);
    groups
        .par_iter_mut()
        .zip(full_candidates.into_par_iter())
        .try_for_each(|(group, indices)| -> Result<()> {
            let worker = worker_id();
            for idx in indices {
                let file = &mut group[idx];

                if let Ok(inode_map) = inode_hash_for_threads.lock()
                    && let Some(hash) = inode_map.get(&(file.dev, file.inode))
                {
                    file.hash = Some(hash.clone());
                    let done = full_done_for_threads.fetch_add(1, Ordering::Relaxed) + 1;
                    emit_hash_progress(done as usize, total_full_files, tui_events);
                    if let Some(progress_bar) = &bar {
                        progress_bar.inc(1);
                    }
                    continue;
                }

                let key = file.path.to_string_lossy().into_owned();
                if let Ok(cache_map) = cache_for_threads.lock()
                    && let Some(entry) = cache_map.get(&key)
                    && entry.size == file.size
                    && entry.mtime_ns == file.mtime_ns
                    && entry.ctime_ns == file.ctime_ns
                    && entry.dev == file.dev
                    && entry.inode == file.inode
                {
                    file.hash = Some(entry.hash.clone());
                    let done = full_done_for_threads.fetch_add(1, Ordering::Relaxed) + 1;
                    emit_hash_progress(done as usize, total_full_files, tui_events);
                    if let Some(progress_bar) = &bar {
                        progress_bar.inc(1);
                    }
                    continue;
                }

                emit_worker_activity(
                    "hashing_full",
                    worker,
                    Some(file.path.display().to_string()),
                    tui_events,
                );
                let digest_result = full_digest(&file.path);
                let done = full_done_for_threads.fetch_add(1, Ordering::Relaxed) + 1;
                emit_hash_progress(done as usize, total_full_files, tui_events);
                if let Some(progress_bar) = &bar {
                    progress_bar.inc(1);
                }
                emit_worker_activity("hashing_full", worker, None, tui_events);

                match digest_result {
                    Ok(hash) => {
                        if let Ok(mut inode_map) = inode_hash_for_threads.lock() {
                            inode_map.insert((file.dev, file.inode), hash.clone());
                        }
                        file.hash = Some(hash.clone());
                        if let Ok(mut cache_map) = cache_for_threads.lock() {
                            cache_map.insert(
                                key,
                                CacheEntry {
                                    size: file.size,
                                    mtime_ns: file.mtime_ns,
                                    ctime_ns: file.ctime_ns,
                                    dev: file.dev,
                                    inode: file.inode,
                                    hash,
                                },
                            );
                        }
                    }
                    Err(err) => {
                        full_errors.fetch_add(1, Ordering::Relaxed);
                        if strict {
                            return Err(anyhow!(
                                "full hash error for {}: {}",
                                file.path.display(),
                                err
                            ));
                        }
                    }
                }
            }
            if tui_events {
                let entries = build_report_entries(&plan_hashed_group_actions(group), link_mode);
                if !entries.is_empty() {
                    let _ = emit_tui_event(&TuiEvent::PlannedEntries { entries });
                }
            }
            Ok(())
        })?;

    warnings.sample_hash_errors += sample_hash_errors.load(Ordering::Relaxed);
    warnings.full_hash_errors += full_hash_errors.load(Ordering::Relaxed);
    if let Some(bar) = bar {
        bar.finish_with_message("Hashing complete ✔".green().bold().to_string());
    }

    Ok(())
}
