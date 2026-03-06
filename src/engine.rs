use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use colored::*;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, ContentArrangement, Table};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use pathdiff::diff_paths;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use walkdir::{DirEntry, WalkDir};

use crate::config::{cache_path, Config};

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
    dev: u64,
    inode: u64,
    hash: Option<String>,
    in_downloads: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    hash: String,
}

type Cache = HashMap<String, CacheEntry>;

#[derive(Default)]
struct RunWarnings {
    walk_errors: u64,
    metadata_errors: u64,
    sample_hash_errors: u64,
    full_hash_errors: u64,
    link_errors: u64,
}

#[derive(Clone)]
struct LinkAction {
    src: PathBuf,
    tgt: PathBuf,
    size: u64,
}

fn is_regular(entry: &DirEntry) -> bool {
    entry.file_type().is_file() && !entry.path_is_symlink()
}

fn collect_files(
    root: &Path,
    in_downloads: bool,
    min_age: Duration,
    strict: bool,
    warnings: &mut RunWarnings,
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
        if mtime > deadline {
            continue;
        }
        files.push(FileInfo {
            path: entry.into_path(),
            size: meta.len(),
            mtime,
            dev: meta.dev(),
            inode: meta.ino(),
            hash: None,
            in_downloads,
        });
    }
    Ok(files)
}

