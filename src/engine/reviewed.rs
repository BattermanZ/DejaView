//! Linking rows a person reviewed in the dashboard without walking both
//! folders again. The scan that showed the rows hashed both files of each in
//! full and cached the hashes, so when neither file has changed since, the
//! cache and two `stat` calls per pair vouch for it. On a network drive that
//! turns a walk of minutes into a moment. Anything the cache cannot vouch for
//! sends the whole run back to the walk, which checks every row as before.

use anyhow::Result;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::cache::CacheKeys;
use super::collect::{Exclude, RootId, ensure_not_other_root, is_own_file};
use super::types::{Cache, FileSnapshot, LinkAction};

/// What a file must pass to be a candidate at all, as the walk applies it.
pub(super) struct Filters<'a> {
    pub(super) min_size: u64,
    /// Files modified after this are too young.
    pub(super) deadline: SystemTime,
    pub(super) exclude: &'a Exclude,
}

/// Why the cache cannot vouch for a row, for the log.
type NotVouched = String;

/// The actions for the reviewed rows, each checked against the cache, or
/// None when one of them cannot be, with the reason. An error is a refusal
/// the walk would make too: a folder on a row's path is the other root seen
/// through a second mount.
pub(super) fn plan_from_cache(
    pairs: &[(PathBuf, PathBuf)],
    downloads: &RootId,
    media: &RootId,
    cache: &Cache,
    keys: &CacheKeys,
    filters: &Filters<'_>,
) -> Result<std::result::Result<Vec<LinkAction>, NotVouched>> {
    let mut actions = Vec::with_capacity(pairs.len());
    for (src, tgt) in pairs {
        let checked = (|| -> Result<std::result::Result<LinkAction, NotVouched>> {
            let (src_snapshot, src_hash) =
                match vouch_for_file(src, downloads, media, true, cache, keys, filters)? {
                    Ok(known) => known,
                    Err(why) => return Ok(Err(why)),
                };
            let (tgt_snapshot, tgt_hash) =
                match vouch_for_file(tgt, media, downloads, false, cache, keys, filters)? {
                    Ok(known) => known,
                    Err(why) => return Ok(Err(why)),
                };
            if src_hash != tgt_hash || src_snapshot.size != tgt_snapshot.size {
                return Ok(Err(format!(
                    "{} and {} no longer hash alike",
                    src.display(),
                    tgt.display()
                )));
            }
            // Already one file, or one file through two mounts: the walk
            // leaves such a download alone and says why.
            if src_snapshot.is_same_physical_file(&tgt_snapshot) {
                return Ok(Err(format!(
                    "{} already is {}",
                    src.display(),
                    tgt.display()
                )));
            }
            Ok(Ok(LinkAction {
                src: src.clone(),
                tgt: tgt.clone(),
                size: src_snapshot.size,
                src_snapshot,
                tgt_snapshot,
            }))
        })()?;
        match checked {
            Ok(action) => actions.push(action),
            Err(why) => return Ok(Err(why)),
        }
    }
    Ok(Ok(actions))
}

/// What the cache knows of one file of a row, if it still holds: the file
/// is where the walk would find it and would take it, unchanged since its
/// full hash was cached.
fn vouch_for_file(
    path: &Path,
    root: &RootId,
    other: &RootId,
    in_downloads: bool,
    cache: &Cache,
    keys: &CacheKeys,
    filters: &Filters<'_>,
) -> Result<std::result::Result<(FileSnapshot, String), NotVouched>> {
    let not = |why: &str| Ok(Err(format!("{} {why}", path.display())));
    let Ok(rel) = path.strip_prefix(&root.path) else {
        return not(&format!("is not under the {} folder", root.name));
    };
    for component in rel.components() {
        match component {
            Component::Normal(name) if filters.exclude.matching(name).is_some() => {
                return not("is under a name on the exclude list");
            }
            Component::Normal(_) => {}
            _ => return not("is not a plain path"),
        }
    }
    if path.file_name().is_none_or(is_own_file) {
        return not("is one of DejaView's own files");
    }
    // The walk refuses a folder that is the other root through a second
    // mount; check every folder from this file up to its root the same way.
    for dir in path.ancestors().skip(1) {
        ensure_not_other_root(dir, root, other)?;
        if dir == root.path {
            break;
        }
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => return not(&format!("cannot be read ({err})")),
    };
    if !meta.file_type().is_file() {
        return not("is not a regular file");
    }
    if meta.len() < filters.min_size.max(1) {
        return not("is under the size floor");
    }
    if meta.modified().unwrap_or(SystemTime::UNIX_EPOCH) > filters.deadline {
        return not("is younger than the minimum age");
    }
    let snapshot = FileSnapshot::of(&meta);
    let Some(entry) = cache.get(&keys.key(path, in_downloads)) else {
        return not("has no cache entry");
    };
    let unchanged = FileSnapshot {
        size: entry.size,
        mtime_ns: entry.mtime_ns,
        ctime_ns: entry.ctime_ns,
        dev: entry.dev,
        inode: entry.inode,
    } == snapshot;
    if !unchanged {
        return not("changed since it was hashed");
    }
    match &entry.hash {
        Some(hash) => Ok(Ok((snapshot, hash.clone()))),
        None => not("has no full hash cached"),
    }
}
