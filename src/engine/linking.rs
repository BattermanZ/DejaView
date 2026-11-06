use anyhow::{Context, Result, anyhow};
use colored::*;
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use pathdiff::diff_paths;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::logger;
use crate::protocol::TuiEvent;

use crate::protocol::LinkReportEntry;

use super::cancel::CancelToken;
use super::types::emit_tui_event;
use super::types::{FileInfo, FileSnapshot, LinkAction, LinkMode, RunWarnings};

pub(super) fn ensure_target_within_media_root(media_root: &Path, target: &Path) -> Result<()> {
    let target_meta =
        fs::symlink_metadata(target).with_context(|| format!("metadata {}", target.display()))?;
    if target_meta.file_type().is_symlink() {
        return Err(anyhow!(
            "refusing to link to symlink target: {}",
            target.display()
        ));
    }

    if target.starts_with(media_root) {
        return Ok(());
    }

    let canonical_media = fs::canonicalize(media_root)
        .with_context(|| format!("canonicalize {}", media_root.display()))?;
    let canonical_target =
        fs::canonicalize(target).with_context(|| format!("canonicalize {}", target.display()))?;

    if !canonical_target.starts_with(&canonical_media) {
        return Err(anyhow!(
            "refusing to link to target outside media root: {}",
            target.display()
        ));
    }

    Ok(())
}

/// The library is never written to: refuse any source that resolves inside the
/// media root, whatever the planner produced. The final path component is not
/// resolved, so a source that is itself a symlink is judged by where it lives.
pub(super) fn ensure_source_outside_media_root(media_root: &Path, source: &Path) -> Result<()> {
    let refuse = || {
        anyhow!(
            "refusing to replace a file inside the media root: {}",
            source.display()
        )
    };
    if source.starts_with(media_root) {
        return Err(refuse());
    }

    let parent = source
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", source.display()))?;
    let canonical_parent =
        fs::canonicalize(parent).with_context(|| format!("canonicalize {}", parent.display()))?;
    let canonical_media = fs::canonicalize(media_root)
        .with_context(|| format!("canonicalize {}", media_root.display()))?;
    if canonical_parent.starts_with(&canonical_media) {
        return Err(refuse());
    }

    Ok(())
}

fn snapshot_from_file_info(file: &FileInfo) -> FileSnapshot {
    FileSnapshot {
        size: file.size,
        mtime_ns: file.mtime_ns,
        ctime_ns: file.ctime_ns,
        dev: file.dev,
        inode: file.inode,
    }
}

fn snapshot_path(path: &Path) -> Result<FileSnapshot> {
    let meta = fs::metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let mtime_ns = meta
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(FileSnapshot {
        size: meta.len(),
        mtime_ns,
        ctime_ns: (meta.ctime() as i128 * 1_000_000_000) + meta.ctime_nsec() as i128,
        dev: meta.dev(),
        inode: meta.ino(),
    })
}

/// The source must be the file the plan saw. One exception: when this run has
/// already replaced another path of the same inode (cross-seeded downloads are
/// hardlinks of each other), that replacement changed the inode's link count
/// and ctime, so a ctime change alone is accepted while size, mtime, device
/// and inode still match.
fn ensure_source_snapshot_matches(
    path: &Path,
    expected: &FileSnapshot,
    replaced_inodes: &HashSet<(u64, u64)>,
) -> Result<()> {
    let current = snapshot_path(path)?;
    if expected.matches_source(&current) {
        return Ok(());
    }
    if replaced_inodes.contains(&(expected.dev, expected.inode))
        && expected.matches_target(&current)
    {
        logger::info(
            "link",
            format!(
                "{} changed only its ctime, from replacing a hardlinked sibling earlier in this run",
                path.display()
            ),
        );
        return Ok(());
    }
    Err(anyhow!(
        "source changed since planning; rerun scan before linking: {}",
        path.display()
    ))
}

