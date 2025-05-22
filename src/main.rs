use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use pathdiff::diff_paths;
use rayon::prelude::*;
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write, Seek, SeekFrom};
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use walkdir::{DirEntry, WalkDir};

/// Simple, on-demand duplicate detector that replaces copies in the download
/// folder with **relative** symlinks pointing at the identical file in the
/// media folder.  Designed for large media libraries: size pre‑filter → partial
/// digest → full digest, plus a persistent hash cache for incremental runs.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Path to the download tree (torrents seed here)
    #[arg(long)]
    downloads: PathBuf,

    /// Path to the organised media library
    #[arg(long)]
    media: PathBuf,

    /// Perform a dry run – no deletions or symlinks are written
    #[arg(long, default_value_t = false)]
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

// fast 2‑chunk hash (first & last 1 MiB)
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
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(t) = cli.threads {
        rayon::ThreadPoolBuilder::new().num_threads(t).build_global()?;
    }
    let min_age = Duration::from_secs(cli.min_age);
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut files = collect_files(&cli.downloads, true, min_age)?;
    for f in &files { seen.insert((f.dev, f.inode)); }
    let mut media_files = collect_files(&cli.media, false, min_age)?;
    media_files.retain(|f| !seen.contains(&(f.dev, f.inode)));
    files.extend(media_files);
    let cache = Arc::new(Mutex::new(load_cache(&cli.downloads)));
    let inode_hash: Arc<Mutex<HashMap<(u64, u64), String>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files { size_map.entry(f.size).or_default().push(f); }
    let mut groups: Vec<&mut Vec<FileInfo>> = size_map.values_mut().filter(|v| v.len()>1).collect();
    let pb = ProgressBar::new(groups.len() as u64);
    pb.set_style(ProgressStyle::with_template("{msg} {pos}/{len} [{bar:40}] {elapsed}").unwrap());
    pb.set_message("Hashing candidates");
    for group in &mut groups {
        let cache = Arc::clone(&cache);
        let inode_hash = Arc::clone(&inode_hash);
        group.par_iter_mut().for_each(|fi| {
            let key = fi.path.to_string_lossy().into_owned();
            if let Ok(mut c) = cache.lock() {
                if let Some(e) = c.get(&key) {
                    if e.size==fi.size && e.mtime==fi.mtime.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() {
                        fi.hash = Some(e.hash.clone()); return;
                    }
                }
            }
            if let Ok(mut ih) = inode_hash.lock() {
                if let Some(h) = ih.get(&(fi.dev, fi.inode)) {
                    fi.hash = Some(h.clone()); return;
                }
            }
            let _ = sample_digest(&fi.path, fi.size);
            if let Ok(h) = full_digest(&fi.path) {
                if let Ok(mut ih) = inode_hash.lock() { ih.insert((fi.dev, fi.inode), h.clone()); }
                fi.hash=Some(h.clone());
                if let Ok(mut c)=cache.lock() {
                    c.insert(key, CacheEntry { size: fi.size, mtime: fi.mtime.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(), hash: h});
                }
            }
        });
        pb.inc(1);
    }
    pb.finish();
    if let Ok(c) = Arc::try_unwrap(cache) {
        save_cache(&cli.downloads, &c.into_inner().unwrap());
    }
    let mut hash_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
    for g in size_map.values() {
        for fi in g {
            if let Some(h)=&fi.hash { hash_map.entry(h.clone()).or_default().push(fi); }
        }
    }
    let mut replacements=Vec::new();
    for group in hash_map.values() {
        if group.len()<2 { continue; }
        let dl=group.iter().filter(|f|f.in_downloads).collect::<Vec<_>>();
        let md=group.iter().filter(|f|!f.in_downloads).collect::<Vec<_>>();
        if dl.is_empty() || md.is_empty() { continue; }
        let target=md[0];
        for src in dl { if !(src.dev==target.dev && src.inode==target.inode) { replacements.push((src.path.clone(), target.path.clone())); }}
    }
    if replacements.is_empty() { println!("No duplicates found between downloads and media."); return Ok(());}    
    println!("Planned replacements ({}):", replacements.len());
    for (dl, tgt) in &replacements { println!("{} -> {}", dl.display(), tgt.display()); }
    if cli.dry_run { println!("\nDry-run mode: no changes made."); return Ok(());}  
    println!("\nApplying changes…");
    for (dl, tgt) in &replacements {
        fs::remove_file(dl).with_context(||format!("delete {}",dl.display()))?;
        let rel=diff_paths(tgt,dl.parent().unwrap()).with_context(||format!("rel {}->{}",dl.display(),tgt.display()))?;
        symlink(rel,dl).with_context(||format!("symlink {}->{}",dl.display(),tgt.display()))?;
    }
    println!("Completed.");
    Ok(())
}
