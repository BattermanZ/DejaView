use anyhow::{Context, Result};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::config::{Config, cache_path};
use crate::fsutil::safe_write_bytes;

use super::types::Cache;

pub(super) fn load_cache(path: &Path) -> Cache {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Cache::new(),
    }
}

pub(super) fn save_cache(path: &Path, cache: &Cache) -> Result<()> {
    let json = serde_json::to_vec(cache)?;
    safe_write_bytes(path, &json)
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
    clear_cache_file(&path)
}