fn ensure_target_snapshot_matches(path: &Path, expected: &FileSnapshot) -> Result<()> {
    let current = snapshot_path(path)?;
    if !expected.matches_target(&current) {
        return Err(anyhow!(
            "target changed since planning; rerun scan before linking: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Owner, group and permission bits of a file.
#[derive(Clone, Copy, Debug)]
pub(super) struct Access {
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) mode: u32,
}

impl Access {
    fn of(meta: &fs::Metadata) -> Self {
        Access {
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode() & 0o7777,
        }
    }

    fn describe(self) -> String {
        format!("uid {}, gid {}, mode {:04o}", self.uid, self.gid, self.mode)
    }
}

/// Whether the user or group a download is read by would lose read access once
/// the download becomes a link to the library file. A hardlink is one inode, so
/// the download takes on the library file's owner and mode; reading through a
/// symlink is governed by them too. Returns who loses access.
///
/// The download's owner is assumed to belong to the download's group, which is
/// how torrent clients and the *arr apps are usually set up; root reads
/// anything and is never locked out.
pub(super) fn read_access_lost(download: Access, library: Access) -> Option<String> {
    let reads = |mode: u32, class_shift: u32| mode & (0o4 << class_shift) != 0;
    let (owner, group, other) = (6, 3, 0);

    if download.uid != 0 && reads(download.mode, owner) {
        let keeps = if library.uid == download.uid {
            reads(library.mode, owner)
        } else if library.gid == download.gid {
            reads(library.mode, group)
        } else {
            reads(library.mode, other)
        };
        if !keeps {
            return Some(format!("its owner (uid {})", download.uid));
        }
    }
    if reads(download.mode, group) {
        let keeps = if library.gid == download.gid {
            reads(library.mode, group)
        } else {
            reads(library.mode, other)
        };
        if !keeps {
            return Some(format!("its group (gid {})", download.gid));
        }
    }
    None
}

fn ensure_link_keeps_read_access(src: &Path, tgt: &Path) -> Result<()> {
    let download =
        Access::of(&fs::metadata(src).with_context(|| format!("metadata {}", src.display()))?);
    let library =
        Access::of(&fs::metadata(tgt).with_context(|| format!("metadata {}", tgt.display()))?);
    if let Some(who) = read_access_lost(download, library) {
        return Err(anyhow!(
            "left alone: once linked, {who} could no longer read {}, so the torrent client would stop seeding it. The download is {}; the library file {} is {}. Let that user or group read the library file (a shared group for the torrent client and Sonarr or Radarr, files 0664 or 0644), then run again.",
            src.display(),
            download.describe(),
            tgt.display(),
            library.describe()
        ));
    }
    Ok(())
}

/// The downloads and media roots, resolved once per run.
pub(super) struct CanonicalRoots {
    pub(super) downloads: PathBuf,
    pub(super) media: PathBuf,
}

impl CanonicalRoots {
    pub(super) fn new(downloads: &Path, media: &Path) -> Result<Self> {
        Ok(CanonicalRoots {
            downloads: fs::canonicalize(downloads)
                .with_context(|| format!("resolve the downloads folder {}", downloads.display()))?,
            media: fs::canonicalize(media)
                .with_context(|| format!("resolve the media folder {}", media.display()))?,
        })
    }
}

/// One replacement: the download, the library file it becomes a link to, and
/// what the plan saw of that library file.
pub(super) struct LinkRequest<'a> {
    pub(super) src: &'a Path,
    pub(super) tgt: &'a Path,
    pub(super) tgt_snapshot: &'a FileSnapshot,
    pub(super) roots: &'a CanonicalRoots,
    pub(super) link_mode: LinkMode,
}

/// What a replacement did.
#[derive(Debug)]
pub(super) struct Linked {
    /// "hardlink" or "symlink": what the download is now.
    pub(super) method: &'static str,
    pub(super) fallback_reason: Option<String>,
    /// The download already was the library file, so nothing changed.
    pub(super) already_linked: bool,
}

/// The download's folder, opened once. On Linux every later step goes through
/// `/proc/self/fd/N`, which reaches the opened folder itself without resolving
/// its path again, so a folder in the path swapped for a symlink after the
/// checks cannot move the replacement anywhere else. Off Linux, or without
/// /proc, the steps fall back to the path.
struct PinnedDir {
    path: PathBuf,
    _handle: Option<fs::File>,
}

impl PinnedDir {
    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.path.join(name)
    }
}

fn ensure_dir_inside_downloads(real_dir: &Path, src: &Path, roots: &CanonicalRoots) -> Result<()> {
    if real_dir.starts_with(&roots.media) || !real_dir.starts_with(&roots.downloads) {
        return Err(anyhow!(
            "refusing to replace {}: its folder is {}, which is not inside the downloads folder {}",
            src.display(),
            real_dir.display(),
            roots.downloads.display()
        ));
    }
    Ok(())
}

fn pin_download_dir(src: &Path, roots: &CanonicalRoots) -> Result<PinnedDir> {
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", src.display()))?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let handle =
            fs::File::open(parent).with_context(|| format!("open folder {}", parent.display()))?;
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()));
        match fs::read_link(&fd_path) {
            Ok(real_dir) => {
                ensure_dir_inside_downloads(&real_dir, src, roots)?;
                return Ok(PinnedDir {
                    path: fd_path,
                    _handle: Some(handle),
                });
            }
            Err(err) => logger::warn(
                "link",
                format!(
                    "cannot pin {} through /proc ({err}); linking by path",
                    parent.display()
                ),
            ),
        }
    }
    let real_dir =
        fs::canonicalize(parent).with_context(|| format!("resolve folder {}", parent.display()))?;
    ensure_dir_inside_downloads(&real_dir, src, roots)?;
    Ok(PinnedDir {
        path: parent.to_path_buf(),
        _handle: None,
    })
}

