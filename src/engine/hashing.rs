use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::types::emit_tui_event;
use super::types::{Cache, CacheEntry, FileInfo, RunWarnings};
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

fn full_digest(path: &Path) -> Result<String> {
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
    warnings: &mut RunWarnings,
) -> Result<()> {
    let bar = if show_progress {
        let bar = ProgressBar::new(groups.len() as u64);
        bar.set_style(
            ProgressStyle::with_template("{msg} {pos}/{len} [{wide_bar:.cyan/blue}] {elapsed}")
                .unwrap(),
        );
        bar.set_message("Hashing groups".bold().to_string());
        Some(bar)
    } else {
        None
    };

    let sample_hash_errors = Arc::new(AtomicU64::new(0));
    let full_hash_errors = Arc::new(AtomicU64::new(0));
    let bar_for_threads = bar.clone();
    let sample_counter = Arc::clone(&sample_hash_errors);
    let full_counter = Arc::clone(&full_hash_errors);
    let cache_for_threads = Arc::clone(cache);
    let inode_hash_for_threads = Arc::clone(inode_hash);
    groups.par_iter_mut().try_for_each(|group| -> Result<()> {
        let mut sample_hashes: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
        for (i, fi) in group.iter().enumerate() {
            if tui_events {
                let _ = emit_tui_event(&TuiEvent::File {
                    phase: Some("hashing".to_string()),
                    path: fi.path.display().to_string(),
                });
            }
            match sample_digest(&fi.path, fi.size) {
                Ok(h) => {
                    sample_hashes.entry(h).or_default().push(i);
                }
                Err(err) => {
                    sample_counter.fetch_add(1, Ordering::Relaxed);
                    if strict {
                        return Err(anyhow!(
                            "sample hash error for {}: {}",
                            fi.path.display(),
                            err
                        ));
                    }
                }
            }
        }

        for indices in sample_hashes.values().filter(|v| v.len() > 1) {
            for &i in indices {
                let fi = &mut group[i];
                if let Ok(ih) = inode_hash_for_threads.lock() {
                    if let Some(h) = ih.get(&(fi.dev, fi.inode)) {
                        fi.hash = Some(h.clone());
                        continue;
                    }
                }
                let key = fi.path.to_string_lossy().into_owned();
                if let Ok(c) = cache_for_threads.lock() {
                    if let Some(e) = c.get(&key) {
                        if e.size == fi.size && e.mtime == fi.mtime_secs {
                            fi.hash = Some(e.hash.clone());
                            continue;
                        }
                    }
                }
                if tui_events {
                    let _ = emit_tui_event(&TuiEvent::File {
                        phase: Some("hashing".to_string()),
                        path: fi.path.display().to_string(),
                    });
                }
                match full_digest(&fi.path) {
                    Ok(h) => {
                        if let Ok(mut ih) = inode_hash_for_threads.lock() {
                            ih.insert((fi.dev, fi.inode), h.clone());
                        }
                        fi.hash = Some(h.clone());
                        if let Ok(mut c) = cache_for_threads.lock() {
                            c.insert(
                                key,
                                CacheEntry {
                                    size: fi.size,
                                    mtime: fi.mtime_secs,
                                    hash: h,
                                },
                            );
                        }
                    }
                    Err(err) => {
                        full_counter.fetch_add(1, Ordering::Relaxed);
                        if strict {
                            return Err(anyhow!(
                                "full hash error for {}: {}",
                                fi.path.display(),
                                err
                            ));
                        }
                    }
                }
            }
        }

        if let Some(bar) = &bar_for_threads {
            bar.inc(1);
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
