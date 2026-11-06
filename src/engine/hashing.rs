use anyhow::{Context, Result, anyhow};
use blake3::Hasher;
use colored::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::current_thread_index;
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::cache::CacheKeys;
use super::cancel::{CancelToken, Cancelled, FirstError};
use super::linking::{plan_hashed_group_actions, snapshot_from_file_info};
use super::report::build_report_entries;
use super::types::{Cache, CacheEntry, FileInfo, LinkMode, RunWarnings};
use super::types::{emit_tui_event, progress_due};
use crate::logger;
use crate::protocol::TuiEvent;

/// Every path a hash opened, so a test can count reads under its own folder.
#[cfg(test)]
pub(super) mod read_log {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(in crate::engine) fn record(path: &Path) {
        OPENED.lock().expect("read log").push(path.to_path_buf());
    }

    /// How many times a hash opened a file under `root`.
    pub(in crate::engine) fn reads_under(root: &Path) -> usize {
        OPENED
            .lock()
            .expect("read log")
            .iter()
            .filter(|path| path.starts_with(root))
            .count()
    }
}

/// Bytes hashed from each end of a file for its quick hash. A file no bigger
/// than this is read whole, so its quick hash is its full BLAKE3.
pub(super) const SAMPLE_BYTES: u64 = 1 << 20;

fn sample_digest(path: &Path, size: u64) -> Result<[u8; 32]> {
    #[cfg(test)]
    read_log::record(path);
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    sample_digest_from(&mut file, size)
}

/// Bytes a full hash reads at a time. Each read is a round trip to a network
/// drive, so fewer, larger reads keep closer to its bandwidth.
const FULL_CHUNK: usize = 8 << 20;

thread_local! {
    /// One quick-hash buffer per worker, reused for every file it reads.
    static SAMPLE_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; SAMPLE_BYTES as usize]);
    /// One full-hash buffer per worker.
    static FULL_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; FULL_CHUNK]);
}

/// BLAKE3 of the first and last [`SAMPLE_BYTES`] of a file of `size` bytes,
/// or of all of it when it is no bigger. It reads exactly the bytes it
/// hashes: the size is known, so no read is spent finding the end of file.
fn sample_digest_from<R: Read + Seek>(reader: &mut R, size: u64) -> Result<[u8; 32]> {
    let mut hasher = Hasher::new();
    let want = size.min(SAMPLE_BYTES) as usize;
    SAMPLE_BUF.with_borrow_mut(|buf| -> Result<()> {
        let buf = &mut buf[..want];
        let n = read_up_to(reader, buf)?;
        hasher.update(&buf[..n]);
        if size > SAMPLE_BYTES {
            reader.seek(SeekFrom::Start(size - SAMPLE_BYTES))?;
            let n = read_up_to(reader, buf)?;
            hasher.update(&buf[..n]);
        }
        Ok(())
    })?;
    Ok(hasher.finalize().into())
}

/// Fill `buf` unless the reader hits end of file first, and return how many
/// bytes were read. A single `read` may return fewer bytes than asked for (FUSE
/// mounts such as mergerfs do), which would make identical files sample
/// differently.
fn read_up_to<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
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