fn temp_name(kind: &str) -> String {
    format!(
        ".dejaview_{kind}.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

fn mtime_ns(meta: &fs::Metadata) -> u128 {
    meta.modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// What the hardlink step left at the temp name.
#[derive(Debug, PartialEq, Eq)]
enum TempLink {
    /// A regular file with the library file's size and mtime: a hardlink.
    Hardlink,
    /// A symlink to the library file, which mergerfs's `link-exdev` option
    /// makes, reporting success, when the two files sit on different disks.
    FilesystemSymlink,
}

/// Check what is at the temp name before it replaces the download. `link(2)`
/// does not follow a symlink, so a library path swapped for a symlink after the
/// checks leaves a symlink to wherever it pointed; anything that is not the
/// planned library file is refused here, with nothing replaced.
fn check_temp_link(tmp: &Path, req: &LinkRequest<'_>) -> Result<TempLink> {
    let meta = fs::symlink_metadata(tmp)
        .with_context(|| format!("metadata of temp link for {}", req.src.display()))?;
    let expected = req.tgt_snapshot;
    if meta.file_type().is_file()
        && meta.len() == expected.size
        && mtime_ns(&meta) == expected.mtime_ns
    {
        return Ok(TempLink::Hardlink);
    }
    if meta.file_type().is_symlink()
        && let (Ok(resolved), Ok(target)) = (fs::canonicalize(tmp), fs::canonicalize(req.tgt))
        && resolved == target
        && target.starts_with(&req.roots.media)
    {
        return Ok(TempLink::FilesystemSymlink);
    }
    Err(anyhow!(
        "the library file {} changed while it was being linked; nothing was replaced, rerun scan",
        req.tgt.display()
    ))
}

/// Rename the temp link over the download. Returns true when the temp name is
/// still there afterwards: rename(2) does nothing when both names are already
/// one file, which means the download already was the library file.
fn swap_into_place<G>(tmp: &Path, dest: &Path, src: &Path, rename_fn: &G) -> Result<bool>
where
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    rename_fn(tmp, dest).with_context(|| format!("rename the temp link over {}", src.display()))?;
    if fs::symlink_metadata(tmp).is_err() {
        return Ok(false);
    }
    logger::warn(
        "link",
        format!(
            "temp link remained after rename: {} already was the library file",
            src.display()
        ),
    );
    fs::remove_file(tmp)
        .with_context(|| format!("remove leftover temp link beside {}", src.display()))?;
    if fs::symlink_metadata(tmp).is_ok() {
        return Err(anyhow!(
            "leftover temp link remained after rename beside {}",
            src.display()
        ));
    }
    Ok(true)
}

fn file_name_of(src: &Path) -> Result<&std::ffi::OsStr> {
    src.file_name()
        .ok_or_else(|| anyhow!("source path has no file name: {}", src.display()))
}

/// Remove a temp name after a failed step, and say what happened to it.
fn clean_up_temp(tmp: &Path, kind: &str, src: &Path) -> String {
    match fs::remove_file(tmp) {
        Ok(()) => format!("temp {kind} beside {} cleaned up", src.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            format!("temp {kind} beside {} not created", src.display())
        }
        Err(err) => format!(
            "failed to remove temp {kind} beside {}: {err}",
            src.display()
        ),
    }
}

fn replace_with_symlink<G>(dir: &PinnedDir, req: &LinkRequest<'_>, rename_fn: &G) -> Result<()>
where
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let parent = req
        .src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", req.src.display()))?;
    let rel = diff_paths(req.tgt, parent).context("failed to compute relative path")?;
    let tmp = dir.join(temp_name("link"));
    logger::info(
        "link",
        format!(
            "creating temp symlink beside {} -> {}",
            req.src.display(),
            rel.display()
        ),
    );
    let result = (|| -> Result<()> {
        symlink(&rel, &tmp)
            .with_context(|| format!("ln -s {} beside {}", rel.display(), req.src.display()))?;
        swap_into_place(&tmp, &dir.join(file_name_of(req.src)?), req.src, rename_fn)?;
        Ok(())
    })();
    if let Err(err) = result {
        let note = clean_up_temp(&tmp, "symlink", req.src);
        return Err(anyhow!("{err:#}; {note}"));
    }
    Ok(())
}

/// Hardlink the library file to a temp name beside the download, check it, and
/// rename it over the download.
fn replace_with_hardlink<F, G>(
    dir: &PinnedDir,
    req: &LinkRequest<'_>,
    hard_link_fn: &F,
    rename_fn: &G,
) -> Result<Linked>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let tmp = dir.join(temp_name("hardlink"));
    logger::info(
        "link",
        format!(
            "creating temp hardlink beside {} -> {}",
            req.src.display(),
            req.tgt.display()
        ),
    );
    let result = (|| -> Result<Linked> {
        hard_link_fn(req.tgt, &tmp)
            .with_context(|| format!("ln {} beside {}", req.tgt.display(), req.src.display()))?;
        let made = check_temp_link(&tmp, req)?;
        let already_linked =
            swap_into_place(&tmp, &dir.join(file_name_of(req.src)?), req.src, rename_fn)?;
        Ok(match made {
            TempLink::Hardlink => Linked {
                method: "hardlink",
                fallback_reason: None,
                already_linked,
            },
            TempLink::FilesystemSymlink => Linked {
                method: "symlink",
                fallback_reason: Some(
                    "the filesystem made a symlink when asked for a hardlink (mergerfs link-exdev, the two files are on different disks)"
                        .to_string(),
                ),
                already_linked,
            },
        })
    })();
    result.map_err(|err| {
        let note = clean_up_temp(&tmp, "hardlink", req.src);
        anyhow!("{err:#}; {note}")
    })
}

pub(super) fn replace_file_with_best_link_with<F, G>(
    req: &LinkRequest<'_>,
    hard_link_fn: F,
    rename_fn: G,
) -> Result<Linked>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let dir = pin_download_dir(req.src, req.roots)?;
    let (src, tgt) = (req.src, req.tgt);
    if req.link_mode == LinkMode::SymlinkOnly {
        logger::info(
            "link",
            format!(
                "symlink-only mode active for {} -> {}",
                src.display(),
                tgt.display()
            ),
        );
        replace_with_symlink(&dir, req, &rename_fn)?;
        return Ok(Linked {
            method: "symlink",
            fallback_reason: Some("symlink-only mode enabled".to_string()),
            already_linked: false,
        });
    }
    let src_dev = fs::metadata(&dir.path)
        .with_context(|| format!("metadata of the folder of {}", src.display()))?
        .dev();
    let tgt_dev = fs::metadata(tgt)
        .with_context(|| format!("metadata {}", tgt.display()))?
        .dev();

    let fallback_reason = if src_dev == tgt_dev {
        match replace_with_hardlink(&dir, req, &hard_link_fn, &rename_fn) {
            Ok(linked) => return Ok(linked),
            Err(err) => {
                logger::warn(
                    "link",
                    format!(
                        "hardlink failed for {} -> {}: {}",
                        src.display(),
                        tgt.display(),
                        err
                    ),
                );
                Some(format!(
                    "hardlink failed; symlink applied successfully; {err}"
                ))
            }
        }
    } else {
        logger::info(
            "link",
            format!(
                "cross-device link fallback for {} -> {}",
                src.display(),
                tgt.display()
            ),
        );
        Some("cross-device; hardlink not possible".to_string())
    };

    replace_with_symlink(&dir, req, &rename_fn)?;
    Ok(Linked {
        method: "symlink",
        fallback_reason,
        already_linked: false,
    })
}

