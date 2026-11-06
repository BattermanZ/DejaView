use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::{Config, cache_path};
use crate::fsutil::safe_write_bytes;
use crate::logger;

use super::types::Cache;

/// Read the cache. A missing file is an empty cache. A file that cannot be
/// read or parsed is too, with a warning in the log, since it means hashing
/// the whole library again.
pub(super) fn load_cache(path: &Path) -> Cache {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            logger::info("cache", format!("cache missing at {}", path.display()));
            return Cache::new();
        }
        Err(err) => {
            logger::warn(
                "cache",
                format!(
                    "cannot read the cache {} ({err}); starting with an empty one, so every candidate is hashed again",
                    path.display()
                ),
            );
            return Cache::new();
        }
    };
    logger::info("cache", format!("loading cache from {}", path.display()));
    // Parsed as it is read, so a large cache is not held twice in memory.
    serde_json::from_reader(BufReader::new(file)).unwrap_or_else(|err| {
        logger::warn(
            "cache",
            format!(
                "the cache {} cannot be parsed ({err}); starting with an empty one, so every candidate is hashed again",
                path.display()
            ),
        );
        Cache::new()
    })
}

pub(super) fn save_cache(path: &Path, cache: &Cache) -> Result<()> {
    let json = serde_json::to_vec(cache)?;
    logger::info(
        "cache",
        format!("saving {} cache entries to {}", cache.len(), path.display()),
    );
    safe_write_bytes(path, &json)
}

/// The first part of the key of every file under the media root.
const MEDIA_KEY_ROOT: &str = "media";

/// Whether a key names a file under the media root.
pub(super) fn is_media_key(key: &str) -> bool {
    key.strip_prefix(MEDIA_KEY_ROOT)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// How cache keys name paths: by root and the path below it, such as
/// `media/Movies/Arrival (2016).mkv`. The same share mounted at two paths, say
/// in a Docker cron job and on the host the dashboard runs on, then keys its
/// files alike, and neither run prunes the other's entries. An entry is used
/// only while the file's size, times, device and inode match, so a key alone
/// never vouches for a file.
pub(super) struct CacheKeys {
    downloads: PathBuf,
    media: PathBuf,
}

impl CacheKeys {
    pub(super) fn new(downloads: &Path, media: &Path) -> Self {
        CacheKeys {
            downloads: downloads.to_path_buf(),
            media: media.to_path_buf(),
        }
    }

    /// The key for a path found under one of the roots. A path under neither,
    /// which a walk never yields, is keyed by itself.
    pub(super) fn key(&self, path: &Path, in_downloads: bool) -> String {
        let (name, root) = if in_downloads {
            ("downloads", &self.downloads)
        } else {
            (MEDIA_KEY_ROOT, &self.media)
        };
        match path.strip_prefix(root) {
            Ok(rel) => format!("{name}/{}", rel.to_string_lossy()),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }

    /// Rename the keys an earlier version wrote, the absolute path of each
    /// file, to this form, so upgrading costs no rehash. A key under neither
    /// root is left as it is and pruned by the next complete run. Returns how
    /// many keys changed.
    pub(super) fn migrate(&self, cache: &mut Cache) -> usize {
        let old: Vec<String> = cache
            .keys()
            .filter(|key| key.starts_with('/'))
            .cloned()
            .collect();
        let mut migrated = 0;
        for old_key in old {
            let path = Path::new(&old_key);
            let in_downloads = if path.starts_with(&self.downloads) {
                true
            } else if path.starts_with(&self.media) {
                false
            } else {
                continue;
            };
            let new_key = self.key(path, in_downloads);
            if let Some(entry) = cache.remove(&old_key) {
                cache.entry(new_key).or_insert(entry);
                migrated += 1;
            }
        }
        migrated
    }
}

/// Drop entries for paths this run did not see, so the cache follows the trees
/// instead of growing with every file that ever passed through them. An entry
/// for a path the walk could not read, or under a folder it could not, is
/// kept: the file may well still be there. Returns how many entries were
/// dropped.
pub(super) fn prune_cache(
    cache: &mut Cache,
    seen: &HashSet<String>,
    unreadable: &[String],
) -> usize {
    let under = |key: &str, path: &str| {
        key == path
            || key
                .strip_prefix(path)
                .is_some_and(|rest| path.ends_with('/') || rest.starts_with('/'))
    };
    let before = cache.len();
    cache.retain(|key, _| seen.contains(key) || unreadable.iter().any(|path| under(key, path)));
    before - cache.len()
}

pub(super) fn clear_cache_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove cache file {}", path.display())),
    }
}

pub(super) fn cache_info(config: &Config) -> Result<(std::path::PathBuf, usize)> {
    let path = cache_path(config)?;
    let cache = load_cache(&path);
    Ok((path, cache.len()))
}

pub(super) fn clear_cache(config: &Config) -> Result<()> {
    let path = cache_path(config)?;
    logger::info("cache", format!("clearing cache at {}", path.display()));
    clear_cache_file(&path)
}

fn is_temp_artifact_name(name: &str) -> bool {
    name.starts_with(".dejaview_hardlink.tmp.")
        || name.starts_with(".dejaview_link.tmp.")
        || name.starts_with(".dejaview_write.tmp.")
        || name.starts_with(".dejaview_tui_plan.")
        || name.starts_with(".dejaview_validate_link.")
}

pub(super) fn audit_temp_artifacts(config: &Config) -> Result<Vec<std::path::PathBuf>> {
    let downloads = config
        .downloads
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Downloads path not set"))?;
    let mut matches = Vec::new();
    for entry in WalkDir::new(downloads).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        if is_temp_artifact_name(name) {
            matches.push(entry.path().to_path_buf());
        }
    }
    logger::info(
        "cache",
        format!(
            "audited temp artifacts under {}: {} found",
            downloads.display(),
            matches.len()
        ),
    );
    Ok(matches)
}

pub(super) fn cleanup_temp_artifacts(config: &Config) -> Result<Vec<std::path::PathBuf>> {
    let matches = audit_temp_artifacts(config)?;
    let mut removed = Vec::new();
    for path in matches {
        fs::remove_file(&path)
            .with_context(|| format!("remove temp artifact {}", path.display()))?;
        removed.push(path);
    }
    logger::info(
        "cache",
        format!("removed {} temp artifact(s)", removed.len()),
    );
    Ok(removed)
}
