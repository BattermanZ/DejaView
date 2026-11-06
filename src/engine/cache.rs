use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use walkdir::WalkDir;

use crate::config::{Config, cache_path};
use crate::fsutil::safe_write_bytes;
use crate::logger;

use super::types::Cache;

pub(super) fn load_cache(path: &Path) -> Cache {
    match fs::read_to_string(path) {
        Ok(s) => {
            logger::info("cache", format!("loading cache from {}", path.display()));
            serde_json::from_str(&s).unwrap_or_default()
        }
        Err(_) => {
            logger::info("cache", format!("cache missing at {}", path.display()));
            Cache::new()
        }
    }
}

pub(super) fn save_cache(path: &Path, cache: &Cache) -> Result<()> {
    let json = serde_json::to_vec(cache)?;
    logger::info(
        "cache",
        format!("saving {} cache entries to {}", cache.len(), path.display()),
    );
    safe_write_bytes(path, &json)
}

/// The cache key for a path. Every cache read, write and prune goes through it.
pub(super) fn cache_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Drop entries for paths this run did not see, so the cache follows the trees
/// instead of growing with every file that ever passed through them. Returns how
/// many entries were dropped.
pub(super) fn prune_cache(cache: &mut Cache, seen: &HashSet<String>) -> usize {
    let before = cache.len();
    cache.retain(|path, _| seen.contains(path));
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