pub(super) fn replace_file_with_best_link(req: &LinkRequest<'_>) -> Result<Linked> {
    replace_file_with_best_link_with(
        req,
        |from, to| fs::hard_link(from, to),
        |from, to| fs::rename(from, to),
    )
}

/// Whether `download` already is the library file `library`: one inode, or one
/// file seen through two mounts. A bind mount or a mergerfs branch shows a
/// library file on a second device, often under another inode number, but with
/// the same size and timestamps; two real copies almost never share a ctime to
/// the nanosecond.
fn is_same_physical_file(download: &FileInfo, library: &FileInfo) -> bool {
    if download.dev == library.dev {
        download.inode == library.inode
    } else {
        download.size == library.size
            && download.mtime_ns == library.mtime_ns
            && download.ctime_ns == library.ctime_ns
    }
}

/// Plan one link per download in a group of identical files, all to the library
/// file whose path sorts first. A download that already is one of the group's
/// library files is left alone: relinking it frees nothing, breaks the
/// hardlink Sonarr or Radarr made, and through a second mount would replace
/// the library's only copy.
pub(super) fn plan_group_actions(group: &[&FileInfo]) -> Vec<LinkAction> {
    let mut downloads: Vec<&FileInfo> = group.iter().copied().filter(|f| f.in_downloads).collect();
    let mut media: Vec<&FileInfo> = group.iter().copied().filter(|f| !f.in_downloads).collect();

    if downloads.is_empty() || media.is_empty() {
        return Vec::new();
    }

    downloads.sort_by(|a, b| a.path.cmp(&b.path));
    media.sort_by(|a, b| a.path.cmp(&b.path));

    let canonical = media[0];
    let mut actions = Vec::new();
    for src in downloads {
        if media
            .iter()
            .any(|library| is_same_physical_file(src, library))
        {
            continue;
        }
        actions.push(LinkAction {
            src: src.path.clone(),
            tgt: canonical.path.clone(),
            size: src.size,
            src_snapshot: snapshot_from_file_info(src),
            tgt_snapshot: snapshot_from_file_info(canonical),
        });
    }

    actions
}

