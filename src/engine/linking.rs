use anyhow::{Context, Result, anyhow};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};
use pathdiff::diff_paths;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::logger;
use crate::protocol::TuiEvent;

use crate::protocol::{Counts, EntryStatus, LinkMethod, LinkReportEntry, Phase, WarningSummary};

use super::cancel::CancelToken;
use super::types::emit_tui_event;
use super::types::{FileInfo, FileSnapshot, LinkAction, LinkMode};

/// The target guard by path alone, as the tests drive it.
#[cfg(test)]
pub(super) fn ensure_target_within_media_root(media_root: &Path, target: &Path) -> Result<()> {
    let target_meta =
        fs::symlink_metadata(target).with_context(|| format!("metadata {}", target.display()))?;
    let canonical_media = fs::canonicalize(media_root)
        .with_context(|| format!("canonicalize {}", media_root.display()))?;
    refuse_target_outside_media_root(media_root, &canonical_media, target, &target_meta)
}

/// Refuse a target that is a symlink or lies outside the media root, given
/// the target's `lstat` and the media root resolved once per run.
fn refuse_target_outside_media_root(
    media_root: &Path,
    canonical_media: &Path,
    target: &Path,
    target_meta: &fs::Metadata,
) -> Result<()> {
    if target_meta.file_type().is_symlink() {
        return Err(anyhow!(
            "refusing to link to symlink target: {}",
            target.display()
        ));
    }

    if target.starts_with(media_root) {
        return Ok(());
    }

    let canonical_target =
        fs::canonicalize(target).with_context(|| format!("canonicalize {}", target.display()))?;

    if !canonical_target.starts_with(canonical_media) {
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
pub(super) fn ensure_source_outside_media_root(
    media_root: &Path,
    canonical_media: &Path,
    source: &Path,
) -> Result<()> {
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
    if canonical_parent.starts_with(canonical_media) {
        return Err(refuse());
    }

    Ok(())
}

/// The file at `path` itself, not what a symlink there points to.
fn lstat(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).with_context(|| {
        format!(
            "cannot look up {}; it may have been moved or deleted since the scan, so scan again",
            path.display()
        )
    })
}

/// The source must be the file the plan saw. One exception: when this run has
/// already replaced another path of the same inode (cross-seeded downloads are
/// hardlinks of each other), that replacement changed the inode's link count
/// and ctime, so a ctime change alone is accepted while size, mtime, device
/// and inode still match.
fn ensure_source_snapshot_matches(
    path: &Path,
    meta: &fs::Metadata,
    expected: &FileSnapshot,
    replaced_inodes: &HashSet<(u64, u64)>,
) -> Result<()> {
    let current = FileSnapshot::from_metadata(meta);
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

fn ensure_target_snapshot_matches(
    path: &Path,
    meta: &fs::Metadata,
    expected: &FileSnapshot,
) -> Result<()> {
    if !expected.matches_target(&FileSnapshot::from_metadata(meta)) {
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

/// Why this pair must be left alone for access, or None to go ahead. A skip is
/// a decision, not a failure: the run carries on, `--strict` included.
fn read_access_refusal(
    src: &Path,
    src_meta: &fs::Metadata,
    tgt: &Path,
    tgt_meta: &fs::Metadata,
) -> Option<String> {
    let download = Access::of(src_meta);
    let library = Access::of(tgt_meta);
    read_access_lost(download, library).map(|who| {
        format!(
            "left alone: once linked, {who} could no longer read {}, so the torrent client would stop seeding it. The download is {}; the library file {} is {}. Let that user or group read the library file (a shared group for the torrent client and Sonarr or Radarr, files 0664 or 0644), then run again.",
            src.display(),
            download.describe(),
            tgt.display(),
            library.describe()
        )
    })
}

/// What became of a planned pair that did not fail.
enum PairOutcome {
    /// Replaced with a link. `freed` when the download was its file's last
    /// path, so its bytes came back.
    Applied { freed: bool },
    /// The download already was the library file; nothing changed.
    AlreadyLinked,
    /// Left alone on purpose, with the reason in the report.
    Skipped(String),
}

/// What a person reads when the download and the library file are on
/// different devices, so no hardlink can join them.
pub(super) const CROSS_DEVICE_REASON: &str = "cross-device: the download and the library file are on different filesystems or mounts, so no hardlink is possible";

/// Why a hardlink failed, in words a person can act on, read from the OS error
/// in the chain. None when no OS error is there.
pub(super) fn hardlink_failure_cause(err: &anyhow::Error) -> Option<&'static str> {
    use std::io::ErrorKind;
    let io = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())?;
    Some(match io.kind() {
        ErrorKind::CrossesDevices => {
            "different filesystems: the two files are on different disks or mounts, such as two mergerfs branches or two Docker volumes"
        }
        ErrorKind::PermissionDenied => {
            "not permitted: the library file belongs to another user and fs.protected_hardlinks lets only its owner hardlink it, or the download's folder is not writable"
        }
        ErrorKind::ReadOnlyFilesystem => "the filesystem is mounted read-only",
        ErrorKind::TooManyLinks => {
            "the library file has as many hardlinks as its filesystem allows"
        }
        _ => "the filesystem refused the hardlink",
    })
}

/// What a person reads for a pair that failed: what happened and what to do
/// about it, then the system's own words. Without an OS error in the chain,
/// or with one no advice fits, it is the chain itself: those messages already
/// name the file and the next step. The log keeps the whole chain either way.
pub(super) fn link_failure_message(err: &anyhow::Error, src: &Path) -> String {
    use std::io::ErrorKind;
    let Some(io) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
    else {
        return format!("{err:#}");
    };
    let folder = src.parent().unwrap_or(src).display();
    let what = match io.kind() {
        ErrorKind::PermissionDenied => format!(
            "DejaView was not allowed to change the folder {folder}, so the download was left as it was. Give the user that runs DejaView write access to that folder, then link again."
        ),
        ErrorKind::ReadOnlyFilesystem => format!(
            "The folder {folder} is on a filesystem mounted read-only, so the download was left as it was. Mount it read-write, then link again."
        ),
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => format!(
            "The disk that holds {folder} is full, so the download was left as it was. Free some space there, then link again."
        ),
        _ => return format!("{err:#}"),
    };
    format!("{what} The system said: {io}.")
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
    pub(super) src_snapshot: &'a FileSnapshot,
    pub(super) tgt: &'a Path,
    pub(super) tgt_snapshot: &'a FileSnapshot,
    pub(super) roots: &'a CanonicalRoots,
    pub(super) link_mode: LinkMode,
}

/// What a replacement did.
#[derive(Debug)]
pub(super) struct Linked {
    /// Hardlink or symlink: what the download is now.
    pub(super) method: LinkMethod,
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
    /// Where the folder really is, for working out a relative symlink.
    real: PathBuf,
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
            "refusing to replace {}: its folder is {}, which is not inside the downloads folder {}. A folder in downloads may be a symlink to somewhere else; remove the symlink or scan again",
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
                    real: real_dir,
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
        real: real_dir,
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
    if meta.file_type().is_file()
        && req
            .tgt_snapshot
            .matches_contents(&FileSnapshot::from_metadata(&meta))
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

/// Check, in the pinned folder and right before the rename, that the download
/// is still the file the plan saw: same device, inode, size and mtime. The
/// earlier checks went by path; this one is what the rename will replace. The
/// ctime may differ when this run replaced a hardlinked sibling (C1).
fn ensure_pinned_source_unchanged(dir: &PinnedDir, req: &LinkRequest<'_>) -> Result<()> {
    let meta = fs::symlink_metadata(dir.join(file_name_of(req.src)?)).with_context(|| {
        format!(
            "cannot look up {} right before replacing it; nothing was replaced, scan again",
            req.src.display()
        )
    })?;
    let unchanged = meta.file_type().is_file()
        && req
            .src_snapshot
            .matches_target(&FileSnapshot::from_metadata(&meta));
    if !unchanged {
        return Err(anyhow!(
            "the download {} changed while it was being linked; nothing was replaced, rerun scan",
            req.src.display()
        ));
    }
    Ok(())
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
    let target = fs::canonicalize(req.tgt)
        .with_context(|| format!("resolve the library file {}", req.tgt.display()))?;
    let rel = diff_paths(&target, &dir.real).context("failed to compute relative path")?;
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
        ensure_pinned_source_unchanged(dir, req)?;
        swap_into_place(&tmp, &dir.join(file_name_of(req.src)?), req.src, rename_fn)?;
        Ok(())
    })();
    if let Err(err) = result {
        // As context, so the OS error stays in the chain for
        // link_failure_message to read.
        let note = clean_up_temp(&tmp, "symlink", req.src);
        return Err(err.context(note));
    }
    Ok(())
}

/// Why the hardlink step did not replace the download.
enum HardlinkError {
    /// The filesystem would not make the hardlink or rename it into place; a
    /// symlink may still do.
    Failed(anyhow::Error),
    /// The library file or the download changed under the run. Nothing may
    /// replace the download, a symlink included: it would point at a file the
    /// plan never compared.
    Refused(anyhow::Error),
}

/// Hardlink the library file to a temp name beside the download, check it and
/// the download, and rename it over the download.
fn replace_with_hardlink<F, G>(
    dir: &PinnedDir,
    req: &LinkRequest<'_>,
    hard_link_fn: &F,
    rename_fn: &G,
) -> std::result::Result<Linked, HardlinkError>
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
    let with_note = |err: anyhow::Error| {
        let note = clean_up_temp(&tmp, "hardlink", req.src);
        match hardlink_failure_cause(&err) {
            Some(cause) => anyhow!("{cause}. {err:#}; {note}"),
            None => anyhow!("{err:#}; {note}"),
        }
    };

    if let Err(err) = hard_link_fn(req.tgt, &tmp) {
        let err = anyhow::Error::new(err).context(format!(
            "ln {} beside {}",
            req.tgt.display(),
            req.src.display()
        ));
        return Err(HardlinkError::Failed(with_note(err)));
    }
    let made = check_temp_link(&tmp, req)
        .and_then(|made| ensure_pinned_source_unchanged(dir, req).map(|()| made))
        .map_err(|err| HardlinkError::Refused(with_note(err)))?;
    let dest = file_name_of(req.src)
        .map(|name| dir.join(name))
        .map_err(|err| HardlinkError::Failed(with_note(err)))?;
    let already_linked = swap_into_place(&tmp, &dest, req.src, rename_fn)
        .map_err(|err| HardlinkError::Failed(with_note(err)))?;
    Ok(match made {
        TempLink::Hardlink => Linked {
            method: LinkMethod::Hardlink,
            fallback_reason: None,
            already_linked,
        },
        TempLink::FilesystemSymlink => Linked {
            method: LinkMethod::Symlink,
            fallback_reason: Some(
                "the filesystem made a symlink when asked for a hardlink (mergerfs link-exdev, the two files are on different disks)"
                    .to_string(),
            ),
            already_linked,
        },
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
            method: LinkMethod::Symlink,
            fallback_reason: Some("symlink-only mode enabled".to_string()),
            already_linked: false,
        });
    }
    let src_dev = fs::metadata(&dir.path)
        .with_context(|| format!("metadata of the folder of {}", src.display()))?
        .dev();
    // The caller checked the library file still matches the plan, device
    // included.
    let tgt_dev = req.tgt_snapshot.dev;

    let fallback_reason = if src_dev == tgt_dev {
        match replace_with_hardlink(&dir, req, &hard_link_fn, &rename_fn) {
            Ok(linked) => return Ok(linked),
            Err(HardlinkError::Refused(err)) => return Err(err),
            Err(HardlinkError::Failed(err)) => {
                logger::warn(
                    "link",
                    format!(
                        "hardlink failed for {} -> {}: {err:#}",
                        src.display(),
                        tgt.display(),
                    ),
                );
                Some(format!(
                    "hardlink failed; symlink applied successfully; {err:#}"
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
        Some(CROSS_DEVICE_REASON.to_string())
    };

    replace_with_symlink(&dir, req, &rename_fn)?;
    Ok(Linked {
        method: LinkMethod::Symlink,
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
            .any(|library| src.snapshot.is_same_physical_file(&library.snapshot))
        {
            continue;
        }
        actions.push(LinkAction {
            src: src.path.clone(),
            tgt: canonical.path.clone(),
            size: src.snapshot.size,
            src_snapshot: src.snapshot,
            tgt_snapshot: canonical.snapshot,
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
                    library.snapshot.dev != download.snapshot.dev
                        && download.snapshot.is_same_physical_file(&library.snapshot)
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

/// The order every plan is shown, reported and applied in: by download path,
/// then library path.
pub(super) fn sort_actions(actions: &mut [LinkAction]) {
    actions.sort_by(|a, b| a.src.cmp(&b.src).then_with(|| a.tgt.cmp(&b.tgt)));
}

pub(super) struct Plan {
    pub(super) actions: Vec<LinkAction>,
    /// Downloads left alone because they are a library file seen through a
    /// second mount, with the library path each matched.
    pub(super) seen_through_second_mount: Vec<(PathBuf, PathBuf)>,
}

pub(super) fn plan_actions(groups: &[Vec<FileInfo>]) -> Plan {
    let mut actions = Vec::new();
    let mut seen_through_second_mount = Vec::new();
    for group in groups {
        for hashed_group in hash_buckets(group) {
            seen_through_second_mount
                .extend(library_files_seen_through_second_mount(&hashed_group));
            actions.extend(plan_group_actions(&hashed_group));
        }
    }
    // Groups come out of hash maps in no fixed order; a plan that changes
    // order between runs cannot be diffed or reviewed twice.
    sort_actions(&mut actions);
    seen_through_second_mount.sort();
    Plan {
        actions,
        seen_through_second_mount,
    }
}

/// The table a person reads: one row per planned action, each path below
/// its folder, which the caller prints once above it. The header is plain:
/// colour codes would count as width and push the columns out of line.
pub(super) fn action_table(actions: &[LinkAction], downloads: &Path, media: &Path) -> Table {
    let below = |path: &Path, root: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download", "Library file", "Size"]);
    for action in actions {
        table.add_row(vec![
            below(&action.src, downloads),
            below(&action.tgt, media),
            crate::human::format_size(action.size),
        ]);
    }
    table
}

pub(super) struct ApplyOptions<'a> {
    /// The downloads root: a download's folder must resolve inside it.
    pub(super) downloads: &'a Path,
    pub(super) strict: bool,
    pub(super) tui_events: bool,
    pub(super) link_mode: LinkMode,
    pub(super) cancel: &'a CancelToken,
}

/// What the apply loop did, counted: the report and the verdict line both
/// take these.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Applied {
    pub(super) applied: usize,
    /// Pairs found at link time to be one file already; nothing changed.
    pub(super) already_linked: usize,
    pub(super) already_linked_bytes: u64,
    /// Bytes that came back: a download's, once its last path was replaced.
    pub(super) freed_bytes: u64,
    /// Pairs left alone on purpose, such as for access; not failures.
    pub(super) skipped: usize,
}

#[derive(Default)]
pub(super) struct ApplyOutcome {
    pub(super) counts: Applied,
    pub(super) strict_error: Option<anyhow::Error>,
    /// Stopped between actions on request; actions not reached stay planned.
    pub(super) cancelled: bool,
}

pub(super) fn apply_actions(
    actions: &[LinkAction],
    media: &Path,
    options: &ApplyOptions<'_>,
    warnings: &mut WarningSummary,
    report_entries: &mut [LinkReportEntry],
) -> ApplyOutcome {
    let ApplyOptions {
        downloads,
        strict,
        tui_events,
        link_mode,
        cancel,
    } = *options;
    let mut applied_actions = 0usize;
    let mut already_linked = 0usize;
    let mut already_linked_bytes = 0u64;
    let mut freed_bytes = 0u64;
    let mut skipped = 0usize;
    let mut strict_link_error: Option<anyhow::Error> = None;
    let mut cancelled = false;
    // Source inodes this run replaced a path of; see ensure_source_snapshot_matches.
    let mut replaced_inodes: HashSet<(u64, u64)> = HashSet::new();

    if actions.is_empty() {
        return ApplyOutcome::default();
    }

    let roots = CanonicalRoots::new(downloads, media);
    // Resolved once per run, not once per pair.
    let canonical_media = fs::canonicalize(media);

    for (index, action) in actions.iter().enumerate() {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::File {
                phase: Some(Phase::Applying),
                path: action.src.display().to_string(),
            });
        }
        let apply_result = (|| -> Result<PairOutcome> {
            logger::info(
                "link",
                format!(
                    "applying {} -> {}",
                    action.src.display(),
                    action.tgt.display()
                ),
            );
            let canonical_media = canonical_media.as_ref().map_err(|err| {
                anyhow!(
                    "cannot resolve the media folder {}: {err}; check that it is mounted and readable, then run again",
                    media.display()
                )
            })?;
            ensure_source_outside_media_root(media, canonical_media, &action.src)?;
            // One lstat of each file serves every check below.
            let src_meta = lstat(&action.src)?;
            ensure_source_snapshot_matches(
                &action.src,
                &src_meta,
                &action.src_snapshot,
                &replaced_inodes,
            )?;
            let tgt_meta = lstat(&action.tgt)?;
            refuse_target_outside_media_root(media, canonical_media, &action.tgt, &tgt_meta)?;
            ensure_target_snapshot_matches(&action.tgt, &tgt_meta, &action.tgt_snapshot)?;
            // A download's bytes come back only with its last path: a
            // cross-seeded sibling still holds them until it is replaced too.
            let last_path = src_meta.nlink() == 1;
            if let Some(reason) =
                read_access_refusal(&action.src, &src_meta, &action.tgt, &tgt_meta)
            {
                let entry = &mut report_entries[index];
                entry.status = EntryStatus::Skipped;
                entry.method = None;
                entry.fallback_reason = None;
                entry.error = Some(reason.clone());
                return Ok(PairOutcome::Skipped(reason));
            }
            let roots = roots.as_ref().map_err(|err| anyhow!("{err:#}"))?;
            let linked = replace_file_with_best_link(&LinkRequest {
                src: &action.src,
                src_snapshot: &action.src_snapshot,
                tgt: &action.tgt,
                tgt_snapshot: &action.tgt_snapshot,
                roots,
                link_mode,
            })?;
            report_entries[index].method = Some(linked.method);
            if linked.already_linked {
                report_entries[index].status = EntryStatus::AlreadyLinked;
                report_entries[index].fallback_reason = Some(
                    "already this library file; nothing changed. The filesystem gives one file different inode numbers: on mergerfs, mount with inodecalc=hybrid-hash, or use_ino on older versions".to_string(),
                );
            } else {
                report_entries[index].status = EntryStatus::Applied;
                report_entries[index].fallback_reason = linked.fallback_reason;
            }
            Ok(if linked.already_linked {
                PairOutcome::AlreadyLinked
            } else {
                PairOutcome::Applied { freed: last_path }
            })
        })();
        let outcome = match apply_result {
            Ok(outcome) => outcome,
            Err(err) => {
                warnings.link_errors += 1;
                // The log gets the whole chain: the outer context names the
                // step, and the OS error that says why is in its cause. The
                // report and the screen get what happened and what to do.
                let chain = format!("{err:#}");
                let err = link_failure_message(&err, &action.src);
                let entry = &mut report_entries[index];
                entry.status = EntryStatus::Failed;
                // What the plan expected no longer applies to a pair that
                // was not linked.
                entry.method = None;
                entry.fallback_reason = None;
                entry.error = Some(err.clone());
                logger::error(
                    "link",
                    format!(
                        "failed {} -> {}: {chain}",
                        action.src.display(),
                        action.tgt.display(),
                    ),
                );
                if tui_events {
                    let _ = emit_tui_event(&TuiEvent::Log {
                        message: format!(
                            "Failed to apply {} -> {}: {err}",
                            action.src.display(),
                            action.tgt.display(),
                        ),
                    });
                }
                if strict {
                    strict_link_error = Some(anyhow!(
                        "--strict stopped the run at its first failed link, {} -> {}: {err}. The pairs before it are linked and the rest stay planned; the report lists each",
                        action.src.display(),
                        action.tgt.display(),
                    ));
                    break;
                } else {
                    super::report::print_warning(format_args!(
                        "Failed to apply {} -> {}: {err}",
                        action.src.display(),
                        action.tgt.display(),
                    ));
                    continue;
                }
            }
        };
        if let PairOutcome::Skipped(reason) = &outcome {
            skipped += 1;
            logger::warn("link", reason);
            if tui_events {
                let _ = emit_tui_event(&TuiEvent::Log {
                    message: reason.clone(),
                });
            }
            continue;
        }
        if matches!(outcome, PairOutcome::AlreadyLinked) {
            already_linked += 1;
            already_linked_bytes += action.size;
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
        if matches!(outcome, PairOutcome::Applied { freed: true }) {
            freed_bytes += action.size;
        }
        replaced_inodes.insert((action.src_snapshot.dev, action.src_snapshot.inode));
        logger::info(
            "link",
            format!(
                "applied {} -> {} via {}",
                action.src.display(),
                action.tgt.display(),
                report_entries[index]
                    .method
                    .map_or("an unknown method", LinkMethod::as_str)
            ),
        );
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::Count(Counts {
                planned_actions: Some(actions.len()),
                applied_actions: Some(applied_actions),
                ..Counts::default()
            }));
        }
    }

    ApplyOutcome {
        counts: Applied {
            applied: applied_actions,
            already_linked,
            already_linked_bytes,
            freed_bytes,
            skipped,
        },
        strict_error: strict_link_error,
        cancelled,
    }
}
