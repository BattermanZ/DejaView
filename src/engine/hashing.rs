use anyhow::{Context, Result, anyhow};
use blake3::Hasher;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::current_thread_index;
use rayon::prelude::*;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::linking::plan_hashed_group_actions;
use super::report::build_report_entries;
use super::types::emit_tui_event;
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

pub(super) struct HashOptions<'a> {
    pub(super) strict: bool,
    pub(super) show_progress: bool,
    pub(super) tui_events: bool,
    pub(super) link_mode: LinkMode,
    pub(super) only_sources: &'a [PathBuf],
}

/// Hashing work for one size group. Paths that share a (dev, inode) are one
/// physical file: only its first path is read, and the hash is copied to the rest.
struct GroupWork {
    /// Indices into the group, one entry per physical file.
    inodes: Vec<Vec<usize>>,
    /// Full hash per physical file, when any of its paths has a valid cache entry.
    cached: Vec<Option<String>>,
}

impl GroupWork {
    fn needs_sampling(&self) -> bool {
        self.cached.iter().any(Option::is_none)
    }
}

fn split_by_inode(group: &[FileInfo]) -> Vec<Vec<usize>> {
    let mut index: HashMap<(u64, u64), usize> = HashMap::new();
    let mut inodes: Vec<Vec<usize>> = Vec::new();
    for (idx, file) in group.iter().enumerate() {
        match index.entry((file.dev, file.inode)) {
            Entry::Occupied(slot) => inodes[*slot.get()].push(idx),
            Entry::Vacant(slot) => {
                slot.insert(inodes.len());
                inodes.push(vec![idx]);
            }
        }
    }
    inodes
}

fn cache_key(file: &FileInfo) -> String {
    file.path.to_string_lossy().into_owned()
}

fn cache_entry_matches(entry: &CacheEntry, file: &FileInfo) -> bool {
    entry.size == file.size
        && entry.mtime_ns == file.mtime_ns
        && entry.ctime_ns == file.ctime_ns
        && entry.dev == file.dev
        && entry.inode == file.inode
}

fn cached_hash(cache: &Cache, group: &[FileInfo], members: &[usize]) -> Option<String> {
    members.iter().find_map(|&idx| {
        let file = &group[idx];
        cache
            .get(&cache_key(file))
            .filter(|entry| cache_entry_matches(entry, file))
            .map(|entry| entry.hash.clone())
    })
}

/// None when every path in the group is the same physical file: such a group
/// cannot produce an action, so nothing in it is read.
fn plan_group_work(group: &[FileInfo], cache: &Cache) -> Option<GroupWork> {
    let inodes = split_by_inode(group);
    if inodes.len() < 2 {
        return None;
    }
    let cached = inodes
        .iter()
        .map(|members| cached_hash(cache, group, members))
        .collect();
    Some(GroupWork { inodes, cached })
}

fn assign_hash(group: &mut [FileInfo], members: &[usize], hash: &str) {
    for &idx in members {
        group[idx].hash = Some(hash.to_string());
    }
}

/// Write a cache entry for every hashed path whose entry is missing or stale, so
/// every path of a hardlinked file is served from cache next run, not only the
/// path that was read.
fn sync_cache(cache: &mut Cache, groups: &[Vec<FileInfo>]) {
    for file in groups.iter().flatten() {
        let Some(hash) = &file.hash else {
            continue;
        };
        let key = cache_key(file);
        let current = cache
            .get(&key)
            .is_some_and(|entry| cache_entry_matches(entry, file) && &entry.hash == hash);
        if !current {
            cache.insert(
                key,
                CacheEntry {
                    size: file.size,
                    mtime_ns: file.mtime_ns,
                    ctime_ns: file.ctime_ns,
                    dev: file.dev,
                    inode: file.inode,
                    hash: hash.clone(),
                },
            );
        }
    }
}

fn emit_planned_entries(group: &[FileInfo], options: &HashOptions<'_>) {
    if !options.tui_events {
        return;
    }
    let actions = plan_hashed_group_actions(group)
        .into_iter()
        .filter(|action| {
            options.only_sources.is_empty()
                || options.only_sources.iter().any(|path| path == &action.src)
        })
        .collect::<Vec<_>>();
    let entries = build_report_entries(&actions, options.link_mode);
    if !entries.is_empty() {
        let _ = emit_tui_event(&TuiEvent::PlannedEntries { entries });
    }
}