/// Downloads in a group that are a library file seen through a second mount,
/// each with the library path it matched. They are never planned; listing them
/// tells the person that downloads and media overlap.
fn library_files_seen_through_second_mount(group: &[&FileInfo]) -> Vec<(PathBuf, PathBuf)> {
    let media: Vec<&FileInfo> = group.iter().copied().filter(|f| !f.in_downloads).collect();
    group
        .iter()
        .filter(|f| f.in_downloads)
        .filter_map(|download| {
            media
                .iter()
                .find(|library| {
                    library.dev != download.dev && is_same_physical_file(download, library)
                })
                .map(|library| (download.path.clone(), library.path.clone()))
        })
        .collect()
}

fn hash_buckets(group: &[FileInfo]) -> Vec<Vec<&FileInfo>> {
    let mut hash_map: std::collections::HashMap<String, Vec<&FileInfo>> =
        std::collections::HashMap::new();
    for file in group {
        if let Some(hash) = &file.hash {
            hash_map.entry(hash.clone()).or_default().push(file);
        }
    }
    hash_map.into_values().collect()
}

pub(super) fn plan_hashed_group_actions(group: &[FileInfo]) -> Vec<LinkAction> {
    hash_buckets(group)
        .iter()
        .flat_map(|hashed_group| plan_group_actions(hashed_group))
        .collect()
}

