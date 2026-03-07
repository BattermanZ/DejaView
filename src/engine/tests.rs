use super::*;
use crate::config::Config;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::cache::{clear_cache_file, load_cache, save_cache};
use super::collect::collect_files;
use super::linking::{
    apply_actions, ensure_target_within_media_root, plan_group_actions,
    replace_file_with_best_link_with, replace_file_with_relative_symlink,
};
use crate::protocol::LinkReportEntry;
use super::types::{Cache, CacheEntry, FileInfo, RunMetrics, RunWarnings};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("dejaview_{prefix}_{}_{}", std::process::id(), ts))
}

fn make_config(downloads: &Path, media: &Path, min_age: u64) -> Config {
    Config {
        downloads: Some(downloads.to_path_buf()),
        media: Some(media.to_path_buf()),
        min_age: Some(min_age),
        threads: None,
    }
}

fn write_file(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

fn sample_file_info(path: &str, dev: u64, inode: u64, in_downloads: bool) -> FileInfo {
    FileInfo {
        path: PathBuf::from(path),
        size: 10,
        mtime_ns: 0,
        ctime_ns: 0,
        dev,
        inode,
        hash: Some("h".into()),
        in_downloads,
    }
}

fn run_scan(config: &Config, dry_run: bool, strict: bool, report_path: Option<&Path>) {
    run_scan_with_options(
        config,
        ScanRunOptions {
            dry_run,
            strict,
            report_path,
            json_output: false,
            show_progress: false,
            require_confirm: false,
            tui_events: false,
        },
    )
    .expect("run scan");
}

fn running_as_root() -> bool {
    let output = Command::new("id").arg("-u").output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "0",
        _ => false,
    }
}

#[test]
fn allows_target_inside_media_root() {
    let root = unique_temp_dir("inside");
    let media = root.join("media");
    let nested = media.join("movies");
    let target = nested.join("a.mkv");
    fs::create_dir_all(&nested).expect("create media dir");
    fs::write(&target, b"content").expect("write target");

    let result = ensure_target_within_media_root(&media, &target);
    assert!(result.is_ok());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn rejects_target_outside_media_root() {
    let root = unique_temp_dir("outside");
    let media = root.join("media");
    let other = root.join("other");
    let target = other.join("b.mkv");
    fs::create_dir_all(&media).expect("create media dir");
    fs::create_dir_all(&other).expect("create other dir");
    fs::write(&target, b"content").expect("write target");

    let result = ensure_target_within_media_root(&media, &target);
    assert!(result.is_err());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn replaces_file_with_relative_symlink_atomically() {
    let root = unique_temp_dir("replace");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads dir");
    fs::create_dir_all(&media).expect("create media dir");

    let src = downloads.join("duplicate.mkv");
    let tgt = media.join("canonical.mkv");
    fs::write(&src, b"old").expect("write source");
    fs::write(&tgt, b"new").expect("write target");

    replace_file_with_relative_symlink(&src, &tgt).expect("replace with symlink");

    let meta = fs::symlink_metadata(&src).expect("metadata");
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&src).expect("read link"),
        PathBuf::from("../media/canonical.mkv")
    );
    assert_eq!(fs::read(&src).expect("read through link"), b"new");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn replace_file_with_best_link_falls_back_to_symlink_on_hardlink_error() {
    let root = unique_temp_dir("fallback_symlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads dir");
    fs::create_dir_all(&media).expect("create media dir");

    let src = downloads.join("duplicate.mkv");
    let tgt = media.join("canonical.mkv");
    fs::write(&src, b"old").expect("write source");
    fs::write(&tgt, b"new").expect("write target");

    let result = replace_file_with_best_link_with(&src, &tgt, |_old, _new| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "simulated hardlink failure",
        ))
    })
    .expect("replace with fallback");

    assert_eq!(result.0, "symlink");
    assert!(
        result
            .1
            .expect("fallback reason")
            .contains("hardlink failed")
    );
    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_symlink());
    assert_eq!(fs::read(&src).expect("read through symlink"), b"new");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

fn planned_entry_for(action: &super::types::LinkAction) -> LinkReportEntry {
    LinkReportEntry {
        source: action.src.display().to_string(),
        target: action.tgt.display().to_string(),
        size: action.size,
        status: "planned".to_string(),
        method: None,
        fallback_reason: None,
        error: None,
    }
}