/// Hashing progress for the TUI, paced except at a phase's start and end.
fn emit_hash_progress(done: usize, total: usize, tui_events: bool) {
    if tui_events && (done == 0 || done == total || progress_due()) {
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

/// Full BLAKE3 of a file of `size` bytes, checking for cancellation between
/// chunks so a cancel does not wait for a large file to finish.
pub(super) fn full_digest(path: &Path, size: u64, cancel: &CancelToken) -> Result<String> {
    #[cfg(test)]
    read_log::record(path);
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    full_digest_from(&mut file, size, cancel)
}

/// Hash exactly the `size` bytes the walk saw. The size is known, so no read
/// is spent finding the end of the file. A file that ends sooner changed
/// since the walk, and is an error rather than the hash of less.
fn full_digest_from<R: Read>(reader: &mut R, size: u64, cancel: &CancelToken) -> Result<String> {
    let mut hasher = Hasher::new();
    FULL_BUF.with_borrow_mut(|buf| -> Result<()> {
        let mut done = 0u64;
        while done < size {
            cancel.check()?;
            let want = (size - done).min(buf.len() as u64) as usize;
            let n = read_up_to(reader, &mut buf[..want])?;
            hasher.update(&buf[..n]);
            done += n as u64;
            if n < want {
                return Err(anyhow!(
                    "the file ended after {done} of the {size} bytes the walk found: it changed since, so scan again"
                ));
            }
        }
        Ok(())
    })?;
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) struct HashOptions<'a> {
    pub(super) strict: bool,
    pub(super) show_progress: bool,
    pub(super) tui_events: bool,
    pub(super) link_mode: LinkMode,
    /// The run's options, for which planned actions it was asked to take.
    pub(super) selection: &'a super::types::ScanRunOptions<'a>,
    pub(super) cancel: &'a CancelToken,
    /// Full reads run here; quick hashes run on the pool the caller installs.
    /// Full reads are limited by the drive's bandwidth and quick ones by its
    /// latency, so each kind gets its own number of workers.
    pub(super) full_pool: &'a rayon::ThreadPool,
    pub(super) keys: &'a CacheKeys,
    pub(super) checkpoint: Checkpoint<'a>,
}

/// One physical file in a size group: every path of it, and what is known of
/// its content. Paths that share dev, inode, mtime and ctime are one physical
/// file: only its first path is read, and what is learned is copied to the
/// rest. The timestamps guard against filesystems that can report one inode
/// for different files (mergerfs with inodecalc=passthrough); hardlinks always
/// share them.
#[derive(Debug)]
struct Physical {
    /// Indices into the group.
    members: Vec<usize>,
    /// Full BLAKE3, hex, from the cache or read this run.
    full: Option<String>,
    /// Quick hash, hex. For a file no bigger than the sample it equals `full`.
    sample: Option<String>,
}

/// Hashing work for one size group.
#[derive(Debug)]
struct GroupWork {
    files: Vec<Physical>,
    /// The group's files are no bigger than the sample, so a quick hash reads
    /// the whole file and is its full hash.
    small: bool,
}

impl GroupWork {
    /// Whether a physical file still lacks its full hash. When none does, the
    /// group is planned from the cache alone.
    fn needs_reading(&self) -> bool {
        self.files.iter().any(|file| file.full.is_none())
    }

    /// Record a quick hash read this run.
    fn set_sample(&mut self, idx: usize, sample: String) {
        let file = &mut self.files[idx];
        if self.small {
            file.full = Some(sample.clone());
        }
        file.sample = Some(sample);
    }

    /// Physical files to read in full: those without a full hash whose quick
    /// hash matches another file's in the group.
    fn full_candidates(&self) -> Vec<usize> {
        let mut by_sample: HashMap<&str, Vec<usize>> = HashMap::new();
        for (idx, file) in self.files.iter().enumerate() {
            if let Some(sample) = &file.sample {
                by_sample.entry(sample).or_default().push(idx);
            }
        }
        let mut candidates: Vec<usize> = by_sample
            .into_values()
            .filter(|indices| indices.len() > 1)
            .flatten()
            .filter(|&idx| self.files[idx].full.is_none())
            .collect();
        candidates.sort_unstable();
        candidates
    }

    /// Copy what is known of each physical file to every path of it.
    fn assign(&self, group: &mut [FileInfo]) {
        for file in &self.files {
            for &idx in &file.members {
                group[idx].hash = file.full.clone();
                group[idx].sample = if self.small {
                    None
                } else {
                    file.sample.clone()
                };
            }
        }
    }
}

fn split_by_inode(group: &[FileInfo]) -> Vec<Vec<usize>> {
    let mut index: HashMap<(u64, u64, u128, i128), usize> = HashMap::new();
    let mut inodes: Vec<Vec<usize>> = Vec::new();
    for (idx, file) in group.iter().enumerate() {
        match index.entry((file.dev, file.inode, file.mtime_ns, file.ctime_ns)) {
            Entry::Occupied(slot) => inodes[*slot.get()].push(idx),
            Entry::Vacant(slot) => {
                slot.insert(inodes.len());
                inodes.push(vec![idx]);
            }
        }
    }
    inodes
}

fn cache_entry_matches(entry: &CacheEntry, file: &FileInfo) -> bool {
    entry.snapshot() == snapshot_from_file_info(file)
}

/// The full and quick hashes the cache holds for a physical file, from any of
/// its paths whose entry is still valid.
fn cached_hashes(
    cache: &Cache,
    keys: &CacheKeys,
    group: &[FileInfo],
    members: &[usize],
) -> (Option<String>, Option<String>) {
    let (mut full, mut sample) = (None, None);
    for &idx in members {
        let file = &group[idx];
        if let Some(entry) = cache
            .get(&keys.key(&file.path, file.in_downloads))
            .filter(|entry| cache_entry_matches(entry, file))
        {
            full = full.or_else(|| entry.hash.clone());
            sample = sample.or_else(|| entry.sample.clone());
        }
    }
    (full, sample)
}

/// None when the group cannot produce an action, so nothing in it is read: it
/// lacks a download or a library file, every path is one physical file, or
/// every download already is a library file.
fn plan_group_work(group: &[FileInfo], cache: &Cache, keys: &CacheKeys) -> Option<GroupWork> {
    if !group.iter().any(|file| !file.in_downloads) {
        return None;
    }
    let inodes = split_by_inode(group);
    if inodes.len() < 2 {
        return None;
    }
    let download_only = |members: &Vec<usize>| members.iter().all(|&idx| group[idx].in_downloads);
    if !inodes.iter().any(download_only) {
        return None;
    }
    let small = group[0].size <= SAMPLE_BYTES;
    let files = inodes
        .into_iter()
        .map(|members| {
            let (mut full, mut sample) = cached_hashes(cache, keys, group, &members);
            if small {
                full = full.or(sample);
                sample = full.clone();
            }
            Physical {
                members,
                full,
                sample,
            }
        })
        .collect();
    Some(GroupWork { files, small })
}

/// Write what is known of `file` to its cache entry, keeping what a still
/// valid entry knows that this run did not learn. Returns whether the entry
/// changed.
fn upsert_entry(
    cache: &mut Cache,
    keys: &CacheKeys,
    file: &FileInfo,
    hash: Option<&String>,
    sample: Option<&String>,
) -> bool {
    if hash.is_none() && sample.is_none() {
        return false;
    }
    let key = keys.key(&file.path, file.in_downloads);
    let valid = cache
        .get(&key)
        .filter(|entry| cache_entry_matches(entry, file));
    let entry = CacheEntry {
        size: file.size,
        mtime_ns: file.mtime_ns,
        ctime_ns: file.ctime_ns,
        dev: file.dev,
        inode: file.inode,
        hash: hash
            .cloned()
            .or_else(|| valid.and_then(|entry| entry.hash.clone())),
        sample: sample
            .cloned()
            .or_else(|| valid.and_then(|entry| entry.sample.clone())),
    };
    if valid == Some(&entry) {
        return false;
    }
    cache.insert(key, entry);
    true
}

/// Write a cache entry for every path whose full or quick hash is known and
/// whose entry is missing, stale or knows less, so every path of a hardlinked
/// file is served from cache next run, not only the path that was read.
/// Returns how many entries were written.
fn sync_cache(cache: &mut Cache, keys: &CacheKeys, groups: &[Vec<FileInfo>]) -> usize {
    groups
        .iter()
        .flatten()
        .filter(|file| upsert_entry(cache, keys, file, file.hash.as_ref(), file.sample.as_ref()))
        .count()
}

/// How often hashing saves the cache while it runs.
pub(super) const CHECKPOINT_EVERY: Duration = Duration::from_secs(5 * 60);

/// Where and how often to save the cache while hashing runs.
pub(super) struct Checkpoint<'a> {
    pub(super) path: &'a Path,
    pub(super) every: Duration,
}