fn load_cache(path: &Path) -> Cache {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_cache(path: &Path, cache: &Cache) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("cache path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;

    let tmp_path = parent.join(format!(
        ".dejaview_cache.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let json = serde_json::to_vec(cache)?;

    let write_result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

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

fn sample_digest(path: &Path, size: u64) -> Result<[u8; 32]> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
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
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
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

fn ensure_target_within_media_root(media_root: &Path, target: &Path) -> Result<()> {
    let canonical_media = fs::canonicalize(media_root)
        .with_context(|| format!("canonicalize {}", media_root.display()))?;
    let canonical_target =
        fs::canonicalize(target).with_context(|| format!("canonicalize {}", target.display()))?;

    if !canonical_target.starts_with(&canonical_media) {
        return Err(anyhow!(
            "refusing to link to target outside media root: {}",
            canonical_target.display()
        ));
    }

    Ok(())
}

fn replace_file_with_relative_symlink(src: &Path, tgt: &Path) -> Result<()> {
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", src.display()))?;
    let rel = diff_paths(tgt, parent).context("failed to compute relative path")?;
    let tmp_link = parent.join(format!(
        ".dejaview_link.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let link_result = (|| -> Result<()> {
        symlink(&rel, &tmp_link)
            .with_context(|| format!("ln -s {:?} {:?}", rel, tmp_link.display()))?;
        fs::rename(&tmp_link, src)
            .with_context(|| format!("mv {:?} {:?}", tmp_link.display(), src.display()))?;
        Ok(())
    })();

    if link_result.is_err() {
        let _ = fs::remove_file(&tmp_link);
    }

    link_result
}

fn clear_cache_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove cache file {}", path.display())),
    }
}

pub fn cache_info(config: &Config) -> Result<(PathBuf, usize)> {
    let path = cache_path(config)?;
    let cache = load_cache(&path);
    Ok((path, cache.len()))
}

pub fn clear_cache(config: &Config) -> Result<()> {
    let path = cache_path(config)?;
    clear_cache_file(&path)
}

fn plan_group_actions(group: &[&FileInfo]) -> Vec<LinkAction> {
    let mut downloads: Vec<&FileInfo> = group.iter().copied().filter(|f| f.in_downloads).collect();
    let mut media: Vec<&FileInfo> = group.iter().copied().filter(|f| !f.in_downloads).collect();

    if downloads.is_empty() || media.is_empty() {
        return Vec::new();
    }

    downloads.sort_by(|a, b| a.path.cmp(&b.path));
    media.sort_by(|a, b| a.path.cmp(&b.path));

    let canonical = media[0];
    let mut actions = Vec::new();
    for src in downloads {
        if src.dev == canonical.dev && src.inode == canonical.inode {
            continue;
        }
        actions.push(LinkAction {
            src: src.path.clone(),
            tgt: canonical.path.clone(),
            size: src.size,
        });
    }

    actions
}

fn print_run_summary(
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

    let warning_total = warnings.walk_errors
        + warnings.metadata_errors
        + warnings.sample_hash_errors
        + warnings.full_hash_errors
        + warnings.link_errors;
    if warning_total > 0 {
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

pub fn run_scan(config: &Config, dry_run: bool, strict: bool) -> Result<()> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    let media = config
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("Media path not set"))?;
    let min_age = Duration::from_secs(config.min_age.unwrap_or(300));

    let mp = MultiProgress::new();
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(120));
    spinner.set_message("Collecting files…".cyan().bold().to_string());

    let mut warnings = RunWarnings::default();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();

    let mut files = collect_files(downloads, true, min_age, strict, &mut warnings)?;
    for f in &files {
        seen.insert((f.dev, f.inode));
    }

    let mut media_files = collect_files(media, false, min_age, strict, &mut warnings)?;
    media_files.retain(|f| !seen.contains(&(f.dev, f.inode)));
    files.extend(media_files);
    let files_seen = files.len();

    spinner.finish_with_message("File list complete ✔".green().bold().to_string());

    let cache_path = cache_path(config)?;
    let cache = Arc::new(Mutex::new(load_cache(&cache_path)));
    let inode_hash: Arc<Mutex<HashMap<(u64, u64), String>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files {
        size_map.entry(f.size).or_default().push(f);
    }

    let mut groups: Vec<&mut Vec<FileInfo>> =
        size_map.values_mut().filter(|v| v.len() > 1).collect();
    let bar = mp.add(ProgressBar::new(groups.len() as u64));
    bar.set_style(
        ProgressStyle::with_template("{msg} {pos}/{len} [{wide_bar:.cyan/blue}] {elapsed}")
            .unwrap(),
    );
    bar.set_message("Hashing groups".bold().to_string());

    for group in &mut groups {
        let mut sample_hashes: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, fi) in group.iter().enumerate() {
            match sample_digest(&fi.path, fi.size) {
                Ok(h) => {
                    let hex_hash = hex::encode(h);
                    sample_hashes.entry(hex_hash).or_default().push(i);
                }
                Err(err) => {
                    warnings.sample_hash_errors += 1;
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

        for (_, indices) in sample_hashes.iter().filter(|(_, v)| v.len() > 1) {
            for &i in indices {
                let fi = &mut group[i];
                let key = fi.path.to_string_lossy().into_owned();
                if let Ok(c) = cache.lock() {
                    if let Some(e) = c.get(&key) {
                        if e.size == fi.size
                            && e.mtime
                                == fi
                                    .mtime
                                    .duration_since(SystemTime::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs()
                        {
                            fi.hash = Some(e.hash.clone());
                            continue;
                        }
                    }
                }
                if let Ok(ih) = inode_hash.lock() {
                    if let Some(h) = ih.get(&(fi.dev, fi.inode)) {
                        fi.hash = Some(h.clone());
                        continue;
                    }
                }
                match full_digest(&fi.path) {
                    Ok(h) => {
                        if let Ok(mut ih) = inode_hash.lock() {
                            ih.insert((fi.dev, fi.inode), h.clone());
                        }
                        fi.hash = Some(h.clone());
                        if let Ok(mut c) = cache.lock() {
                            c.insert(
                                key,
                                CacheEntry {
                                    size: fi.size,
                                    mtime: fi
                                        .mtime
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .unwrap()
                                        .as_secs(),
                                    hash: h,
                                },
                            );
                        }
                    }
                    Err(err) => {
                        warnings.full_hash_errors += 1;
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
        bar.inc(1);
    }
    bar.finish_with_message("Hashing complete ✔".green().bold().to_string());

    if let Ok(c) = Arc::try_unwrap(cache) {
        save_cache(&cache_path, &c.into_inner().unwrap())?;
    }

    let mut hash_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
    for group in &groups {
        for fi in group.iter() {
            if let Some(ref h) = fi.hash {
                hash_map.entry(h.clone()).or_default().push(fi);
            }
        }
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download".yellow(), "Media".yellow()]);

    let mut actions = Vec::new();
    let mut total_saved_space: u64 = 0;

    for group in hash_map.values() {
        let planned = plan_group_actions(group);
        for action in planned {
            table.add_row(vec![
                action.src.to_string_lossy().to_string(),
                action.tgt.to_string_lossy().to_string(),
            ]);
            total_saved_space += action.size;
            actions.push(action);
        }
    }

    println!("\n{} Found duplicates:", "▶".cyan());
    println!("{}", table);
    if actions.is_empty() {
        println!("{} No actions needed.", "✔".green());
        return Ok(());
    }

    println!(
        "{} Potential space saved: {}",
        "💾".cyan(),
        format_size(total_saved_space)
    );
    print_run_summary(files_seen, actions.len(), 0, &warnings);

    if dry_run {
        println!("{} Dry-run; no changes.", "ℹ".yellow());
        if strict
            && (warnings.walk_errors
                + warnings.metadata_errors
                + warnings.sample_hash_errors
                + warnings.full_hash_errors
                + warnings.link_errors
                > 0)
        {
            return Err(anyhow!("strict mode failed due to scan/hash warnings"));
        }
        return Ok(());
    }

    println!("{} Applying…", "▶".cyan());
    let mut applied_actions = 0usize;
    let planned_actions = actions.len();
    for action in actions {
        let apply_result = (|| -> Result<()> {
            ensure_target_within_media_root(media, &action.tgt)?;
            replace_file_with_relative_symlink(&action.src, &action.tgt)?;
            Ok(())
        })();
        if let Err(err) = apply_result {
            warnings.link_errors += 1;
            if strict {
                return Err(anyhow!(
                    "strict mode link failure for {} -> {}: {}",
                    action.src.display(),
                    action.tgt.display(),
                    err
                ));
            }
            eprintln!(
                "{} Failed to apply {} -> {}: {}",
                "⚠".yellow(),
                action.src.display(),
                action.tgt.display(),
                err
            );
            continue;
        }
        applied_actions += 1;
    }
    print_run_summary(files_seen, planned_actions, applied_actions, &warnings);
    println!("{} Done.", "✔".green());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("dejaview_{prefix}_{}_{}", std::process::id(), ts))
    }

    #[test]
    fn allows_target_inside_media_root() {
        let root = unique_temp_dir("inside");
        let media = root.join("media");
        let nested = media.join("movies");
        let target = nested.join("a.mkv");
        fs::create_dir_all(&nested).expect("create media dir");
        fs::write(&target, b"content").expect("write target");

        let result = ensure_target_within_media_root(&media, &target);
        assert!(result.is_ok());

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn rejects_target_outside_media_root() {
        let root = unique_temp_dir("outside");
        let media = root.join("media");
        let other = root.join("other");
        let target = other.join("b.mkv");
        fs::create_dir_all(&media).expect("create media dir");
        fs::create_dir_all(&other).expect("create other dir");
        fs::write(&target, b"content").expect("write target");

        let result = ensure_target_within_media_root(&media, &target);
        assert!(result.is_err());

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn replaces_file_with_relative_symlink_atomically() {
        let root = unique_temp_dir("replace");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads dir");
        fs::create_dir_all(&media).expect("create media dir");

        let src = downloads.join("duplicate.mkv");
        let tgt = media.join("canonical.mkv");
        fs::write(&src, b"old").expect("write source");
        fs::write(&tgt, b"new").expect("write target");

        replace_file_with_relative_symlink(&src, &tgt).expect("replace with symlink");

        let meta = fs::symlink_metadata(&src).expect("metadata");
        assert!(meta.file_type().is_symlink());
        assert_eq!(
            fs::read_link(&src).expect("read link"),
            PathBuf::from("../media/canonical.mkv")
        );
        assert_eq!(fs::read(&src).expect("read through link"), b"new");

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn clear_cache_file_is_idempotent() {
        let root = unique_temp_dir("cache_clear");
        fs::create_dir_all(&root).expect("create temp dir");
        let cache = root.join(".dejaview_cache.json");

        clear_cache_file(&cache).expect("first clear on missing file");
        fs::write(&cache, b"{}").expect("write cache");
        clear_cache_file(&cache).expect("second clear removes file");
        clear_cache_file(&cache).expect("third clear still ok");

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn save_cache_writes_json() {
        let root = unique_temp_dir("save_cache");
        fs::create_dir_all(&root).expect("create temp dir");
        let cache_path = root.join(".dejaview_cache.json");
        let mut cache = Cache::new();
        cache.insert(
            "foo".into(),
            CacheEntry {
                size: 42,
                mtime: 123,
                hash: "abc".into(),
            },
        );

        save_cache(&cache_path, &cache).expect("save cache");
        let loaded = load_cache(&cache_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get("foo").expect("foo key").hash, "abc");

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn plan_group_actions_is_deterministic() {
        let later_media = FileInfo {
            path: PathBuf::from("/media/z-last.mkv"),
            size: 10,
            mtime: SystemTime::UNIX_EPOCH,
            dev: 1,
            inode: 2,
            hash: Some("h".into()),
            in_downloads: false,
        };
        let first_media = FileInfo {
            path: PathBuf::from("/media/a-first.mkv"),
            size: 10,
            mtime: SystemTime::UNIX_EPOCH,
            dev: 1,
            inode: 3,
            hash: Some("h".into()),
            in_downloads: false,
        };
        let download_b = FileInfo {
            path: PathBuf::from("/downloads/b.mkv"),
            size: 10,
            mtime: SystemTime::UNIX_EPOCH,
            dev: 2,
            inode: 4,
            hash: Some("h".into()),
            in_downloads: true,
        };
        let download_a = FileInfo {
            path: PathBuf::from("/downloads/a.mkv"),
            size: 10,
            mtime: SystemTime::UNIX_EPOCH,
            dev: 2,
            inode: 5,
            hash: Some("h".into()),
            in_downloads: true,
        };
        let group = vec![&later_media, &download_b, &first_media, &download_a];

        let actions = plan_group_actions(&group);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].src, PathBuf::from("/downloads/a.mkv"));
        assert_eq!(actions[1].src, PathBuf::from("/downloads/b.mkv"));
        assert_eq!(actions[0].tgt, PathBuf::from("/media/a-first.mkv"));
        assert_eq!(actions[1].tgt, PathBuf::from("/media/a-first.mkv"));
    }

    #[test]
    fn collect_files_strict_fails_on_walk_error() {
        let missing = PathBuf::from("/this/path/does/not/exist/dejaview");
        let mut warnings = RunWarnings::default();
        let result = collect_files(&missing, true, Duration::from_secs(0), true, &mut warnings);
        assert!(result.is_err());
        assert!(warnings.walk_errors > 0);
    }
}