#[test]
fn clear_cache_file_is_idempotent() {
    let root = unique_temp_dir("cache_clear");
    fs::create_dir_all(&root).expect("create temp dir");
    let cache = root.join(".dejaview_cache.json");

    clear_cache_file(&cache).expect("first clear on missing file");
    fs::write(&cache, b"{}").expect("write cache");
    clear_cache_file(&cache).expect("second clear removes file");
    clear_cache_file(&cache).expect("third clear still ok");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn save_cache_writes_json() {
    let root = unique_temp_dir("save_cache");
    fs::create_dir_all(&root).expect("create temp dir");
    let cache_path = root.join(".dejaview_cache.json");
    let mut cache = Cache::new();
    cache.insert(
        "foo".into(),
        CacheEntry {
            size: 42,
            mtime_ns: 123,
            ctime_ns: 456,
            dev: 7,
            inode: 8,
            hash: "abc".into(),
        },
    );

    save_cache(&cache_path, &cache).expect("save cache");
    let loaded = load_cache(&cache_path);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded.get("foo").expect("foo key").hash, "abc");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn plan_group_actions_is_deterministic() {
    let later_media = sample_file_info("/media/z-last.mkv", 1, 2, false);
    let first_media = sample_file_info("/media/a-first.mkv", 1, 3, false);
    let download_b = sample_file_info("/downloads/b.mkv", 2, 4, true);
    let download_a = sample_file_info("/downloads/a.mkv", 2, 5, true);
    let group = vec![&later_media, &download_b, &first_media, &download_a];

    let actions = plan_group_actions(&group);
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0].src, PathBuf::from("/downloads/a.mkv"));
    assert_eq!(actions[1].src, PathBuf::from("/downloads/b.mkv"));
    assert_eq!(actions[0].tgt, PathBuf::from("/media/a-first.mkv"));
    assert_eq!(actions[1].tgt, PathBuf::from("/media/a-first.mkv"));
}

#[test]
fn plan_group_actions_skips_already_hardlinked_source() {
    let media = sample_file_info("/media/a.mkv", 1, 111, false);
    let download = sample_file_info("/downloads/a.mkv", 1, 111, true);
    let actions = plan_group_actions(&[&media, &download]);
    assert!(actions.is_empty());
}

#[test]
fn plan_group_actions_requires_both_sides() {
    let media_only = sample_file_info("/media/a.mkv", 1, 1, false);
    let downloads_only = sample_file_info("/downloads/a.mkv", 2, 2, true);
    assert!(plan_group_actions(&[&media_only]).is_empty());
    assert!(plan_group_actions(&[&downloads_only]).is_empty());
}

#[test]
fn collect_files_strict_fails_on_walk_error() {
    let missing = PathBuf::from("/this/path/does/not/exist/dejaview");
    let mut warnings = RunWarnings::default();
    let mut metrics = RunMetrics::default();
    let result = collect_files(
        &missing,
        true,
        Duration::from_secs(0),
        true,
        false,
        &mut warnings,
        &mut metrics,
    );
    assert!(result.is_err());
    assert!(warnings.walk_errors > 0);
}

