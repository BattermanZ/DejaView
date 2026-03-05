use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, ContentArrangement, Table};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use pathdiff::diff_paths;

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use walkdir::{DirEntry, WalkDir};

// ---- CLI --------------------------------------------------------------------
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scan for duplicates (dry run)
    Scan,
    /// Replace duplicates with symlinks
    Link,
    /// Manage the configuration
    Config(ConfigArgs),
    /// Manage the cache
    Cache(CacheArgs),
}

#[derive(Parser, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Set the downloads path
    SetDownloads { path: PathBuf },
    /// Set the media path
    SetMedia { path: PathBuf },
    /// Show the current configuration
    Show,
}

#[derive(Parser, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommands,
}

#[derive(Subcommand, Debug)]
enum CacheCommands {
    /// Clear the cache
    Clear,
    /// Show cache info
    Info,
}

// ---- Configuration ----------------------------------------------------------
#[derive(Debug, Serialize, Deserialize, Default)]
struct Config {
    downloads: Option<PathBuf>,
    media: Option<PathBuf>,
    min_age: Option<u64>,
    threads: Option<usize>,
}

fn config_path() -> Result<PathBuf> {
    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow!("Could not find config directory"))?;
    Ok(config_dir.join("dejaview").join("config.json"))
}

fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let s = fs::read_to_string(path)?;
    let config = serde_json::from_str(&s)?;
    Ok(config)
}

fn save_config(config: &Config) -> Result<()> {
    let path = config_path()?;
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    let s = serde_json::to_string_pretty(config)?;
    fs::write(path, s)?;
    Ok(())
}

// ---- File Info and Cache ----------------------------------------------------
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

fn cache_path(config: &Config) -> Result<PathBuf> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    Ok(downloads.join(".dejaview_cache.json"))
}

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

fn load_cache(path: &Path) -> Cache {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_cache(path: &Path, cache: &Cache) {
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = fs::File::create(path).and_then(|mut f| f.write_all(json.as_bytes()));
    }
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

// ---- Subcommand Implementations ---------------------------------------------
fn run_scan(config: &Config, dry_run: bool) -> Result<()> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow!("Downloads path not set"))?;
    let media = config
        .media
        .as_ref()
        .ok_or_else(|| anyhow!("Media path not set"))?;
    let min_age = Duration::from_secs(config.min_age.unwrap_or(300));

    // -------- Progress bars --------
    let mp = MultiProgress::new();
    let spinner = mp.add(ProgressBar::new_spinner());
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(120));
    spinner.set_message("Collecting files…".cyan().bold().to_string());

    // -------- Gather files --------
    let mut seen: HashSet<(u64, u64)> = HashSet::new();

    let mut files = collect_files(downloads, true, min_age)?;
    for f in &files {
        seen.insert((f.dev, f.inode));
    }

    let mut media_files = collect_files(media, false, min_age)?;
    media_files.retain(|f| !seen.contains(&(f.dev, f.inode)));
    files.extend(media_files);

    spinner.finish_with_message("File list complete ✔".green().bold().to_string());

    // -------- Hashing phase --------
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
            }
        }
        bar.inc(1);
    }
    bar.finish_with_message("Hashing complete ✔".green().bold().to_string());

    if let Ok(c) = Arc::try_unwrap(cache) {
        save_cache(&cache_path, &c.into_inner().unwrap());
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
    let mut total_saved_space: u64 = 0;

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
            total_saved_space += src.size;
        }
    }

    // print results
    println!(
        "
{} Found duplicates:",
        "▶".cyan()
    );
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

    if dry_run {
        println!("{} Dry-run; no changes.", "ℹ".yellow());
        return Ok(());
    }

    // apply replacements
    println!("{} Applying…", "▶".cyan());
    for (src, tgt) in actions {
        ensure_target_within_media_root(media, &tgt)?;
        fs::remove_file(&src).with_context(|| format!("rm {}", src.display()))?;
        let rel =
            diff_paths(&tgt, src.parent().unwrap()).context("failed to compute relative path")?;
        symlink(rel, &src).with_context(|| format!("ln {:?} -> {:?}", &src, &tgt))?;
    }
    println!("{} Done.", "✔".green());
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = load_config()?;

    if let Some(threads) = config.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()?;
    }

    match cli.command {
        Commands::Scan => run_scan(&config, true)?,
        Commands::Link => run_scan(&config, false)?,
        Commands::Config(args) => match args.command {
            ConfigCommands::SetDownloads { path } => {
                config.downloads = Some(path);
                save_config(&config)?;
                println!("{} Downloads path set.", "✔".green());
            }
            ConfigCommands::SetMedia { path } => {
                config.media = Some(path);
                save_config(&config)?;
                println!("{} Media path set.", "✔".green());
            }
            ConfigCommands::Show => {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
        },
        Commands::Cache(args) => match args.command {
            CacheCommands::Clear => {
                let path = cache_path(&config)?;
                fs::remove_file(path)?;
                println!("{} Cache cleared.", "✔".green());
            }
            CacheCommands::Info => {
                let path = cache_path(&config)?;
                let cache = load_cache(&path);
                println!("Cache location: {}", path.display());
                println!("Cached entries: {}", cache.len());
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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
}
