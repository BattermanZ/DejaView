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

/// The first part of a library file's key when there is one library folder:
/// `media/<path below it>`, as DejaView has always keyed it.
const MEDIA_KEY_ROOT: &str = "media";

/// The first part of a library file's key when there are several library
/// folders: `library/<folder name>/<path below it>`.
const LIBRARY_KEY_ROOT: &str = "library";

/// Whether a key names a file in a library folder.
pub(super) fn is_media_key(key: &str) -> bool {
    [MEDIA_KEY_ROOT, LIBRARY_KEY_ROOT].iter().any(|root| {
        key.strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// How cache keys name paths: by root and the path below it, such as
/// `downloads/Arrival.2016.mkv` and `media/Movies/Arrival (2016).mkv`. The
/// same share mounted at two paths, say in a Docker cron job and on the host
/// the dashboard runs on, then keys its files alike, and neither run prunes
/// the other's entries. With several library folders a library key also
/// names its folder, `library/movies/Arrival (2016).mkv`, so two mounts of
/// them agree while each folder keeps its name. An entry is used only while
/// the file's size, times, device and inode match, so a key alone never
/// vouches for a file.
pub(super) struct CacheKeys {
    downloads: PathBuf,
    /// Each library folder with its name, which `config::check` keeps unique.
    library: Vec<(String, PathBuf)>,
}

impl CacheKeys {
    pub(super) fn new<P: AsRef<Path>>(downloads: &Path, library: &[P]) -> Self {
        CacheKeys {
            downloads: downloads.to_path_buf(),
            library: library
                .iter()
                .map(|folder| {
                    let folder = folder.as_ref();
                    (crate::config::library_name(folder), folder.to_path_buf())
                })
                .collect(),
        }
    }

    /// The key for a path found under one of the roots. A path under none,
    /// which a walk never yields, is keyed by itself.
    pub(super) fn key(&self, path: &Path, in_downloads: bool) -> String {
        if in_downloads {
            if let Ok(rel) = path.strip_prefix(&self.downloads) {
                return format!("downloads/{}", lossless(rel));
            }
            return lossless(path);
        }
        for (name, folder) in &self.library {
            if let Ok(rel) = path.strip_prefix(folder) {
                return match self.library.len() {
                    1 => format!("{MEDIA_KEY_ROOT}/{}", lossless(rel)),
                    _ => format!("{LIBRARY_KEY_ROOT}/{name}/{}", lossless(rel)),
                };
            }
        }
        lossless(path)
    }

    /// Rename keys of another form to this one, so no change costs a
    /// rehash: the absolute paths the oldest versions wrote, `media/` keys
    /// once a second library folder is added, and `library/` keys once only
    /// one is left. A `media/` key goes to the library folder that holds its
    /// path, which costs one `stat` per entry, once. A key that fits no root
    /// is left as it is and pruned by the next complete run. Returns how many
    /// keys changed.
    pub(super) fn migrate(&self, cache: &mut Cache) -> usize {
        let foreign_root = match self.library.len() {
            1 => LIBRARY_KEY_ROOT,
            _ => MEDIA_KEY_ROOT,
        };
        let old: Vec<String> = cache
            .keys()
            .filter(|key| key.starts_with('/') || below_root(key, foreign_root).is_some())
            .cloned()
            .collect();
        let mut migrated = 0;
        for old_key in old {
            let Some(new_key) = self.renamed(&old_key) else {
                continue;
            };
            if let Some(entry) = cache.remove(&old_key) {
                cache.entry(new_key).or_insert(entry);
                migrated += 1;
            }
        }
        migrated
    }

    /// This version's key for a key of another form, if a root fits it.
    fn renamed(&self, old_key: &str) -> Option<String> {
        let rel = if let Some(rel) = below_root(old_key, MEDIA_KEY_ROOT) {
            rel
        } else if let Some(rest) = below_root(old_key, LIBRARY_KEY_ROOT) {
            // Only the folder of that name can hold it.
            let (name, rel) = rest.split_once('/')?;
            let (_, folder) = self.library.iter().find(|(own, _)| own == name)?;
            return Some(self.key(&folder.join(unlossless(rel)), false));
        } else {
            let path = Path::new(old_key);
            let in_downloads = path.starts_with(&self.downloads);
            if !in_downloads && !self.library.iter().any(|(_, f)| path.starts_with(f)) {
                return None;
            }
            return Some(self.key(path, in_downloads));
        };
        let rel = unlossless(rel);
        let folder = match self.library.as_slice() {
            [(_, folder)] => folder,
            several => several
                .iter()
                .map(|(_, folder)| folder)
                .find(|folder| std::fs::symlink_metadata(folder.join(&rel)).is_ok())?,
        };
        Some(self.key(&folder.join(rel), false))
    }
}

/// What follows `<root>/` in a key.
fn below_root<'k>(key: &'k str, root: &str) -> Option<&'k str> {
    key.strip_prefix(root)?.strip_prefix('/')
}

/// The path a [`lossless`] string names: its NUL and two hex digits back
/// to the byte they stand for.
fn unlossless(text: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// A path as a string that loses nothing: UTF-8 as it is, and each byte
/// that is not as a NUL and two hex digits. No path holds a NUL, so two
/// names that differ only in such bytes get different keys, where a lossy
/// string gave them one; a UTF-8 path's key is what it always was.
fn lossless(path: &Path) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for chunk in path.as_os_str().as_bytes().utf8_chunks() {
        out.push_str(chunk.valid());
        for byte in chunk.invalid() {
            let _ = write!(out, "\0{byte:02x}");
        }
    }
    out
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
        || name.starts_with(".dejaview_validate_write.")
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
        // A run stopped between the symlink and the rename leaves a temp
        // symlink, which is removed as a link, never followed.
        if !(entry.file_type().is_file() || entry.file_type().is_symlink()) {
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