pub(super) struct Plan {
    pub(super) table: Table,
    pub(super) actions: Vec<LinkAction>,
    /// Downloads left alone because they are a library file seen through a
    /// second mount, with the library path each matched.
    pub(super) seen_through_second_mount: Vec<(PathBuf, PathBuf)>,
}

pub(super) fn plan_actions_and_table(groups: &[Vec<FileInfo>]) -> Plan {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download".yellow(), "Media".yellow()]);

    let mut actions = Vec::new();
    let mut seen_through_second_mount = Vec::new();
    for group in groups {
        for hashed_group in hash_buckets(group) {
            seen_through_second_mount
                .extend(library_files_seen_through_second_mount(&hashed_group));
            for action in plan_group_actions(&hashed_group) {
                table.add_row(vec![
                    action.src.to_string_lossy().to_string(),
                    action.tgt.to_string_lossy().to_string(),
                ]);
                actions.push(action);
            }
        }
    }

    Plan {
        table,
        actions,
        seen_through_second_mount,
    }
}

pub(super) struct ApplyOptions<'a> {
    /// The downloads root: a download's folder must resolve inside it.
    pub(super) downloads: &'a Path,
    pub(super) strict: bool,
    pub(super) json_output: bool,
    pub(super) tui_events: bool,
    pub(super) link_mode: LinkMode,
    pub(super) cancel: &'a CancelToken,
}

#[derive(Default)]
pub(super) struct ApplyOutcome {
    pub(super) applied: usize,
    /// Pairs found at link time to be one file already; nothing changed.
    pub(super) already_linked: usize,
    pub(super) strict_error: Option<anyhow::Error>,
    /// Stopped between actions on request; actions not reached stay planned.
    pub(super) cancelled: bool,
}

