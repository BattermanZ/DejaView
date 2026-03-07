use anyhow::{anyhow, Context, Result};
use colored::*;
use comfy_table::{modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL, ContentArrangement, Table};
use pathdiff::diff_paths;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::TuiEvent;

use crate::protocol::LinkReportEntry;

use super::types::emit_tui_event;
use super::types::{FileInfo, LinkAction, RunWarnings};

pub(super) fn ensure_target_within_media_root(media_root: &Path, target: &Path) -> Result<()> {
    let canonical_media = fs::canonicalize(media_root)
        .with_context(|| format!("canonicalize {}", media_root.display()))?;
    let canonical_target =
        fs::canonicalize(target).with_context(|| format!("canonicalize {}", target.display()))?;

    if !canonical_target.starts_with(&canonical_media) {
        return Err(anyhow!(
            "refusing to link to target outside media root: {}",
            canonical_target.display()
        ));
    }

    Ok(())
}

pub(super) fn replace_file_with_relative_symlink(src: &Path, tgt: &Path) -> Result<()> {
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
        symlink(&rel, &tmp_link)
            .with_context(|| format!("ln -s {:?} {:?}", rel, tmp_link.display()))?;
        fs::rename(&tmp_link, src)
            .with_context(|| format!("mv {:?} {:?}", tmp_link.display(), src.display()))?;
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
        hard_link_fn(tgt, &tmp_link)
            .with_context(|| format!("ln {:?} {:?}", tgt.display(), tmp_link.display()))?;
        fs::rename(&tmp_link, src)
            .with_context(|| format!("mv {:?} {:?}", tmp_link.display(), src.display()))?;
        Ok(())
    })();

    if link_result.is_err() {
        let _ = fs::remove_file(&tmp_link);
    }

    link_result
}

pub(super) fn replace_file_with_best_link_with<F>(
    src: &Path,
    tgt: &Path,
    hard_link_fn: F,
) -> Result<(String, Option<String>)>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let src_dev = fs::metadata(src)
        .with_context(|| format!("metadata {}", src.display()))?
        .dev();
    let tgt_dev = fs::metadata(tgt)
        .with_context(|| format!("metadata {}", tgt.display()))?
        .dev();

    let fallback_reason = if src_dev == tgt_dev {
        match replace_file_with_hardlink_with(src, tgt, hard_link_fn) {
            Ok(()) => return Ok(("hardlink".to_string(), None)),
            Err(err) => Some(format!("hardlink failed: {err}")),
        }
    } else {
        Some("cross-device; hardlink not possible".to_string())
    };

    replace_file_with_relative_symlink(src, tgt)?;
    Ok(("symlink".to_string(), fallback_reason))
}

pub(super) fn replace_file_with_best_link(
    src: &Path,
    tgt: &Path,
) -> Result<(String, Option<String>)> {
    replace_file_with_best_link_with(src, tgt, |from, to| fs::hard_link(from, to))
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
        });
    }

    actions
}

pub(super) fn plan_actions_and_table(groups: &[Vec<FileInfo>]) -> (Table, Vec<LinkAction>, u64) {
    let mut hash_map: std::collections::HashMap<String, Vec<&FileInfo>> =
        std::collections::HashMap::new();
    for group in groups {
        for fi in group {
            if let Some(ref h) = fi.hash {
                hash_map.entry(h.clone()).or_default().push(fi);
            }
        }
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Download".yellow(), "Media".yellow()]);

    let mut actions = Vec::new();
    let mut total_saved_space: u64 = 0;
    for group in hash_map.values() {
        let planned = plan_group_actions(group);
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
    warnings: &mut RunWarnings,
    report_entries: &mut [LinkReportEntry],
) -> (usize, Option<anyhow::Error>) {
    let mut applied_actions = 0usize;
    let mut strict_link_error: Option<anyhow::Error> = None;

    if actions.is_empty() {
        return (applied_actions, strict_link_error);
    }

    if !json_output {
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
            ensure_target_within_media_root(media, &action.tgt)?;
            let (method, fallback_reason) = replace_file_with_best_link(&action.src, &action.tgt)?;
            report_entries[index].method = Some(method);
            report_entries[index].fallback_reason = fallback_reason;
            report_entries[index].status = "applied".to_string();
            Ok(())
        })();
        if let Err(err) = apply_result {
            warnings.link_errors += 1;
            report_entries[index].status = "failed".to_string();
            report_entries[index].error = Some(err.to_string());
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
        if tui_events {
            let _ = emit_tui_event(&TuiEvent::Count {
                files_seen: None,
                groups_found: None,
                planned_actions: Some(actions.len()),
                applied_actions: Some(applied_actions),
            });
        }
    }

    (applied_actions, strict_link_error)
}