/// The cache while hashing runs: each hash goes in as it finishes, and the
/// cache is saved every [`Checkpoint::every`], so a run that is killed
/// outright (SIGKILL, a power cut, a container stopped without grace) loses
/// at most that much hashing. A run that is asked to stop saves at its end
/// as usual.
struct LiveCache<'c> {
    cache: Mutex<&'c mut Cache>,
    keys: &'c CacheKeys,
    checkpoint: &'c Checkpoint<'c>,
    last_save: Mutex<Instant>,
    writes: AtomicUsize,
}

impl<'c> LiveCache<'c> {
    fn new(cache: &'c mut Cache, keys: &'c CacheKeys, checkpoint: &'c Checkpoint<'c>) -> Self {
        LiveCache {
            cache: Mutex::new(cache),
            keys,
            checkpoint,
            last_save: Mutex::new(Instant::now()),
            writes: AtomicUsize::new(0),
        }
    }

    /// Record one physical file's hashes for every path of it, and save the
    /// cache if a checkpoint is due.
    fn record(
        &self,
        group: &[FileInfo],
        members: &[usize],
        hash: Option<&String>,
        sample: Option<&String>,
    ) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        for &idx in members {
            if upsert_entry(&mut cache, self.keys, &group[idx], hash, sample) {
                self.writes.fetch_add(1, Ordering::Relaxed);
            }
        }
        let Ok(mut last_save) = self.last_save.lock() else {
            return;
        };
        if last_save.elapsed() < self.checkpoint.every {
            return;
        }
        *last_save = Instant::now();
        logger::info("cache", "saving the cache while hashing runs");
        if let Err(err) = super::cache::save_cache(self.checkpoint.path, &cache) {
            logger::warn(
                "cache",
                format!(
                    "cannot save the cache to {} while hashing ({err:#}); it is saved again when the run ends",
                    self.checkpoint.path.display()
                ),
            );
        }
    }

    fn into_writes(self) -> usize {
        self.writes.into_inner()
    }
}