pub(super) fn hash_groups_and_update_cache(
    groups: &mut [Vec<FileInfo>],
    cache: &mut Cache,
    options: &HashOptions<'_>,
    warnings: &mut RunWarnings,
) -> Result<()> {
    let strict = options.strict;
    let tui_events = options.tui_events;
    let works: Vec<Option<GroupWork>> = groups
        .iter()
        .map(|group| plan_group_work(group, cache))
        .collect();

    // Groups whose every physical file is cached need no reads at all.
    for (group, work) in groups.iter_mut().zip(&works) {
        let Some(work) = work.as_ref().filter(|work| !work.needs_sampling()) else {
            continue;
        };
        for (members, hash) in work.inodes.iter().zip(&work.cached) {
            if let Some(hash) = hash {
                assign_hash(group, members, hash);
            }
        }
        emit_planned_entries(group, options);
    }

    let total_sample_files = works
        .iter()
        .flatten()
        .filter(|work| work.needs_sampling())
        .map(|work| work.inodes.len())
        .sum::<usize>();
    let total_sample_u64 = total_sample_files as u64;
    let bar = if options.show_progress {
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

    let sample_hash_errors = AtomicU64::new(0);
    let full_hash_errors = AtomicU64::new(0);
    let sample_done = AtomicU64::new(0);
    let full_done = AtomicU64::new(0);

    // Phase 1: sample one path per physical file and keep the physical files whose
    // sample collides with another's.
    let full_candidates = groups
        .par_iter()
        .zip(works.par_iter())
        .map(|(group, work)| -> Result<Vec<usize>> {
            let Some(work) = work.as_ref().filter(|work| work.needs_sampling()) else {
                return Ok(Vec::new());
            };
            let worker = worker_id();
            let mut sample_hashes: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
            for (inode_idx, members) in work.inodes.iter().enumerate() {
                let file = &group[members[0]];
                emit_worker_activity(
                    "hashing_sample",
                    worker,
                    Some(file.path.display().to_string()),
                    tui_events,
                );

                let digest_result = sample_digest(&file.path, file.size);
                let done = sample_done.fetch_add(1, Ordering::Relaxed) + 1;
                emit_hash_progress(done as usize, total_sample_files, tui_events);
                if let Some(progress_bar) = &bar {
                    progress_bar.inc(1);
                }
                emit_worker_activity("hashing_sample", worker, None, tui_events);

                match digest_result {
                    Ok(digest) => {
                        sample_hashes.entry(digest).or_default().push(inode_idx);
                    }
                    Err(err) => {
                        sample_hash_errors.fetch_add(1, Ordering::Relaxed);
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

            let mut candidates: Vec<usize> = sample_hashes
                .into_values()
                .filter(|inode_indices| inode_indices.len() > 1)
                .flatten()
                .collect();
            candidates.sort_unstable();
            Ok(candidates)
        })
        .collect::<Result<Vec<_>>>()?;

    let total_full_files = works
        .iter()
        .zip(&full_candidates)
        .filter_map(|(work, candidates)| work.as_ref().map(|work| (work, candidates)))
        .map(|(work, candidates)| {
            candidates
                .iter()
                .filter(|&&inode_idx| work.cached[inode_idx].is_none())
                .count()
        })
        .sum::<usize>();
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

    // Phase 2: full hash for each colliding physical file not already cached, then
    // copy the hash to every path of that file.
    groups
        .par_iter_mut()
        .zip(works.par_iter())
        .zip(full_candidates.into_par_iter())
        .try_for_each(|((group, work), candidates)| -> Result<()> {
            let Some(work) = work.as_ref().filter(|work| work.needs_sampling()) else {
                return Ok(());
            };
            let worker = worker_id();
            for inode_idx in candidates {
                let members = &work.inodes[inode_idx];
                let hash = if let Some(hash) = &work.cached[inode_idx] {
                    hash.clone()
                } else {
                    let path = group[members[0]].path.clone();
                    emit_worker_activity(
                        "hashing_full",
                        worker,
                        Some(path.display().to_string()),
                        tui_events,
                    );
                    let digest_result = full_digest(&path);
                    let done = full_done.fetch_add(1, Ordering::Relaxed) + 1;
                    emit_hash_progress(done as usize, total_full_files, tui_events);
                    if let Some(progress_bar) = &bar {
                        progress_bar.inc(1);
                    }
                    emit_worker_activity("hashing_full", worker, None, tui_events);

                    match digest_result {
                        Ok(hash) => hash,
                        Err(err) => {
                            full_hash_errors.fetch_add(1, Ordering::Relaxed);
                            if strict {
                                return Err(anyhow!(
                                    "full hash error for {}: {}",
                                    path.display(),
                                    err
                                ));
                            }
                            continue;
                        }
                    }
                };
                assign_hash(group, members, &hash);
            }
            emit_planned_entries(group, options);
            Ok(())
        })?;

    sync_cache(cache, groups);

    warnings.sample_hash_errors += sample_hash_errors.load(Ordering::Relaxed);
    warnings.full_hash_errors += full_hash_errors.load(Ordering::Relaxed);
    if let Some(bar) = bar {
        bar.finish_with_message("Hashing complete ✔".green().bold().to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, dev: u64, inode: u64, mtime_ns: u128) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 10,
            mtime_ns,
            ctime_ns: 0,
            dev,
            inode,
            hash: None,
            in_downloads: path.starts_with("/dl"),
        }
    }

    fn entry_for(file: &FileInfo, hash: &str) -> CacheEntry {
        CacheEntry {
            size: file.size,
            mtime_ns: file.mtime_ns,
            ctime_ns: file.ctime_ns,
            dev: file.dev,
            inode: file.inode,
            hash: hash.to_string(),
        }
    }

    #[test]
    fn group_of_one_physical_file_needs_no_work() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 1, 7, 0)];
        assert!(plan_group_work(&group, &Cache::new()).is_none());
    }

    #[test]
    fn hardlinked_paths_are_read_once() {
        let group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/copy", 1, 9, 0),
        ];
        let work = plan_group_work(&group, &Cache::new()).expect("two physical files");
        assert_eq!(work.inodes, vec![vec![0, 1], vec![2]]);
        assert!(work.needs_sampling());
    }

    #[test]
    fn same_inode_on_another_device_is_a_different_file() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 2, 7, 0)];
        let work = plan_group_work(&group, &Cache::new()).expect("two physical files");
        assert_eq!(work.inodes.len(), 2);
    }

    #[test]
    fn cache_hit_on_any_path_covers_the_physical_file() {
        let group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/copy", 1, 9, 0),
        ];
        let mut cache = Cache::new();
        cache.insert("/media/a".into(), entry_for(&group[1], "h"));
        cache.insert("/dl/copy".into(), entry_for(&group[2], "h"));
        let work = plan_group_work(&group, &cache).expect("two physical files");
        assert_eq!(work.cached, vec![Some("h".into()), Some("h".into())]);
        assert!(!work.needs_sampling());
    }

    #[test]
    fn stale_cache_entry_is_ignored() {
        let group = vec![file("/dl/a", 1, 7, 5), file("/media/a", 1, 8, 0)];
        let mut cache = Cache::new();
        let stale = file("/dl/a", 1, 7, 4);
        cache.insert("/dl/a".into(), entry_for(&stale, "old"));
        let work = plan_group_work(&group, &cache).expect("two physical files");
        assert_eq!(work.cached[0], None);
    }

    #[test]
    fn sync_cache_records_every_hashed_path() {
        let mut group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/unhashed", 1, 9, 0),
        ];
        assign_hash(&mut group, &[0, 1], "h");
        let mut cache = Cache::new();
        let stale = file("/media/a", 1, 7, 99);
        cache.insert("/media/a".into(), entry_for(&stale, "old"));

        sync_cache(&mut cache, &[group.clone()]);

        assert_eq!(cache.len(), 2);
        assert!(cache_entry_matches(&cache["/dl/a"], &group[0]));
        assert!(cache_entry_matches(&cache["/media/a"], &group[1]));
        assert_eq!(cache["/media/a"].hash, "h");
    }
}
