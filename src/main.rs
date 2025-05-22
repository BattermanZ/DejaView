use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use pathdiff::diff_paths;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Simple, on-demand duplicate detector that replaces copies in the
/// download folder with relative symlinks pointing at the identical file in the
/// media folder.
#[derive(Parser, Debug)]
#[command(author, version, about = "Detect duplicates and symlink download copies to the media library.")]
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
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
    dev: u64,
    inode: u64,
    hash: Option<String>,
    in_downloads: bool,
}

fn is_regular(entry: &DirEntry) -> bool {
    entry.file_type().is_file() && !entry.path_is_symlink()
}

fn collect_files(root: &Path, in_downloads: bool) -> Result<Vec<FileInfo>> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(is_regular)
        .map(|entry| {
            let meta = entry.metadata()?;
            Ok(FileInfo {
                path: entry.into_path(),
                size: meta.len(),
                dev: meta.dev(),
                inode: meta.ino(),
                hash: None,
                in_downloads,
            })
        })
        .collect()
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. Index files
    println!("Scanning {:?} and {:?}…", cli.downloads, cli.media);
    let mut files = collect_files(&cli.downloads, true)?;
    files.extend(collect_files(&cli.media, false)?);

    // 2. Group by size
    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files {
        size_map.entry(f.size).or_default().push(f);
    }

    // Collect groups needing hashing
    let mut groups_to_hash: Vec<&mut Vec<FileInfo>> = size_map
        .values_mut()
        .filter(|v| v.len() > 1)
        .collect();

    // Progress bar (single-threaded updates)
    let total_groups = groups_to_hash.len() as u64;
    let pb = ProgressBar::new(total_groups);
    pb.set_style(
        ProgressStyle::with_template("{msg} {pos}/{len} [{bar:40}] {elapsed}").unwrap(),
    );
    pb.set_message("Hashing candidates");

    // 3. Hash each group in parallel, then update the bar
    for group in &mut groups_to_hash {
        group.par_iter_mut().for_each(|fi| {
            if fi.hash.is_none() {
                if let Ok(h) = hash_file(&fi.path) {
                    fi.hash = Some(h);
                }
            }
        });
        pb.inc(1);
    }
    pb.finish();

    // 4. Map hashes and find cross-root duplicates
    let mut hash_map: HashMap<String, Vec<&FileInfo>> = HashMap::new();
    for g in size_map.values() {
        for fi in g {
            if let Some(h) = &fi.hash {
                hash_map.entry(h.clone()).or_default().push(fi);
            }
        }
    }

    let mut replacements = Vec::new();
    for group in hash_map.values() {
        if group.len() < 2 { continue; }
        let downloads: Vec<&FileInfo> = group.iter().copied().filter(|f| f.in_downloads).collect();
        let medias: Vec<&FileInfo>   = group.iter().copied().filter(|f| !f.in_downloads).collect();
        if downloads.is_empty() || medias.is_empty() { continue; }
        let target = medias[0];
        for src in downloads {
            if src.dev == target.dev && src.inode == target.inode { continue; }
            replacements.push((src.path.clone(), target.path.clone()));
        }
    }

    // 5. Report plan
    if replacements.is_empty() {
        println!("No duplicates found between downloads and media.");
        return Ok(());
    }
    println!("Planned replacements ({}):", replacements.len());
    for (dl, target) in &replacements {
        println!("{} -> {}", dl.display(), target.display());
    }

    if cli.dry_run {
        println!("\nDry-run mode: no changes made.");
        return Ok(());
    }

    // 6. Apply replacements using relative symlinks
    println!("\nApplying changes…");
    for (dl, target) in &replacements {
        fs::remove_file(dl).with_context(|| format!("delete {}", dl.display()))?;
        // compute a relative path from the download location to the media file
        let relative = diff_paths(target, dl.parent().unwrap())
            .with_context(|| format!("computing relative path from {} to {}", dl.display(), target.display()))?;
        symlink(relative, dl)
            .with_context(|| format!("symlink {} -> {}", dl.display(), target.display()))?;
    }

    println!("Completed.");
    Ok(())
}
