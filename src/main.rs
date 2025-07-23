use anyhow::{Context, Result};
use clap::{ArgAction, Parser};
use colored::*;
use comfy_table::{presets::UTF8_FULL, modifiers::UTF8_ROUND_CORNERS, ContentArrangement, Table};
use hex;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use pathdiff::diff_paths;

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use walkdir::{DirEntry, WalkDir};

/// DejaView – fast duplicate detector that swaps download copies for symlinks.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Path to the download tree (torrents seed here)
    #[arg(short = 'd', long)]
    downloads: PathBuf,

    /// Path to the organised media library
    #[arg(short = 'm', long)]
    media: PathBuf,

    /// Perform a dry run – no deletions or symlinks are written
    #[arg(short = 'n', long, action = ArgAction::SetTrue)]
    dry_run: bool,

    /// Override number of Rayon threads (default = #physical CPUs)
    #[arg(long)]
    threads: Option<usize>,

    /// Ignore files modified in the last N seconds (skip active downloads)
    #[arg(long, default_value_t = 300)]
    min_age: u64,
}

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

type Cache = HashMap<String, CacheEntry>; // key = absolute path

const CACHE_FILE: &str = ".dejaview_cache.json";

// ---- helpers --------------------------------------------------------------
fn is_regular(entry: &DirEntry) -> bool {
    entry.file_type().is_file() && !entry.path_is_symlink()
}

fn collect_files(root: &Path, in_downloads: bool, min_age: Duration) -> Result<Vec<FileInfo>> {
    let deadline = SystemTime::now() - min_age;
    let files: Vec<FileInfo> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(is_regular)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime > deadline {
                return None;
            }
            Some(FileInfo {
                path: entry.into_path(),
                size: meta.len(),
                mtime,
                dev: meta.dev(),
                inode: meta.ino(),
                hash: None,
                in_downloads,
            })
        })
        .collect();
    Ok(files)
}

fn load_cache(root: &Path) -> Cache {
    let cache_path = root.join(CACHE_FILE);
    match fs::read_to_string(&cache_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_cache(root: &Path, cache: &Cache) {
    let cache_path = root.join(CACHE_FILE);
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = fs::File::create(cache_path).and_then(|mut f| f.write_all(json.as_bytes()));
    }
}

// fast 2‑chunk hash (first & last 1 MiB)
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    // tune thread‑pool if requested
    if let Some(t) = cli.threads {
        rayon::ThreadPoolBuilder::new().num_threads(t).build_global()?;
    }

    // -------- Progress bars --------
    let mp = MultiProgress::new();
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(ProgressStyle::default_spinner().template("{spinner} {msg}").unwrap());
    spinner.enable_steady_tick(Duration::from_millis(120));
    spinner.set_message("Collecting files…".cyan().bold().to_string());

    // -------- Gather files --------
    let min_age = Duration::from_secs(cli.min_age);
    let mut seen: HashSet<(u64, u64)> = HashSet::new();

    let mut files = collect_files(&cli.downloads, true, min_age)?;
    for f in &files {
        seen.insert((f.dev, f.inode));
    }

    let mut media_files = collect_files(&cli.media, false, min_age)?;
    media_files.retain(|f| !seen.contains(&(f.dev, f.inode)));
    files.extend(media_files);

    spinner.finish_with_message("File list complete ✔".green().bold().to_string());

    // -------- Hashing phase --------
    let cache = Arc::new(Mutex::new(load_cache(&cli.downloads)));
    let inode_hash: Arc<Mutex<HashMap<(u64, u64), String>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files {
        size_map.entry(f.size).or_default().push(f);
    }

    let mut groups: Vec<&mut Vec<FileInfo>> = size_map.values_mut().filter(|v| v.len() > 1).collect();
    let bar = mp.add(ProgressBar::new(groups.len() as u64));
    bar.set_style(ProgressStyle::with_template("{msg} {pos}/{len} [{wide_bar:.cyan/blue}] {elapsed}").unwrap());
    bar.set_message("Hashing groups".bold().to_string());

    for group in &mut groups {
        // Phase 1: Fast sample hash
        let mut sample_hashes: HashMap<String, Vec<usize>> = HashMap::new();
        group.iter().enumerate().for_each(|(i, fi)| {
            if let Ok(h) = sample_digest(&fi.path, fi.size) {
                let hex_hash = hex::encode(h);
                sample_hashes.entry(hex_hash).or_default().push(i);
            }
        });

        // Phase 2: Full hash for sample-hash collisions
        for (_, indices) in sample_hashes.iter().filter(|(_, v)| v.len() > 1) {
            for &i in indices {
                let fi = &mut group[i];
                let key = fi.path.to_string_lossy().into_owned();
                if let Ok(c) = cache.lock() {
                    if let Some(e) = c.get(&key) {
                        if e.size == fi.size && e.mtime == fi.mtime.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() {
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
                if let Ok(h) = full_digest(&fi.path) {
                    if let Ok(mut ih) = inode_hash.lock() {
                        ih.insert((fi.dev, fi.inode), h.clone());
                    }
                    fi.hash = Some(h.clone());
                    if let Ok(mut c) = cache.lock() {
                        c.insert(
                            key,
                            CacheEntry {
                                size: fi.size,
                                mtime: fi.mtime.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(),
                                hash: h,
                            },
                        );
                    }
                }
            }
        }
        bar.inc(1);
    }
    bar.finish_with_message("Hashing complete ✔".green().bold().to_string());

    if let Ok(c) = Arc::try_unwrap(cache) {
        save_cache(&cli.downloads, &c.into_inner().unwrap());
    }

    // -------- Build hash map --------
    let mut hash_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
    for group in &groups {
        for fi in group.iter() {
            if let Some(ref h) = fi.hash {
                hash_map.entry(h.clone()).or_default().push(fi);
            }
        }
    }

    // -------- Duplicate resolution & output --------
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download".yellow(), "Media".yellow()]);

    let mut actions = Vec::new();

    for group in hash_map.values() {
        // split into downloads vs media
        let downloads: Vec<&&FileInfo> = group.iter().filter(|f| f.in_downloads).collect();
        let media: Vec<&&FileInfo> = group.iter().filter(|f| !f.in_downloads).collect();
        if downloads.is_empty() || media.is_empty() {
            continue;
        }
        // choose first media file as canonical
        let target = &media[0].path;
        for src in downloads {
            // skip already hard-links
            if src.dev == media[0].dev && src.inode == media[0].inode {
                continue;
            }
            table.add_row(vec![
                src.path.to_string_lossy().to_string(),
                target.to_string_lossy().to_string(),
            ]);
            actions.push((src.path.clone(), target.clone()));
        }
    }

    // print results
    println!("
{} Found duplicates:", "▶".cyan());
    println!("{}", table);
    if actions.is_empty() {
        println!("{} No actions needed.", "✔".green());
        return Ok(());
    }
    if cli.dry_run {
        println!("{} Dry-run; no changes.", "ℹ".yellow());
        return Ok(());
    }

    // apply replacements
    println!("{} Applying…", "▶".cyan());
    for (src, tgt) in actions {
        fs::remove_file(&src).with_context(|| format!("rm {}", src.display()))?;
        let rel = diff_paths(&tgt, &src.parent().unwrap()).context("failed to compute relative path")?;
        symlink(rel, &src).with_context(|| format!("ln {:?} -> {:?}", &src, &tgt))?;
    }
    println!("{} Done.", "✔".green());
    Ok(())
}
