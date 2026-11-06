use anyhow::{Context, Result, anyhow};
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::{DirEntry, WalkDir};

use super::cancel::CancelToken;
use super::types::emit_tui_event;
use super::types::{FileInfo, RunMetrics, RunWarnings};
use crate::protocol::TuiEvent;

#[derive(Copy, Clone)]
pub(super) struct CollectOptions<'a> {
    /// Files smaller than this are skipped; at least 1, so empty files always are.
    pub min_size: u64,
    pub strict: bool,
    pub show_progress: bool,
    pub tui_events: bool,
    pub cancel: &'a CancelToken,
}

fn is_regular(entry: &DirEntry) -> bool {
    entry.file_type().is_file() && !entry.path_is_symlink()
}

/// DejaView writes its cache, reports and temp files under the `.dejaview_`
/// prefix in the downloads root; they are never candidates.
fn is_own_file(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|name| name.starts_with(".dejaview_"))
}

/// A root folder as the filesystem knows it: its device and inode, which stay
/// the same through every mount that shows it.
#[derive(Clone, Debug)]
pub(super) struct RootId {
    pub(super) name: &'static str,
    pub(super) path: PathBuf,
    /// The path with symlinks resolved, or the path itself if it cannot be.
    pub(super) canonical: PathBuf,
    pub(super) dev: u64,
    pub(super) ino: u64,
}

impl RootId {
    pub(super) fn of(name: &'static str, path: &Path) -> Result<Self> {
        let meta = fs::metadata(path)
            .with_context(|| format!("cannot read the {name} folder {}", path.display()))?;
        Ok(RootId {
            name,
            path: path.to_path_buf(),
            canonical: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.path) || path.starts_with(&self.canonical)
    }
}

/// `base` joined with `rel`, with `.` and `..` worked out on the text alone:
/// a dangling symlink's destination cannot be resolved on disk.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in base.join(rel).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A symlink in downloads that points into the library and no longer resolves:
/// most likely a link DejaView made whose library file Sonarr or Radarr has
/// since upgraded, renamed or deleted, so its torrent cannot seed.
fn is_dangling_link_into(entry: &DirEntry, media: &RootId) -> bool {
    if !entry.file_type().is_symlink() {
        return false;
    }
    let Ok(dest) = fs::read_link(entry.path()) else {
        return false;
    };
    let parent = entry.path().parent().unwrap_or(Path::new("/"));
    media.contains(&lexical_join(parent, &dest))
        && matches!(fs::metadata(entry.path()), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
}

/// Refuse a directory that is the other root seen through a second mount.
/// Path checks cannot see a bind mount, so the walk compares device and inode:
/// without this, a library reached from inside downloads is walked as
/// downloads and its files get rewritten.
fn ensure_not_other_root(dir: &Path, walking: &RootId, other: &RootId) -> Result<()> {
    let meta = match fs::metadata(dir) {
        Ok(meta) => meta,
        // An unreadable directory is the walk's own error to report.
        Err(_) => return Ok(()),
    };
    if meta.dev() == other.dev && meta.ino() == other.ino {
        return Err(anyhow!(
            "the {} folder {} reaches the {} folder {} at {}: the same directory through a second mount, such as a bind mount. DejaView would treat library files as downloads and replace them. Point downloads and media at separate folders that neither contains the other on disk.",
            walking.name,
            walking.path.display(),
            other.name,
            other.path.display(),
            dir.display()
        ));
    }
    Ok(())
}

/// Walk only the directories of `walking` and refuse if one of them is `other`.
/// `config validate` runs this both ways; a scan does the same check as part of
/// its own walk.
pub(super) fn ensure_root_not_reached(walking: &RootId, other: &RootId) -> Result<()> {
    for entry in WalkDir::new(&walking.path).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_dir() {
            ensure_not_other_root(entry.path(), walking, other)?;
        }
    }
    Ok(())
}

pub(super) fn collect_files(
    root: &RootId,
    other_root: &RootId,
    in_downloads: bool,
    min_age: Duration,
    options: &CollectOptions<'_>,
    warnings: &mut RunWarnings,
    metrics: &mut RunMetrics,
) -> Result<Vec<FileInfo>> {
    let CollectOptions {
        strict, tui_events, ..
    } = *options;
    let deadline = SystemTime::now() - min_age;
    let mut files = Vec::new();
    for entry in WalkDir::new(&root.path).follow_links(false).into_iter() {
        options.cancel.check()?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warnings.walk_errors += 1;
                if strict {
                    return Err(anyhow!("walk error under {}: {}", root.path.display(), err));
                }
                continue;
            }
        };
        if entry.file_type().is_dir() {
            ensure_not_other_root(entry.path(), root, other_root)?;
            continue;
        }
        if in_downloads && is_dangling_link_into(&entry, other_root) {
            metrics.dangling_symlinks.push(entry.path().to_path_buf());
            continue;
        }
        if !is_regular(&entry) || is_own_file(&entry) {
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
        if meta.len() < options.min_size.max(1) {
            metrics.skipped_by_min_size += 1;
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mtime_ns = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        if mtime > deadline {
            metrics.skipped_by_min_age += 1;
            continue;
        }
        files.push(FileInfo {
            path: entry.into_path(),
            size: meta.len(),
            mtime_ns,
            ctime_ns: (meta.ctime() as i128 * 1_000_000_000) + meta.ctime_nsec() as i128,
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
    options: CollectOptions<'_>,
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

    let downloads_root = RootId::of("downloads", downloads)?;
    let media_root = RootId::of("media", media)?;
    let mut files = collect_files(
        &downloads_root,
        &media_root,
        true,
        min_age,
        &options,
        warnings,
        metrics,
    )?;
    let media_files = collect_files(
        &media_root,
        &downloads_root,
        false,
        min_age,
        &options,
        warnings,
        metrics,
    )?;
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