fn emit_planned_entries(group: &[FileInfo], options: &HashOptions<'_>) {
    if !options.tui_events {
        return;
    }
    let actions = plan_hashed_group_actions(group)
        .into_iter()
        .filter(|action| options.selection.selects(action))
        .collect::<Vec<_>>();
    let entries = build_report_entries(&actions, options.link_mode);
    if !entries.is_empty() {
        let _ = emit_tui_event(&TuiEvent::PlannedEntries { entries });
    }
}

/// What hashing did: how many cache entries it wrote, and whether it
/// finished. The entries are written either way.
pub(super) struct Hashed {
    pub(super) cache_writes: usize,
    pub(super) result: Result<()>,
}

pub(super) fn hash_groups_and_update_cache(
    groups: &mut [Vec<FileInfo>],
    cache: &mut Cache,
    options: &HashOptions<'_>,
    warnings: &mut RunWarnings,
) -> Hashed {
    let strict = options.strict;
    let tui_events = options.tui_events;
    let mut works: Vec<Option<GroupWork>> = groups
        .iter()
        .map(|group| plan_group_work(group, cache, options.keys))
        .collect();

    // Groups whose every physical file has a cached full hash need no reads.
    for (group, work) in groups.iter_mut().zip(&works) {
        if let Some(work) = work.as_ref().filter(|work| !work.needs_reading()) {
            work.assign(group);
            emit_planned_entries(group, options);
        }
    }

    // Whether each group is read this run, fixed before reading: reading the
    // quick hashes of small files completes their full hashes.
    let reading: Vec<bool> = works
        .iter()
        .map(|work| work.as_ref().is_some_and(GroupWork::needs_reading))
        .collect();
    let total_sample_files = works
        .iter()
        .flatten()
        .filter(|work| work.needs_reading())
        .map(|work| {
            work.files
                .iter()
                .filter(|file| file.sample.is_none())
                .count()
        })
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
    let stop = FirstError::default();
    let live = LiveCache::new(cache, options.keys, &options.checkpoint);

    // Phase 1: the quick hash of each physical file that has none yet. Work is
    // split per file, not per group, so one large group of same-size files
    // does not run on one worker.
    let sampled: Vec<Vec<(usize, String)>> = groups
        .par_iter()
        .zip(works.par_iter())
        .zip(reading.par_iter())
        .map(|((group, work), &reading)| {
            let Some(work) = work.as_ref().filter(|_| reading) else {
                return Vec::new();
            };
            work.files
                .par_iter()
                .enumerate()
                .filter(|(_, file)| file.sample.is_none())
                .filter_map(|(idx, physical)| {
                    if stop.should_stop(options.cancel) {
                        return None;
                    }
                    let worker = worker_id();
                    let file = &group[physical.members[0]];
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
                            let sample = blake3::Hash::from(digest).to_hex().to_string();
                            let (hash, kept_sample) = if work.small {
                                (Some(&sample), None)
                            } else {
                                (physical.full.as_ref(), Some(&sample))
                            };
                            live.record(group, &physical.members, hash, kept_sample);
                            Some((idx, sample))
                        }
                        Err(err) => {
                            sample_hash_errors.fetch_add(1, Ordering::Relaxed);
                            if strict {
                                stop.set(anyhow!(
                                    "sample hash error for {}: {err:#}. Check that DejaView can read it and nothing is still writing it, or run without --strict, which skips it with a warning",
                                    file.path.display()
                                ));
                            }
                            None
                        }
                    }
                })
                .collect()
        })
        .collect();
    for (work, sampled) in works.iter_mut().zip(sampled) {
        if let Some(work) = work.as_mut() {
            for (idx, sample) in sampled {
                work.set_sample(idx, sample);
            }
        }
    }

    let full_candidates: Vec<Vec<usize>> = works
        .iter()
        .map(|work| {
            work.as_ref()
                .filter(|_| !stop.is_set())
                .map(GroupWork::full_candidates)
                .unwrap_or_default()
        })
        .collect();
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

    // Phase 2: the full hash of each physical file whose quick hash collides
    // with another's, again split per file. A group's rows go to the TUI as
    // soon as its own files are done.
    options.full_pool.install(|| {
        groups
            .par_iter_mut()
            .zip(works.par_iter_mut())
            .zip(full_candidates.into_par_iter())
            .zip(reading.into_par_iter())
            .for_each(|(((group, work), candidates), reading)| {
                let Some(work) = work.as_mut().filter(|_| reading) else {
                    return;
                };
                let hashed: Vec<(usize, String)> = candidates
                    .par_iter()
                    .filter_map(|&idx| {
                        if stop.should_stop(options.cancel) {
                            return None;
                        }
                        let worker = worker_id();
                        let file = &group[work.files[idx].members[0]];
                        let path = &file.path;
                        emit_worker_activity(
                            "hashing_full",
                            worker,
                            Some(path.display().to_string()),
                            tui_events,
                        );
                        let digest_result = full_digest(path, file.size, options.cancel);
                        let done = full_done.fetch_add(1, Ordering::Relaxed) + 1;
                        emit_hash_progress(done as usize, total_full_files, tui_events);
                        if let Some(progress_bar) = &bar {
                            progress_bar.inc(1);
                        }
                        emit_worker_activity("hashing_full", worker, None, tui_events);
                        match digest_result {
                            Ok(hash) => {
                                let physical = &work.files[idx];
                                live.record(
                                    group,
                                    &physical.members,
                                    Some(&hash),
                                    physical.sample.as_ref(),
                                );
                                Some((idx, hash))
                            }
                            Err(err) if err.is::<Cancelled>() => {
                                stop.set(err);
                                None
                            }
                            Err(err) => {
                                full_hash_errors.fetch_add(1, Ordering::Relaxed);
                                if strict {
                                    stop.set(anyhow!(
                                        "full hash error for {}: {err:#}. Check that DejaView can read it and nothing is still writing it, or run without --strict, which skips it with a warning",
                                        path.display()
                                    ));
                                }
                                None
                            }
                        }
                    })
                    .collect();
                for (idx, hash) in hashed {
                    work.files[idx].full = Some(hash);
                }
                work.assign(group);
                if !stop.is_set() {
                    emit_planned_entries(group, options);
                }
            });
    });

    // Hashes finished before a cancel or a strict failure are still valid.
    let mut cache_writes = live.into_writes();
    cache_writes += sync_cache(cache, options.keys, groups);

    warnings.sample_hash_errors += sample_hash_errors.load(Ordering::Relaxed);
    warnings.full_hash_errors += full_hash_errors.load(Ordering::Relaxed);
    if let Some(bar) = bar {
        bar.finish_with_message("Hashing complete ✔".green().bold().to_string());
    }

    Hashed {
        cache_writes,
        result: stop.into_result(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(path: &str, dev: u64, inode: u64, mtime_ns: u128) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 10,
            mtime_ns,
            ctime_ns: 0,
            dev,
            inode,
            hash: None,
            sample: None,
            in_downloads: path.starts_with("/dl"),
        }
    }

    /// A file bigger than the sample, so its quick hash is not its full hash.
    fn large(path: &str, inode: u64) -> FileInfo {
        FileInfo {
            size: SAMPLE_BYTES + 1,
            ..file(path, 1, inode, 0)
        }
    }

    fn entry_for(file: &FileInfo, hash: Option<&str>, sample: Option<&str>) -> CacheEntry {
        CacheEntry {
            size: file.size,
            mtime_ns: file.mtime_ns,
            ctime_ns: file.ctime_ns,
            dev: file.dev,
            inode: file.inode,
            hash: hash.map(str::to_string),
            sample: sample.map(str::to_string),
        }
    }

    fn keys() -> CacheKeys {
        CacheKeys::new(Path::new("/dl"), Path::new("/media"))
    }

    fn key(file: &FileInfo) -> String {
        keys().key(&file.path, file.in_downloads)
    }

    fn members(work: &GroupWork) -> Vec<Vec<usize>> {
        work.files.iter().map(|file| file.members.clone()).collect()
    }

    /// Physical files phase 1 would read.
    fn to_sample(work: &GroupWork) -> usize {
        work.files
            .iter()
            .filter(|file| file.sample.is_none())
            .count()
    }

    /// Returns at most `max` bytes per read, as a FUSE mount such as mergerfs
    /// with direct I/O can.
    struct ShortReads<R> {
        inner: R,
        max: usize,
    }

    impl<R: Read> Read for ShortReads<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let len = buf.len().min(self.max);
            self.inner.read(&mut buf[..len])
        }
    }

    impl<R: Seek> Seek for ShortReads<R> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn sample_digest_is_the_same_when_reads_come_back_short() {
        // Bigger than the 1 MiB sample so both the head and the tail are read.
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let size = data.len() as u64;
        let whole = sample_digest_from(&mut std::io::Cursor::new(data.clone()), size)
            .expect("digest with full reads");
        let mut short = ShortReads {
            inner: std::io::Cursor::new(data),
            max: 7,
        };
        let chunked = sample_digest_from(&mut short, size).expect("digest with short reads");
        assert_eq!(whole, chunked);
    }

    /// Fails the first read with EINTR, then reads normally.
    struct InterruptedOnce<R> {
        inner: R,
        interrupted: bool,
    }

    impl<R: Read> Read for InterruptedOnce<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            self.inner.read(buf)
        }
    }

    /// Counts the reads that reach it.
    struct CountedReads<R> {
        inner: R,
        reads: usize,
    }

    impl<R: Read> Read for CountedReads<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.inner.read(buf)
        }
    }

    impl<R: Seek> Seek for CountedReads<R> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn a_quick_hash_reads_only_the_bytes_it_hashes() {
        for size in [4096usize, SAMPLE_BYTES as usize, 3 * SAMPLE_BYTES as usize] {
            let data: Vec<u8> = (0..size).map(|i| (i % 253) as u8).collect();
            let mut counted = CountedReads {
                inner: std::io::Cursor::new(data.clone()),
                reads: 0,
            };
            let digest = sample_digest_from(&mut counted, size as u64).expect("digest");
            let expected_reads = if size as u64 > SAMPLE_BYTES { 2 } else { 1 };
            assert_eq!(counted.reads, expected_reads, "size {size}");
            if size as u64 <= SAMPLE_BYTES {
                // Read whole, so it is the file's full BLAKE3.
                assert_eq!(digest, *blake3::hash(&data).as_bytes());
            }
        }
    }

    #[test]
    fn a_full_hash_reads_exactly_the_size_the_walk_found() {
        let size = 2 * FULL_CHUNK + 4096;
        let data: Vec<u8> = (0..size).map(|i| (i % 241) as u8).collect();
        let mut counted = CountedReads {
            inner: std::io::Cursor::new(data.clone()),
            reads: 0,
        };
        let hash = full_digest_from(&mut counted, size as u64, &CancelToken::default())
            .expect("full hash");
        assert_eq!(hash, blake3::hash(&data).to_hex().to_string());
        assert_eq!(counted.reads, 3, "no read is spent finding the end");

        // A file shorter than the walk saw changed since: an error, not the
        // hash of what is left.
        let short = full_digest_from(
            &mut std::io::Cursor::new(data),
            size as u64 + 1,
            &CancelToken::default(),
        );
        assert!(short.is_err());
    }

    #[test]
    fn read_up_to_retries_an_interrupted_read() {
        let mut reader = InterruptedOnce {
            inner: std::io::Cursor::new(vec![9u8; 10]),
            interrupted: false,
        };
        let mut buf = [0u8; 16];
        assert_eq!(read_up_to(&mut reader, &mut buf).expect("read"), 10);
        assert_eq!(&buf[..10], &[9u8; 10]);
    }

    #[test]
    fn group_of_one_physical_file_needs_no_work() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 1, 7, 0)];
        assert!(plan_group_work(&group, &Cache::new(), &keys()).is_none());
    }

    #[test]
    fn group_without_a_library_file_is_not_read() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/dl/b", 1, 8, 0)];
        assert!(plan_group_work(&group, &Cache::new(), &keys()).is_none());
    }

    #[test]
    fn group_without_a_download_is_not_read() {
        let group = vec![file("/media/a", 1, 7, 0), file("/media/b", 1, 8, 0)];
        assert!(plan_group_work(&group, &Cache::new(), &keys()).is_none());
    }

    #[test]
    fn group_whose_downloads_all_are_library_files_is_not_read() {
        // The download is a hardlink of one library file; the other library
        // file can only be compared with the first, which plans nothing.
        let group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/media/b", 1, 8, 0),
        ];
        assert!(plan_group_work(&group, &Cache::new(), &keys()).is_none());
    }

    #[test]
    fn hardlinked_paths_are_read_once() {
        let group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/copy", 1, 9, 0),
        ];
        let work = plan_group_work(&group, &Cache::new(), &keys()).expect("two physical files");
        assert_eq!(members(&work), vec![vec![0, 1], vec![2]]);
        assert!(work.needs_reading());
        assert_eq!(to_sample(&work), 2);
    }

    #[test]
    fn same_inode_on_another_device_is_a_different_file() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 2, 7, 0)];
        let work = plan_group_work(&group, &Cache::new(), &keys()).expect("two physical files");
        assert_eq!(work.files.len(), 2);
    }

    #[test]
    fn colliding_inode_with_different_times_is_not_shared() {
        // mergerfs inodecalc=passthrough can report one inode for different files on
        // different branches; a real hardlink always shares its timestamps too.
        let mut other = file("/media/b", 1, 7, 5);
        other.ctime_ns = 5;
        let group = vec![file("/dl/a", 1, 7, 0), other];
        let work = plan_group_work(&group, &Cache::new(), &keys()).expect("two physical files");
        assert_eq!(work.files.len(), 2);
    }

    #[test]
    fn cache_hit_on_any_path_covers_the_physical_file() {
        let group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/copy", 1, 9, 0),
        ];
        let mut cache = Cache::new();
        cache.insert(key(&group[1]), entry_for(&group[1], Some("h"), None));
        cache.insert(key(&group[2]), entry_for(&group[2], Some("h"), None));
        let work = plan_group_work(&group, &cache, &keys()).expect("two physical files");
        let full: Vec<_> = work.files.iter().map(|file| file.full.clone()).collect();
        assert_eq!(full, vec![Some("h".into()), Some("h".into())]);
        assert!(!work.needs_reading());
    }

    #[test]
    fn stale_cache_entry_is_ignored() {
        let group = vec![file("/dl/a", 1, 7, 5), file("/media/a", 1, 8, 0)];
        let mut cache = Cache::new();
        let stale = file("/dl/a", 1, 7, 4);
        cache.insert(key(&stale), entry_for(&stale, Some("old"), None));
        let work = plan_group_work(&group, &cache, &keys()).expect("two physical files");
        assert_eq!(work.files[0].full, None);
        assert_eq!(to_sample(&work), 2);
    }

    #[test]
    fn a_small_file_s_quick_hash_is_its_full_hash() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 1, 8, 0)];
        let mut work = plan_group_work(&group, &Cache::new(), &keys()).expect("two physical files");
        work.set_sample(0, "h".into());
        work.set_sample(1, "h".into());
        assert!(!work.needs_reading());
        assert!(work.full_candidates().is_empty());
    }

    #[test]
    fn a_small_file_s_cached_full_hash_needs_no_quick_hash() {
        let group = vec![file("/dl/a", 1, 7, 0), file("/media/a", 1, 8, 0)];
        let mut cache = Cache::new();
        cache.insert(key(&group[1]), entry_for(&group[1], Some("h"), None));
        let work = plan_group_work(&group, &cache, &keys()).expect("two physical files");
        assert_eq!(to_sample(&work), 1);
        assert_eq!(work.files[1].sample.as_deref(), Some("h"));
    }

    #[test]
    fn cached_quick_hashes_that_match_nothing_need_no_reads() {
        // A new download whose quick hash matched no library file last run:
        // neither file is read again while both stay unchanged.
        let group = vec![large("/dl/a", 7), large("/media/a", 8)];
        let mut cache = Cache::new();
        cache.insert(key(&group[0]), entry_for(&group[0], None, Some("s1")));
        cache.insert(key(&group[1]), entry_for(&group[1], None, Some("s2")));
        let work = plan_group_work(&group, &cache, &keys()).expect("two physical files");
        assert!(work.needs_reading());
        assert_eq!(to_sample(&work), 0);
        assert!(work.full_candidates().is_empty());
    }

    #[test]
    fn only_files_whose_quick_hashes_collide_are_read_in_full() {
        let group = vec![
            large("/dl/a", 7),
            large("/media/a", 8),
            large("/media/b", 9),
        ];
        let mut cache = Cache::new();
        cache.insert(key(&group[1]), entry_for(&group[1], Some("f"), Some("s")));
        let mut work = plan_group_work(&group, &cache, &keys()).expect("three physical files");
        assert_eq!(to_sample(&work), 2);
        work.set_sample(0, "s".into());
        work.set_sample(2, "other".into());
        assert_eq!(work.full_candidates(), vec![0]);
    }

    #[test]
    fn sync_cache_records_every_hashed_path() {
        let mut group = vec![
            file("/dl/a", 1, 7, 0),
            file("/media/a", 1, 7, 0),
            file("/dl/unhashed", 1, 9, 0),
        ];
        group[0].hash = Some("h".into());
        group[1].hash = Some("h".into());
        let mut cache = Cache::new();
        let stale = file("/media/a", 1, 7, 99);
        cache.insert(key(&stale), entry_for(&stale, Some("old"), None));

        assert_eq!(
            sync_cache(&mut cache, &keys(), std::slice::from_ref(&group)),
            2
        );

        assert_eq!(cache.len(), 2);
        assert!(cache_entry_matches(&cache[&key(&group[0])], &group[0]));
        assert!(cache_entry_matches(&cache[&key(&group[1])], &group[1]));
        assert_eq!(cache[&key(&group[1])].hash.as_deref(), Some("h"));
        assert_eq!(sync_cache(&mut cache, &keys(), &[group]), 0);
    }

    #[test]
    fn sync_cache_keeps_what_an_entry_knows_when_this_run_learned_less() {
        let mut group = vec![large("/dl/a", 7)];
        group[0].hash = Some("f".into());
        let mut cache = Cache::new();
        cache.insert(key(&group[0]), entry_for(&group[0], Some("f"), Some("s")));

        let path_key = key(&group[0]);
        assert_eq!(sync_cache(&mut cache, &keys(), &[group]), 0);
        assert_eq!(cache[&path_key].sample.as_deref(), Some("s"));
    }
}