pub(super) fn apply_actions(
    actions: &[LinkAction],
    media: &Path,
    options: &ApplyOptions<'_>,
    warnings: &mut RunWarnings,
    report_entries: &mut [LinkReportEntry],
) -> ApplyOutcome {
    let ApplyOptions {
        downloads,
        strict,
        json_output,
        tui_events,
        link_mode,
        cancel,
    } = *options;
    let mut applied_actions = 0usize;
    let mut already_linked = 0usize;
    let mut strict_link_error: Option<anyhow::Error> = None;
    let mut cancelled = false;
    // Source inodes this run replaced a path of; see ensure_source_snapshot_matches.
    let mut replaced_inodes: HashSet<(u64, u64)> = HashSet::new();

    if actions.is_empty() {
        return ApplyOutcome::default();
    }

    if !json_output && !tui_events {
        println!("{} Applying…", "▶".cyan());
    }
    let roots = CanonicalRoots::new(downloads, media);

    for (index, action) in actions.iter().enumerate() {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::File {
                phase: Some("applying".to_string()),
                path: action.src.display().to_string(),
            });
        }
        let apply_result = (|| -> Result<bool> {
            logger::info(
                "link",
                format!(
                    "applying {} -> {}",
                    action.src.display(),
                    action.tgt.display()
                ),
            );
            ensure_source_outside_media_root(media, &action.src)?;
            ensure_source_snapshot_matches(&action.src, &action.src_snapshot, &replaced_inodes)?;
            ensure_target_within_media_root(media, &action.tgt)?;
            ensure_target_snapshot_matches(&action.tgt, &action.tgt_snapshot)?;
            ensure_link_keeps_read_access(&action.src, &action.tgt)?;
            let roots = roots.as_ref().map_err(|err| anyhow!("{err:#}"))?;
            let linked = replace_file_with_best_link(&LinkRequest {
                src: &action.src,
                tgt: &action.tgt,
                tgt_snapshot: &action.tgt_snapshot,
                roots,
                link_mode,
            })?;
            report_entries[index].method = Some(linked.method.to_string());
            if linked.already_linked {
                report_entries[index].status = "already-linked".to_string();
                report_entries[index].fallback_reason = Some(
                    "already this library file; nothing changed. The filesystem gives one file different inode numbers: on mergerfs, mount with inodecalc=hybrid-hash, or use_ino on older versions".to_string(),
                );
            } else {
                report_entries[index].status = "applied".to_string();
                report_entries[index].fallback_reason = linked.fallback_reason;
            }
            Ok(linked.already_linked)
        })();
        let was_already_linked = match apply_result {
            Ok(was_already_linked) => was_already_linked,
            Err(err) => {
                warnings.link_errors += 1;
                report_entries[index].status = "failed".to_string();
                report_entries[index].error = Some(err.to_string());
                logger::error(
                    "link",
                    format!(
                        "failed {} -> {}: {}",
                        action.src.display(),
                        action.tgt.display(),
                        err
                    ),
                );
                if tui_events {
                    let _ = emit_tui_event(&TuiEvent::Log {
                        message: format!(
                            "Failed to apply {} -> {}: {}",
                            action.src.display(),
                            action.tgt.display(),
                            err
                        ),
                    });
                }
                if strict {
                    strict_link_error = Some(anyhow!(
                        "strict mode link failure for {} -> {}: {}",
                        action.src.display(),
                        action.tgt.display(),
                        err
                    ));
                    break;
                } else {
                    eprintln!(
                        "{} Failed to apply {} -> {}: {}",
                        "⚠".yellow(),
                        action.src.display(),
                        action.tgt.display(),
                        err
                    );
                    continue;
                }
            }
        };
        if was_already_linked {
            already_linked += 1;
            logger::warn(
                "link",
                format!(
                    "{} already was {}; nothing changed",
                    action.src.display(),
                    action.tgt.display()
                ),
            );
            continue;
        }
        applied_actions += 1;
        replaced_inodes.insert((action.src_snapshot.dev, action.src_snapshot.inode));
        logger::info(
            "link",
            format!(
                "applied {} -> {} via {}",
                action.src.display(),
                action.tgt.display(),
                report_entries[index].method.as_deref().unwrap_or("unknown")
            ),
        );
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::Count {
                files_seen: None,
                groups_found: None,
                planned_actions: Some(actions.len()),
                applied_actions: Some(applied_actions),
                hash_progress_done: None,
                hash_progress_total: None,
            });
        }
    }

    ApplyOutcome {
        applied: applied_actions,
        already_linked,
        strict_error: strict_link_error,
        cancelled,
    }
}
