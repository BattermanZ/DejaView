use anyhow::{Context, Result, anyhow};
use colored::*;
use comfy_table::{ContentArrangement, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};
use pathdiff::diff_paths;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::logger;
use crate::protocol::TuiEvent;

use crate::protocol::LinkReportEntry;

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

fn ensure_source_snapshot_matches(path: &Path, expected: &FileSnapshot) -> Result<()> {
    let current = snapshot_path(path)?;
    if !expected.matches_source(&current) {
        return Err(anyhow!(
            "source changed since planning; rerun scan before linking: {}",
            path.display()
        ));
    }
    Ok(())
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

fn finalize_tmp_swap_with<G>(tmp_link: &Path, src: &Path, rename_fn: G) -> Result<()>
where
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    rename_fn(tmp_link, src)
        .with_context(|| format!("mv {:?} {:?}", tmp_link.display(), src.display()))?;
    if tmp_link.exists() {
        logger::warn(
            "link",
            format!(
                "temp link remained after rename, cleaning up {}",
                tmp_link.display()
            ),
        );
        fs::remove_file(tmp_link)
            .with_context(|| format!("remove leftover temp link {}", tmp_link.display()))?;
        if tmp_link.exists() {
            return Err(anyhow!(
                "leftover temp link remained after rename: {}",
                tmp_link.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn replace_file_with_relative_symlink(src: &Path, tgt: &Path) -> Result<()> {
    replace_file_with_relative_symlink_with(src, tgt, |from, to| fs::rename(from, to))
}

fn replace_file_with_relative_symlink_with<G>(src: &Path, tgt: &Path, rename_fn: G) -> Result<()>
where
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", src.display()))?;
    let rel = diff_paths(tgt, parent).context("failed to compute relative path")?;
    let tmp_link = parent.join(format!(
        ".dejaview_link.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let link_result = (|| -> Result<()> {
        logger::info(
            "link",
            format!(
                "creating temp symlink {} -> {}",
                tmp_link.display(),
                tgt.display()
            ),
        );
        symlink(&rel, &tmp_link)
            .with_context(|| format!("ln -s {:?} {:?}", rel, tmp_link.display()))?;
        finalize_tmp_swap_with(&tmp_link, src, rename_fn)?;
        Ok(())
    })();

    if link_result.is_err() {
        let _ = fs::remove_file(&tmp_link);
    }

    link_result
}

fn replace_file_with_hardlink_with<F>(src: &Path, tgt: &Path, hard_link_fn: F) -> Result<()>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
{
    replace_file_with_hardlink_and_rename_with(src, tgt, hard_link_fn, |from, to| {
        fs::rename(from, to)
    })
}

pub(super) fn replace_file_with_hardlink_and_rename_with<F, G>(
    src: &Path,
    tgt: &Path,
    hard_link_fn: F,
    rename_fn: G,
) -> Result<()>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", src.display()))?;
    let tmp_link = parent.join(format!(
        ".dejaview_hardlink.tmp.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let link_result = (|| -> Result<()> {
        logger::info(
            "link",
            format!(
                "creating temp hardlink {} -> {}",
                tmp_link.display(),
                tgt.display()
            ),
        );
        hard_link_fn(tgt, &tmp_link)
            .with_context(|| format!("ln {:?} {:?}", tgt.display(), tmp_link.display()))?;
        finalize_tmp_swap_with(&tmp_link, src, rename_fn)?;
        Ok(())
    })();

    if let Err(err) = link_result {
        let cleanup_note = match fs::remove_file(&tmp_link) {
            Ok(()) => format!("temp hardlink path cleaned up: {}", tmp_link.display()),
            Err(remove_err) if remove_err.kind() == std::io::ErrorKind::NotFound => {
                format!("temp hardlink path not created: {}", tmp_link.display())
            }
            Err(remove_err) => format!(
                "failed to remove temp hardlink path {}: {}",
                tmp_link.display(),
                remove_err
            ),
        };
        return Err(anyhow!("{err}; {cleanup_note}"));
    }

    Ok(())
}

pub(super) fn replace_file_with_best_link_with<F>(
    src: &Path,
    tgt: &Path,
    hard_link_fn: F,
    link_mode: LinkMode,
) -> Result<(String, Option<String>)>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
{
    if link_mode == LinkMode::SymlinkOnly {
        logger::info(
            "link",
            format!(
                "symlink-only mode active for {} -> {}",
                src.display(),
                tgt.display()
            ),
        );
        replace_file_with_relative_symlink(src, tgt)?;
        return Ok((
            "symlink".to_string(),
            Some("symlink-only mode enabled".to_string()),
        ));
    }
    let parent = src
        .parent()
        .ok_or_else(|| anyhow!("source path has no parent: {}", src.display()))?;
    let src_dev = fs::metadata(parent)
        .with_context(|| format!("metadata {}", parent.display()))?
        .dev();
    let tgt_dev = fs::metadata(tgt)
        .with_context(|| format!("metadata {}", tgt.display()))?
        .dev();

    let fallback_reason = if src_dev == tgt_dev {
        match replace_file_with_hardlink_with(src, tgt, hard_link_fn) {
            Ok(()) => return Ok(("hardlink".to_string(), None)),
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

    replace_file_with_relative_symlink(src, tgt)?;
    Ok(("symlink".to_string(), fallback_reason))
}

pub(super) fn replace_file_with_best_link(
    src: &Path,
    tgt: &Path,
    link_mode: LinkMode,
) -> Result<(String, Option<String>)> {
    replace_file_with_best_link_with(src, tgt, |from, to| fs::hard_link(from, to), link_mode)
}

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
        if src.dev == canonical.dev && src.inode == canonical.inode {
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

pub(super) fn plan_hashed_group_actions(group: &[FileInfo]) -> Vec<LinkAction> {
    let mut hash_map: std::collections::HashMap<String, Vec<&FileInfo>> =
        std::collections::HashMap::new();
    for file in group {
        if let Some(hash) = &file.hash {
            hash_map.entry(hash.clone()).or_default().push(file);
        }
    }

    let mut actions = Vec::new();
    for hashed_group in hash_map.values() {
        actions.extend(plan_group_actions(hashed_group));
    }
    actions
}

pub(super) fn plan_actions_and_table(groups: &[Vec<FileInfo>]) -> (Table, Vec<LinkAction>, u64) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download".yellow(), "Media".yellow()]);

    let mut actions = Vec::new();
    let mut total_saved_space: u64 = 0;
    for group in groups {
        let planned = plan_hashed_group_actions(group);
        for action in planned {
            table.add_row(vec![
                action.src.to_string_lossy().to_string(),
                action.tgt.to_string_lossy().to_string(),
            ]);
            total_saved_space += action.size;
            actions.push(action);
        }
    }

    (table, actions, total_saved_space)
}

pub(super) fn apply_actions(
    actions: &[LinkAction],
    media: &Path,
    strict: bool,
    json_output: bool,
    tui_events: bool,
    link_mode: LinkMode,
    warnings: &mut RunWarnings,
    report_entries: &mut [LinkReportEntry],
) -> (usize, Option<anyhow::Error>) {
    let mut applied_actions = 0usize;
    let mut strict_link_error: Option<anyhow::Error> = None;

    if actions.is_empty() {
        return (applied_actions, strict_link_error);
    }

    if !json_output && !tui_events {
        println!("{} Applying…", "▶".cyan());
    }

    for (index, action) in actions.iter().enumerate() {
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::File {
                phase: Some("applying".to_string()),
                path: action.src.display().to_string(),
            });
        }
        let apply_result = (|| -> Result<()> {
            logger::info(
                "link",
                format!(
                    "applying {} -> {}",
                    action.src.display(),
                    action.tgt.display()
                ),
            );
            ensure_source_snapshot_matches(&action.src, &action.src_snapshot)?;
            ensure_target_within_media_root(media, &action.tgt)?;
            ensure_target_snapshot_matches(&action.tgt, &action.tgt_snapshot)?;
            let (method, fallback_reason) =
                replace_file_with_best_link(&action.src, &action.tgt, link_mode)?;
            report_entries[index].method = Some(method);
            report_entries[index].fallback_reason = fallback_reason;
            report_entries[index].status = "applied".to_string();
            Ok(())
        })();
        if let Err(err) = apply_result {
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
        applied_actions += 1;
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

    (applied_actions, strict_link_error)
}