#[test]
fn run_scan_dry_run_does_not_replace_file() {
    let root = unique_temp_dir("dry_run");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, true, false, None);

    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_file());
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_apply_prefers_hardlink_when_possible() {
    let root = unique_temp_dir("apply_run");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, None);

    let src_meta = fs::metadata(&src).expect("source metadata");
    let tgt_meta = fs::metadata(&tgt).expect("target metadata");
    assert_eq!(src_meta.dev(), tgt_meta.dev());
    assert_eq!(src_meta.ino(), tgt_meta.ino());
    assert_eq!(
        fs::read(&src).expect("read through hardlink"),
        b"same-content"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_writes_link_report_manifest() {
    let root = unique_temp_dir("report_manifest");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let report = root.join("report.json");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, Some(&report));

    let report_json = fs::read_to_string(&report).expect("read report");
    let report_value: serde_json::Value =
        serde_json::from_str(&report_json).expect("parse report json");
    assert_eq!(
        report_value["planned_actions"]
            .as_u64()
            .expect("planned actions"),
        1
    );
    assert_eq!(
        report_value["applied_actions"]
            .as_u64()
            .expect("applied actions"),
        1
    );
    assert_eq!(
        report_value["entries"][0]["method"]
            .as_str()
            .expect("method"),
        "hardlink"
    );
    assert_eq!(
        report_value["entries"][0]["status"]
            .as_str()
            .expect("status"),
        "applied"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn apply_actions_rejects_source_metadata_change_after_planning() {
    let root = unique_temp_dir("source_changed");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");

    let src_meta = fs::metadata(&src).expect("src metadata");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
    let action = super::types::LinkAction {
        src: src.clone(),
        tgt: tgt.clone(),
        size: src_meta.len(),
        src_snapshot: super::types::FileSnapshot {
            size: src_meta.len(),
            mtime_ns: src_meta
                .modified()
                .expect("src modified")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("src mtime")
                .as_nanos(),
            ctime_ns: (src_meta.ctime() as i128 * 1_000_000_000) + src_meta.ctime_nsec() as i128,
            dev: src_meta.dev(),
            inode: src_meta.ino(),
        },
        tgt_snapshot: super::types::FileSnapshot {
            size: tgt_meta.len(),
            mtime_ns: tgt_meta
                .modified()
                .expect("tgt modified")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("tgt mtime")
                .as_nanos(),
            ctime_ns: (tgt_meta.ctime() as i128 * 1_000_000_000) + tgt_meta.ctime_nsec() as i128,
            dev: tgt_meta.dev(),
            inode: tgt_meta.ino(),
        },
    };
    std::thread::sleep(Duration::from_millis(2));
    write_file(&src, b"changed-content");

    let mut warnings = RunWarnings::default();
    let mut report_entries = vec![planned_entry_for(&action)];
    let (applied, strict_error) = apply_actions(
        &[action],
        &media,
        false,
        true,
        false,
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 0);
    assert!(strict_error.is_none());
    assert_eq!(warnings.link_errors, 1);
    assert_eq!(report_entries[0].status, "failed");
    assert!(
        report_entries[0]
            .error
            .as_deref()
            .expect("error message")
            .contains("source changed since planning")
    );
    assert_eq!(fs::read(&src).expect("read src"), b"changed-content");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn apply_actions_rejects_target_metadata_change_after_planning() {
    let root = unique_temp_dir("target_changed");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");

    let src_meta = fs::metadata(&src).expect("src metadata");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
    let action = super::types::LinkAction {
        src: src.clone(),
        tgt: tgt.clone(),
        size: src_meta.len(),
        src_snapshot: super::types::FileSnapshot {
            size: src_meta.len(),
            mtime_ns: src_meta
                .modified()
                .expect("src modified")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("src mtime")
                .as_nanos(),
            ctime_ns: (src_meta.ctime() as i128 * 1_000_000_000) + src_meta.ctime_nsec() as i128,
            dev: src_meta.dev(),
            inode: src_meta.ino(),
        },
        tgt_snapshot: super::types::FileSnapshot {
            size: tgt_meta.len(),
            mtime_ns: tgt_meta
                .modified()
                .expect("tgt modified")
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("tgt mtime")
                .as_nanos(),
            ctime_ns: (tgt_meta.ctime() as i128 * 1_000_000_000) + tgt_meta.ctime_nsec() as i128,
            dev: tgt_meta.dev(),
            inode: tgt_meta.ino(),
        },
    };
    std::thread::sleep(Duration::from_millis(2));
    write_file(&tgt, b"changed-content");

    let mut warnings = RunWarnings::default();
    let mut report_entries = vec![planned_entry_for(&action)];
    let (applied, strict_error) = apply_actions(
        &[action],
        &media,
        false,
        true,
        false,
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 0);
    assert!(strict_error.is_none());
    assert_eq!(warnings.link_errors, 1);
    assert_eq!(report_entries[0].status, "failed");
    assert!(
        report_entries[0]
            .error
            .as_deref()
            .expect("error message")
            .contains("target changed since planning")
    );
    let meta = fs::symlink_metadata(&src).expect("src metadata after reject");
    assert!(meta.file_type().is_file());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_report_write_refuses_symlink_target() {
    #[cfg(not(unix))]
    {
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("report_symlink");
        let downloads = root.join("downloads");
        let media = root.join("media");
        let src = downloads.join("movie.mkv");
        let tgt = media.join("movie.mkv");
        let victim = root.join("victim.json");
        write_file(&src, b"same-content");
        write_file(&tgt, b"same-content");
        write_file(&victim, b"keep");
        fs::create_dir_all(&downloads).expect("create downloads");
        symlink(&victim, downloads.join(".dejaview_link_report.json")).expect("symlink report");
        let config = make_config(&downloads, &media, 0);

        run_scan(&config, false, false, None);

        assert_eq!(fs::read(&victim).expect("read victim"), b"keep");
        let src_meta = fs::metadata(&src).expect("src metadata");
        let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
        assert_eq!(src_meta.ino(), tgt_meta.ino());

        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }
}

#[test]
fn run_scan_report_write_failure_is_non_fatal() {
    if running_as_root() {
        return;
    }
    let root = unique_temp_dir("report_write_failure");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let report_dir = root.join("readonly_report");
    let report_path = report_dir.join("report.json");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    fs::create_dir_all(&report_dir).expect("create report dir");
    let mut perms = fs::metadata(&report_dir)
        .expect("report dir metadata")
        .permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&report_dir, perms).expect("make report dir readonly");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, Some(&report_path));
    let src_meta = fs::metadata(&src).expect("src metadata");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
    assert_eq!(src_meta.ino(), tgt_meta.ino());
    assert_eq!(src_meta.dev(), tgt_meta.dev());

    let mut reset = fs::metadata(&report_dir)
        .expect("report dir metadata")
        .permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&report_dir, reset).expect("reset perms");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_respects_min_age_filter() {
    let root = unique_temp_dir("min_age");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let config = make_config(&downloads, &media, 3600);

    run_scan(&config, false, false, None);

    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_file());
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_strict_fails_on_sample_hash_error() {
    if running_as_root() {
        return;
    }
    let root = unique_temp_dir("strict_hash");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("bad.mkv");
    let tgt = media.join("bad.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let mut perms = fs::metadata(&src).expect("src metadata").permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&src, perms).expect("make unreadable");
    let config = make_config(&downloads, &media, 0);

    let strict_result = run_scan_with_options(
        &config,
        ScanRunOptions {
            dry_run: true,
            strict: true,
            report_path: None,
            json_output: false,
            show_progress: false,
            require_confirm: false,
            tui_events: false,
        },
    );
    assert!(strict_result.is_err());
    let non_strict_result = run_scan_with_options(
        &config,
        ScanRunOptions {
            dry_run: true,
            strict: false,
            report_path: None,
            json_output: false,
            show_progress: false,
            require_confirm: false,
            tui_events: false,
        },
    );
    assert!(non_strict_result.is_ok());

    let mut reset = fs::metadata(&src).expect("src metadata").permissions();
    reset.set_mode(0o644);
    fs::set_permissions(&src, reset).expect("reset perms");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_non_strict_link_failure_continues() {
    if running_as_root() {
        return;
    }
    let root = unique_temp_dir("link_fail_non_strict");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src_dir = downloads.join("readonly");
    let src = src_dir.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let mut dir_perms = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    dir_perms.set_mode(0o555);
    fs::set_permissions(&src_dir, dir_perms).expect("make dir readonly");
    let config = make_config(&downloads, &media, 0);

    let result = run_scan_with_options(
        &config,
        ScanRunOptions {
            dry_run: false,
            strict: false,
            report_path: None,
            json_output: false,
            show_progress: false,
            require_confirm: false,
            tui_events: false,
        },
    );
    assert!(result.is_ok());
    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_file());

    let mut reset = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&src_dir, reset).expect("reset dir perms");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_strict_link_failure_returns_error() {
    if running_as_root() {
        return;
    }
    let root = unique_temp_dir("link_fail_strict");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src_dir = downloads.join("readonly");
    let src = src_dir.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let mut dir_perms = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    dir_perms.set_mode(0o555);
    fs::set_permissions(&src_dir, dir_perms).expect("make dir readonly");
    let config = make_config(&downloads, &media, 0);

    let result = run_scan_with_options(
        &config,
        ScanRunOptions {
            dry_run: false,
            strict: true,
            report_path: None,
            json_output: false,
            show_progress: false,
            require_confirm: false,
            tui_events: false,
        },
    );
    assert!(result.is_err());
    let err = result.expect_err("strict link failure expected");
    assert!(
        err.to_string().contains("strict mode link failure"),
        "unexpected strict error: {err}"
    );

    let mut reset = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&src_dir, reset).expect("reset dir perms");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn run_scan_creates_and_reuses_cache_file() {
    let root = unique_temp_dir("cache_reuse");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, true, false, None);
    let cache_file = downloads.join(".dejaview_cache.json");
    assert!(cache_file.exists());
    let first_cache = load_cache(&cache_file);
    assert!(!first_cache.is_empty());

    run_scan(&config, true, false, None);
    let second_cache = load_cache(&cache_file);
    assert_eq!(second_cache.len(), first_cache.len());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn save_cache_failure_cleans_up_temp_file() {
    if running_as_root() {
        return;
    }
    let root = unique_temp_dir("cache_fail");
    fs::create_dir_all(&root).expect("create root");
    let cache_file = root.join(".dejaview_cache.json");
    let mut perms = fs::metadata(&root).expect("root metadata").permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&root, perms).expect("set readonly");

    let mut cache = Cache::new();
    cache.insert(
        "k".into(),
        CacheEntry {
            size: 1,
            mtime_ns: 1,
            ctime_ns: 2,
            dev: 3,
            inode: 4,
            hash: "h".into(),
        },
    );
    let result = save_cache(&cache_file, &cache);
    assert!(result.is_err());

    let entries = fs::read_dir(&root)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".dejaview_write.tmp.")
        })
        .count();
    assert_eq!(entries, 0);

    let mut reset = fs::metadata(&root).expect("root metadata").permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&root, reset).expect("reset perms");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}
