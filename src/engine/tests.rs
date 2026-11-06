use super::*;
use crate::config::Config;
use crate::test_common::TempDir;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::cache::{clear_cache_file, load_cache, save_cache};
use super::collect::{
    CollectOptions, Exclude, RootId, collect_files, collect_run_files, group_files_by_size,
};
use super::linking::{
    Applied, ApplyOptions, ApplyOutcome, CanonicalRoots, LinkRequest, Linked, apply_actions,
    ensure_target_within_media_root, plan_group_actions, replace_file_with_best_link_with,
};
use super::types::{Cache, CacheEntry, FileInfo, FileSnapshot, LinkMode, RunMetrics};
use crate::protocol::{EntryStatus, LinkMethod, LinkReportEntry};

fn make_config(downloads: &Path, media: &Path, min_age: u64) -> Config {
    Config {
        downloads: Some(downloads.to_path_buf()),
        media: Some(media.to_path_buf()),
        min_age: Some(min_age),
        // Test files are a few bytes; only the empty-file rule applies.
        min_size: Some(1),
        threads: None,
        full_threads: None,
        exclude: None,
        link_mode: Some(LinkMode::Best),
    }
}

fn write_file(path: &Path, content: &[u8]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, content).expect("write file");
}

/// A file whose ctime is its inode number, so different inodes never share
/// timestamps by accident and paths of one inode always do.
fn sample_file_info(path: &str, dev: u64, inode: u64, in_downloads: bool) -> FileInfo {
    FileInfo {
        path: PathBuf::from(path),
        snapshot: FileSnapshot {
            size: 10,
            mtime_ns: 0,
            ctime_ns: inode as i128,
            dev,
            inode,
        },
        hash: Some("h".into()),
        sample: None,
        in_downloads,
    }
}

fn run_scan(config: &Config, dry_run: bool, strict: bool, report_path: Option<&Path>) {
    run_scan_with_options(
        config,
        ScanRunOptions {
            strict,
            report_path,
            ..ScanRunOptions::for_test(dry_run)
        },
    )
    .expect("run scan");
}

/// The downloads root for a direct collect_files call. A missing path gets
/// placeholder ids, since the walk reports it as an error anyway.
fn test_root(path: &Path) -> RootId {
    RootId::of("downloads", path).unwrap_or(RootId {
        name: "downloads",
        path: path.to_path_buf(),
        canonical: path.to_path_buf(),
        dev: u64::MAX,
        ino: u64::MAX,
    })
}

/// A media root no directory in a test tree can match.
fn unrelated_root() -> RootId {
    RootId {
        name: "media",
        path: PathBuf::from("/nonexistent/media"),
        canonical: PathBuf::from("/nonexistent/media"),
        dev: u64::MAX - 1,
        ino: u64::MAX - 1,
    }
}

/// Run one replacement with injected link and rename steps.
fn link_with<F, G>(
    src: &Path,
    tgt: &Path,
    downloads: &Path,
    media: &Path,
    link_mode: LinkMode,
    hard_link_fn: F,
    rename_fn: G,
) -> anyhow::Result<Linked>
where
    F: Fn(&Path, &Path) -> std::io::Result<()>,
    G: Fn(&Path, &Path) -> std::io::Result<()>,
{
    let roots = CanonicalRoots::new(downloads, media).expect("resolve roots");
    let src_snapshot = snapshot_of(src);
    let tgt_snapshot = snapshot_of(tgt);
    replace_file_with_best_link_with(
        &LinkRequest {
            src,
            src_snapshot: &src_snapshot,
            tgt,
            tgt_snapshot: &tgt_snapshot,
            roots: &roots,
            link_mode,
        },
        hard_link_fn,
        rename_fn,
    )
}

/// A small pool for walks a test runs directly.
fn test_pool() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("build test pool")
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
    let root = TempDir::new("inside");
    let media = root.join("media");
    let nested = media.join("movies");
    let target = nested.join("a.mkv");
    fs::create_dir_all(&nested).expect("create media dir");
    fs::write(&target, b"content").expect("write target");

    let result = ensure_target_within_media_root(&media, &target);
    assert!(result.is_ok());
}

#[test]
fn rejects_target_outside_media_root() {
    let root = TempDir::new("outside");
    let media = root.join("media");
    let other = root.join("other");
    let target = other.join("b.mkv");
    fs::create_dir_all(&media).expect("create media dir");
    fs::create_dir_all(&other).expect("create other dir");
    fs::write(&target, b"content").expect("write target");

    let result = ensure_target_within_media_root(&media, &target);
    assert!(result.is_err());
}

#[test]
fn rejects_symlink_target_inside_media_root() {
    #[cfg(not(unix))]
    {
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let root = TempDir::new("symlink_target");
        let media = root.join("media");
        let real = root.join("real");
        let target = media.join("movie.mkv");
        fs::create_dir_all(&media).expect("create media dir");
        fs::create_dir_all(&real).expect("create real dir");
        fs::write(real.join("movie.mkv"), b"content").expect("write real target");
        symlink(real.join("movie.mkv"), &target).expect("create target symlink");

        let result = ensure_target_within_media_root(&media, &target);
        assert!(result.is_err());
        assert!(
            result
                .expect_err("symlink target should fail")
                .to_string()
                .contains("refusing to link to symlink target")
        );
    }
}

#[test]
fn replaces_file_with_relative_symlink_atomically() {
    let root = TempDir::new("replace");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads dir");
    fs::create_dir_all(&media).expect("create media dir");

    let src = downloads.join("duplicate.mkv");
    let tgt = media.join("canonical.mkv");
    fs::write(&src, b"old").expect("write source");
    fs::write(&tgt, b"new").expect("write target");

    let linked = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::SymlinkOnly,
        |from, to| fs::hard_link(from, to),
        |from, to| fs::rename(from, to),
    )
    .expect("replace with symlink");
    assert_eq!(linked.method, LinkMethod::Symlink);

    let meta = fs::symlink_metadata(&src).expect("metadata");
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&src).expect("read link"),
        PathBuf::from("../media/canonical.mkv")
    );
    assert_eq!(fs::read(&src).expect("read through link"), b"new");
}

#[test]
fn replace_file_with_best_link_falls_back_to_symlink_on_hardlink_error() {
    let root = TempDir::new("fallback_symlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads dir");
    fs::create_dir_all(&media).expect("create media dir");

    let src = downloads.join("duplicate.mkv");
    let tgt = media.join("canonical.mkv");
    fs::write(&src, b"old").expect("write source");
    fs::write(&tgt, b"new").expect("write target");

    let result = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        |_old, _new| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated hardlink failure",
            ))
        },
        |from, to| fs::rename(from, to),
    )
    .expect("replace with fallback");

    assert_eq!(result.method, LinkMethod::Symlink);
    let fallback_reason = result.fallback_reason.expect("fallback reason");
    assert!(fallback_reason.contains("hardlink failed"));
    assert!(fallback_reason.contains("symlink applied successfully"));
    assert!(fallback_reason.contains("temp hardlink"));
    assert!(
        fallback_reason.contains("simulated hardlink failure"),
        "names the OS error: {fallback_reason}"
    );
    assert!(
        fallback_reason.contains("fs.protected_hardlinks"),
        "names the likely cause: {fallback_reason}"
    );
    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_symlink());
    assert_eq!(fs::read(&src).expect("read through symlink"), b"new");
}

#[test]
fn hardlink_replace_cleans_up_temp_name_that_survives_rename() {
    let root = TempDir::new("leftover_temp_hardlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads dir");
    fs::create_dir_all(&media).expect("create media dir");

    let src = downloads.join("duplicate.mkv");
    let tgt = media.join("canonical.mkv");
    fs::write(&src, b"old").expect("write source");
    fs::write(&tgt, b"new").expect("write target");

    let result = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        |from, to| fs::hard_link(from, to),
        |from, to| {
            fs::copy(from, to)?;
            Ok(())
        },
    );

    let linked = result.expect("leftover temp link should be cleaned up");
    assert!(
        linked.already_linked,
        "a surviving temp name means one file"
    );
    let leftovers = fs::read_dir(&downloads)
        .expect("read downloads dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".dejaview_hardlink.tmp.")
        })
        .count();
    assert_eq!(leftovers, 0);
}

fn planned_entry_for(action: &super::types::LinkAction) -> LinkReportEntry {
    LinkReportEntry {
        source: action.src.display().to_string(),
        target: action.tgt.display().to_string(),
        size: action.size,
        status: EntryStatus::Planned,
        method: None,
        fallback_reason: None,
        error: None,
        ..Default::default()
    }
}

#[test]
fn clear_cache_file_is_idempotent() {
    let root = TempDir::new("cache_clear");
    let cache = root.join(".dejaview_cache.json");

    clear_cache_file(&cache).expect("first clear on missing file");
    fs::write(&cache, b"{}").expect("write cache");
    clear_cache_file(&cache).expect("second clear removes file");
    clear_cache_file(&cache).expect("third clear still ok");
}

#[test]
fn save_cache_writes_json() {
    let root = TempDir::new("save_cache");
    let cache_path = root.join(".dejaview_cache.json");
    let mut cache = Cache::new();
    cache.insert(
        "foo".into(),
        CacheEntry {
            snapshot: FileSnapshot {
                size: 42,
                mtime_ns: 123,
                ctime_ns: 456,
                dev: 7,
                inode: 8,
            },
            hash: Some("abc".into()),
            sample: None,
        },
    );

    save_cache(&cache_path, &cache).expect("save cache");
    let loaded = load_cache(&cache_path);
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded.get("foo").expect("foo key").hash.as_deref(),
        Some("abc")
    );
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
fn plan_group_actions_skips_download_already_linked_to_another_library_copy() {
    // The library holds two identical copies; the *arr hardlink joins the
    // download to the second one, which does not sort first.
    let first_copy = sample_file_info("/media/A Movies/m.mkv", 1, 10, false);
    let second_copy = sample_file_info("/media/B Movies/m.mkv", 1, 11, false);
    let download = sample_file_info("/downloads/m.mkv", 1, 11, true);
    let actions = plan_group_actions(&[&first_copy, &second_copy, &download]);
    assert!(
        actions.is_empty(),
        "relinking would break the existing hardlink and free nothing"
    );
}

#[test]
fn plan_group_actions_skips_library_file_seen_through_a_second_mount() {
    // mergerfs: downloads points at a branch that holds the library, media at
    // the pool. One file shows up on two devices with the same size and
    // timestamps; its only copy must not be replaced.
    let pool_view = sample_file_info("/pool/media/m.mkv", 1, 10, false);
    let mut branch_view = sample_file_info("/disk1/media/m.mkv", 2, 500, true);
    branch_view.snapshot.ctime_ns = pool_view.snapshot.ctime_ns;
    assert!(plan_group_actions(&[&pool_view, &branch_view]).is_empty());

    // The same holds when the library has a second copy that sorts first: the
    // branch view of B must not be paired with A.
    let copy_a = sample_file_info("/pool/media/A/m.mkv", 1, 10, false);
    let copy_b = sample_file_info("/pool/media/B/m.mkv", 1, 11, false);
    let mut branch_b = sample_file_info("/disk1/media/B/m.mkv", 2, 501, true);
    branch_b.snapshot.ctime_ns = copy_b.snapshot.ctime_ns;
    assert!(plan_group_actions(&[&copy_a, &copy_b, &branch_b]).is_empty());

    // A real copy on another device has its own ctime and is still planned.
    let real_copy = sample_file_info("/downloads/m.mkv", 2, 502, true);
    assert_eq!(plan_group_actions(&[&pool_view, &real_copy]).len(), 1);
}

#[test]
fn link_run_keeps_a_download_hardlinked_to_a_second_library_copy() {
    let root = TempDir::new("second_library_copy");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let copy_a = media.join("A Movies").join("m.mkv");
    let copy_b = media.join("B Movies").join("m.mkv");
    let download = downloads.join("m.mkv");
    write_file(&copy_a, b"same-movie-bytes");
    write_file(&copy_b, b"same-movie-bytes");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::hard_link(&copy_b, &download).expect("arr hardlink");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, None);

    assert_eq!(
        fs::metadata(&download).expect("download").ino(),
        fs::metadata(&copy_b).expect("copy b").ino(),
        "the download keeps its hardlink to B"
    );
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
    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();
    let result = collect_files(
        &test_root(&missing),
        &unrelated_root(),
        true,
        Duration::from_secs(0),
        &CollectOptions {
            min_size: 1,
            exclude: &Exclude::default(),
            strict: true,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut metrics,
    );
    assert!(result.is_err());
    assert!(warnings.walk_errors > 0);
}

#[test]
fn partially_linked_groups_still_plan_remaining_download_duplicates() {
    let root = TempDir::new("partial_group");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let linked_download = downloads.join("movie.mkv");
    let remaining_download = downloads.join("movie-copy.mkv");
    let target = media.join("movie.mkv");
    write_file(&remaining_download, b"same-content");
    write_file(&target, b"same-content");
    fs::create_dir_all(linked_download.parent().expect("downloads parent"))
        .expect("create downloads");
    fs::hard_link(&target, &linked_download).expect("create already-linked download");

    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();
    let mut files = collect_run_files(
        &downloads,
        &media,
        Duration::from_secs(0),
        CollectOptions {
            min_size: 1,
            exclude: &Exclude::default(),
            strict: false,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &test_pool(),
        &mut warnings,
        &mut metrics,
    )
    .expect("collect run files");
    assert_eq!(files.len(), 3);

    for file in &mut files {
        file.hash = Some("same-hash".to_string());
    }

    let groups = group_files_by_size(files);
    let actions = plan_actions(&groups).actions;
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].src, remaining_download);
    assert_eq!(actions[0].tgt, target);
}

#[test]
fn run_scan_dry_run_does_not_replace_file() {
    let root = TempDir::new("dry_run");
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
}

#[test]
fn run_scan_apply_prefers_hardlink_when_possible() {
    let root = TempDir::new("apply_run");
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
}

#[test]
fn run_scan_writes_link_report_manifest() {
    let root = TempDir::new("report_manifest");
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
}

#[test]
fn apply_actions_rejects_source_metadata_change_after_planning() {
    let root = TempDir::new("source_changed");
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
        src_snapshot: FileSnapshot::from_metadata(&src_meta),
        tgt_snapshot: FileSnapshot::from_metadata(&tgt_meta),
    };
    std::thread::sleep(Duration::from_millis(2));
    write_file(&src, b"changed-content");

    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![planned_entry_for(&action)];
    let ApplyOutcome {
        counts: Applied { applied, .. },
        strict_error,
        ..
    } = apply_actions(
        &[action],
        &media,
        &ApplyOptions {
            downloads: &downloads,
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 0);
    assert!(strict_error.is_none());
    assert_eq!(warnings.link_errors, 1);
    assert_eq!(report_entries[0].status, EntryStatus::Failed);
    assert!(
        report_entries[0]
            .error
            .as_deref()
            .expect("error message")
            .contains("source changed since planning")
    );
    assert_eq!(fs::read(&src).expect("read src"), b"changed-content");
}

#[test]
fn apply_actions_rejects_target_metadata_change_after_planning() {
    let root = TempDir::new("target_changed");
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
        src_snapshot: FileSnapshot::from_metadata(&src_meta),
        tgt_snapshot: FileSnapshot::from_metadata(&tgt_meta),
    };
    std::thread::sleep(Duration::from_millis(2));
    write_file(&tgt, b"changed-content");

    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![planned_entry_for(&action)];
    let ApplyOutcome {
        counts: Applied { applied, .. },
        strict_error,
        ..
    } = apply_actions(
        &[action],
        &media,
        &ApplyOptions {
            downloads: &downloads,
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 0);
    assert!(strict_error.is_none());
    assert_eq!(warnings.link_errors, 1);
    assert_eq!(report_entries[0].status, EntryStatus::Failed);
    assert!(
        report_entries[0]
            .error
            .as_deref()
            .expect("error message")
            .contains("target changed since planning")
    );
    let meta = fs::symlink_metadata(&src).expect("src metadata after reject");
    assert!(meta.file_type().is_file());
}

#[test]
fn apply_actions_allow_multiple_sources_for_same_target_in_one_run() {
    let root = TempDir::new("multi_source_same_target");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src_a = downloads.join("movie-a.mkv");
    let src_b = downloads.join("movie-b.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src_a, b"same-content");
    write_file(&src_b, b"same-content");
    write_file(&tgt, b"same-content");

    let src_a_meta = fs::metadata(&src_a).expect("src_a metadata");
    let src_b_meta = fs::metadata(&src_b).expect("src_b metadata");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
    let tgt_snapshot = FileSnapshot::from_metadata(&tgt_meta);
    let actions = vec![
        super::types::LinkAction {
            src: src_a.clone(),
            tgt: tgt.clone(),
            size: src_a_meta.len(),
            src_snapshot: FileSnapshot::from_metadata(&src_a_meta),
            tgt_snapshot,
        },
        super::types::LinkAction {
            src: src_b.clone(),
            tgt: tgt.clone(),
            size: src_b_meta.len(),
            src_snapshot: FileSnapshot::from_metadata(&src_b_meta),
            tgt_snapshot,
        },
    ];

    let mut warnings = WarningSummary::default();
    let mut report_entries = actions.iter().map(planned_entry_for).collect::<Vec<_>>();
    let ApplyOutcome {
        counts: Applied { applied, .. },
        strict_error,
        ..
    } = apply_actions(
        &actions,
        &media,
        &ApplyOptions {
            downloads: &downloads,
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 2);
    assert!(strict_error.is_none());
    assert_eq!(warnings.link_errors, 0);
    assert!(
        report_entries
            .iter()
            .all(|entry| entry.status == EntryStatus::Applied)
    );

    let src_a_meta = fs::metadata(&src_a).expect("src_a metadata after apply");
    let src_b_meta = fs::metadata(&src_b).expect("src_b metadata after apply");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata after apply");
    assert_eq!(src_a_meta.ino(), tgt_meta.ino());
    assert_eq!(src_b_meta.ino(), tgt_meta.ino());
    assert_eq!(src_a_meta.dev(), tgt_meta.dev());
    assert_eq!(src_b_meta.dev(), tgt_meta.dev());
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

        let root = TempDir::new("report_symlink");
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
    }
}

#[test]
fn run_scan_report_write_failure_is_non_fatal() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("report_write_failure");
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
}

/// A link leaves files younger than `min_age` alone, and links the same
/// pair once no minimum age keeps it out: the second run is the control that
/// shows the first had something to leave alone.
#[test]
fn run_scan_respects_min_age_filter() {
    let root = TempDir::new("min_age");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let inode = |path: &Path| fs::metadata(path).expect("metadata").ino();
    let src_inode = inode(&src);
    let report = root.join("report.json");

    run_scan(
        &make_config(&downloads, &media, 3600),
        false,
        false,
        Some(&report),
    );
    assert_eq!(inode(&src), src_inode, "an hour old at most: left alone");
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).expect("report")).expect("parse report");
    assert_eq!(written["skipped_by_min_age"], 2);
    assert_eq!(written["planned_actions"], 0);

    run_scan(&make_config(&downloads, &media, 0), false, false, None);
    assert_eq!(inode(&src), inode(&tgt), "with no minimum age, linked");
}

/// A link run that links one pair and fails the other finishes, exits with
/// code 2 (ADR 0003), and reports each pair as it ended: the failure with
/// its OS error and no planned method, and only the linked pair's bytes as
/// freed. Root writes into any folder, so as root this returns early; `just
/// test` runs it again as nobody.
#[test]
fn a_link_run_with_one_failed_pair_reports_each_pair_as_it_ended() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("partial_failure");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("locked/b.mkv"), b"second-content");
    write_file(&downloads.join("open/a.mkv"), b"first");
    write_file(&media.join("b.mkv"), b"second-content");
    write_file(&media.join("a.mkv"), b"first");
    let locked = downloads.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).expect("lock folder");
    let report = root.join("report.json");

    let end = run_scan_with_options(
        &make_config(&downloads, &media, 0),
        ScanRunOptions {
            report_path: Some(&report),
            ..ScanRunOptions::for_test(false)
        },
    )
    .expect("a run with a failed pair still finishes");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock folder");

    assert_eq!(end, RunEnd::WithFailures);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).expect("report")).expect("parse report");
    assert_eq!(report["outcome"], "complete");
    assert_eq!(report["planned_actions"], 2);
    assert_eq!(report["applied_actions"], 1);
    assert_eq!(report["warnings"]["link_errors"], 1);
    let failed = &report["entries"][0];
    assert!(
        failed["source"]
            .as_str()
            .is_some_and(|source| source.ends_with("locked/b.mkv"))
    );
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["method"], serde_json::Value::Null);
    assert!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("Permission denied")),
        "{failed}"
    );
    assert_eq!(report["entries"][1]["status"], "applied");
    assert_eq!(report["entries"][1]["method"], "hardlink");
    assert_eq!(report["planned_bytes"], 19);
    assert_eq!(report["freed_bytes"], 5, "only the linked pair's bytes");
    assert_eq!(report["total_saved_bytes"], 5);
}

/// A dashboard link of reviewed rows stopped before its first pair still
/// writes its report: outcome `cancelled`, every pair still planned, nothing
/// freed, and the run ends as cancelled, which exits 1.
#[test]
fn a_link_cancelled_before_its_first_pair_reports_cancelled() {
    let root = TempDir::new("cancelled_report");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let src_inode = fs::metadata(&src).expect("meta").ino();
    let config = make_config(&downloads, &media, 0);
    // The scan caches both hashes, so the reviewed row is planned from the
    // cache and the cancel is first seen when linking starts.
    run_scan(&config, true, false, None);
    let cancel = CancelToken::default();
    cancel.cancel();
    let pairs = [(src.clone(), tgt.clone())];
    let report = root.join("report.json");

    let end = run_scan_with_options(
        &config,
        ScanRunOptions {
            only_pairs: &pairs,
            cancel,
            report_path: Some(&report),
            ..ScanRunOptions::for_test(false)
        },
    )
    .expect("a link cancelled partway returns how it ended");

    assert_eq!(end, RunEnd::Cancelled);
    assert_eq!(fs::metadata(&src).expect("meta").ino(), src_inode);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).expect("report")).expect("parse report");
    assert_eq!(report["outcome"], "cancelled");
    assert_eq!(report["applied_actions"], 0);
    assert_eq!(report["freed_bytes"], 0);
    assert_eq!(report["entries"][0]["status"], "planned");
}

#[test]
fn run_scan_strict_fails_on_sample_hash_error() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("strict_hash");
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
            strict: true,
            ..ScanRunOptions::for_test(true)
        },
    );
    assert!(strict_result.is_err());
    let non_strict_result = run_scan_with_options(&config, ScanRunOptions::for_test(true));
    assert!(non_strict_result.is_ok());

    let mut reset = fs::metadata(&src).expect("src metadata").permissions();
    reset.set_mode(0o644);
    fs::set_permissions(&src, reset).expect("reset perms");
}

#[test]
fn run_scan_non_strict_link_failure_continues() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("link_fail_non_strict");
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

    let result = run_scan_with_options(&config, ScanRunOptions::for_test(false));
    // ADR 0003: exit code 2, finished with a failure.
    assert_eq!(result.expect("the run finishes"), RunEnd::WithFailures);
    let meta = fs::symlink_metadata(&src).expect("source metadata");
    assert!(meta.file_type().is_file());

    let mut reset = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&src_dir, reset).expect("reset dir perms");
}

#[test]
fn run_scan_strict_link_failure_returns_error() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("link_fail_strict");
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
            strict: true,
            ..ScanRunOptions::for_test(false)
        },
    );
    assert!(result.is_err());
    let err = result.expect_err("strict link failure expected");
    assert!(
        err.to_string()
            .contains("--strict stopped the run at its first failed link"),
        "unexpected strict error: {err}"
    );

    let mut reset = fs::metadata(&src_dir)
        .expect("readonly dir metadata")
        .permissions();
    reset.set_mode(0o755);
    fs::set_permissions(&src_dir, reset).expect("reset dir perms");
}

#[test]
fn run_scan_creates_and_reuses_cache_file() {
    let root = TempDir::new("cache_reuse");
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
}

#[test]
fn save_cache_failure_cleans_up_temp_file() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("cache_fail");
    let cache_file = root.join(".dejaview_cache.json");
    let mut perms = fs::metadata(&root).expect("root metadata").permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&root, perms).expect("set readonly");

    let mut cache = Cache::new();
    cache.insert(
        "k".into(),
        CacheEntry {
            snapshot: FileSnapshot {
                size: 1,
                mtime_ns: 1,
                ctime_ns: 2,
                dev: 3,
                inode: 4,
            },
            hash: Some("h".into()),
            sample: None,
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
}

fn snapshot_of(path: &Path) -> FileSnapshot {
    FileSnapshot::from_metadata(&fs::symlink_metadata(path).expect("metadata"))
}

#[test]
fn apply_actions_refuses_to_replace_a_file_inside_the_media_root() {
    let root = TempDir::new("source_in_media");
    let media = root.join("media");
    let canonical = media.join("a.mkv");
    let library_copy = media.join("b.mkv");
    write_file(&canonical, b"same-content");
    write_file(&library_copy, b"same-content");
    let library_copy_inode = fs::metadata(&library_copy).expect("meta").ino();

    let action = super::types::LinkAction {
        src: library_copy.clone(),
        tgt: canonical.clone(),
        size: 12,
        src_snapshot: snapshot_of(&library_copy),
        tgt_snapshot: snapshot_of(&canonical),
    };
    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![planned_entry_for(&action)];
    let ApplyOutcome {
        counts: Applied { applied, .. },
        ..
    } = apply_actions(
        &[action],
        &media,
        &ApplyOptions {
            downloads: &root.join("downloads"),
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    assert_eq!(applied, 0);
    assert_eq!(warnings.link_errors, 1);
    assert!(
        report_entries[0]
            .error
            .as_deref()
            .expect("error message")
            .contains("refusing to replace a file inside the media root")
    );
    assert_eq!(
        fs::metadata(&library_copy).expect("meta").ino(),
        library_copy_inode,
        "library file must not be replaced"
    );
}

#[test]
fn collect_refuses_a_directory_that_is_the_other_root() {
    // Without mount rights, stand in for a bind mount: the other root's device
    // and inode are those of a folder inside the tree being walked.
    let root = TempDir::new("other_root_inside");
    let downloads = root.join("downloads");
    let library_view = downloads.join("data").join("media");
    write_file(&library_view.join("m.mkv"), b"library bytes");
    let mut media = RootId::of("media", &library_view).expect("media id");
    media.path = root.join("media");

    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();
    let err = collect_files(
        &test_root(&downloads),
        &media,
        true,
        Duration::from_secs(0),
        &CollectOptions {
            min_size: 1,
            exclude: &Exclude::default(),
            strict: false,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut metrics,
    )
    .expect_err("the walk must refuse, strict or not");
    let message = err.to_string();
    assert!(message.contains("second mount"), "{message}");
    assert!(
        message.contains(&library_view.display().to_string()),
        "names where: {message}"
    );

    // The same root reached at the top, as when both are one folder.
    let same = RootId::of("media", &downloads).expect("same id");
    assert!(
        super::collect::ensure_root_not_reached(&test_root(&downloads), &same, &Exclude::default())
            .is_err()
    );
    assert!(
        super::collect::ensure_root_not_reached(
            &test_root(&downloads),
            &unrelated_root(),
            &Exclude::default()
        )
        .is_ok()
    );
}

#[test]
fn collect_files_skips_dejaview_own_files() {
    let root = TempDir::new("own_files");
    write_file(&root.join("movie.mkv"), b"content");
    write_file(&root.join(".dejaview_cache.json"), b"{}");
    write_file(&root.join(".dejaview_link_report.json"), b"{}");
    write_file(&root.join(".dejaview_tui_report.json"), b"{}");
    write_file(
        &root.join("sub").join(".dejaview_hardlink.tmp.1.2"),
        b"content",
    );
    write_file(&root.join(".dejaview-notes.txt"), b"not ours");

    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();
    let files = collect_files(
        &test_root(&root),
        &unrelated_root(),
        true,
        Duration::from_secs(0),
        &CollectOptions {
            min_size: 1,
            exclude: &Exclude::default(),
            strict: false,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut metrics,
    )
    .expect("collect files");
    let mut names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();

    assert_eq!(names, vec![".dejaview-notes.txt", "movie.mkv"]);
}

#[test]
fn run_scan_prunes_cache_entries_for_removed_files() {
    let root = TempDir::new("cache_prune");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("keep.mkv"), b"keep-content");
    write_file(&media.join("keep.mkv"), b"keep-content");
    write_file(&downloads.join("gone.mkv"), b"gone-content");
    write_file(&media.join("gone.mkv"), b"gone-content");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, true, false, None);
    let cache_file = downloads.join(".dejaview_cache.json");
    let gone_key = "downloads/gone.mkv".to_string();
    assert!(load_cache(&cache_file).contains_key(&gone_key));

    fs::remove_file(downloads.join("gone.mkv")).expect("remove download");
    run_scan(&config, true, false, None);

    let cache = load_cache(&cache_file);
    assert!(
        !cache.contains_key(&gone_key),
        "removed path must be pruned"
    );
    let keep_key = "downloads/keep.mkv".to_string();
    assert!(
        cache.contains_key(&keep_key),
        "present path must stay cached"
    );
}

#[test]
fn prune_cache_keeps_entries_under_what_the_walk_could_not_read() {
    let entry = CacheEntry {
        snapshot: FileSnapshot {
            size: 1,
            mtime_ns: 0,
            ctime_ns: 0,
            dev: 1,
            inode: 1,
        },
        hash: Some("h".into()),
        sample: None,
    };
    let mut cache: Cache = [
        "media/#recycle/old.mkv",
        "media/#recycle-not/a.mkv",
        "media/locked.mkv",
        "media/gone.mkv",
        "downloads/x/a.mkv",
    ]
    .into_iter()
    .map(|key| (key.to_string(), entry.clone()))
    .collect();
    let seen: std::collections::HashSet<String> =
        ["downloads/x/a.mkv".to_string()].into_iter().collect();
    let unreadable = ["media/#recycle".to_string(), "media/locked.mkv".to_string()];

    let pruned = super::cache::prune_cache(&mut cache, &seen, &unreadable);

    let mut kept: Vec<_> = cache.keys().cloned().collect();
    kept.sort();
    assert_eq!(
        kept,
        [
            "downloads/x/a.mkv",
            "media/#recycle/old.mkv",
            "media/locked.mkv"
        ]
    );
    assert_eq!(pruned, 2, "a sibling that merely shares a prefix is pruned");

    // A root that could not be read keeps everything under it.
    let mut cache: Cache = [("media/a.mkv".to_string(), entry)].into_iter().collect();
    super::cache::prune_cache(&mut cache, &Default::default(), &["media/".to_string()]);
    assert_eq!(cache.len(), 1);
}

#[test]
fn a_folder_the_walk_cannot_read_keeps_its_entries_and_the_rest_is_pruned() {
    if running_as_root() {
        return;
    }
    let root = TempDir::new("prune_unreadable");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let locked = media.join("locked");
    write_file(&downloads.join("kept.mkv"), b"kept-content");
    write_file(&locked.join("kept.mkv"), b"kept-content");
    write_file(&downloads.join("gone.mkv"), b"gone-content");
    write_file(&media.join("gone.mkv"), b"gone-content");
    let config = make_config(&downloads, &media, 0);
    run_scan(&config, true, false, None);
    let cache_file = downloads.join(".dejaview_cache.json");
    assert!(load_cache(&cache_file).contains_key("media/locked/kept.mkv"));

    fs::remove_file(media.join("gone.mkv")).expect("remove library file");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("lock folder");
    let result = run_scan_with_options(&config, ScanRunOptions::for_test(true));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock folder");

    assert_eq!(result.expect("the scan finishes"), RunEnd::WithFailures);
    let cache = load_cache(&cache_file);
    assert!(
        cache.contains_key("media/locked/kept.mkv"),
        "kept under the folder that could not be read"
    );
    assert!(!cache.contains_key("media/gone.mkv"), "pruned elsewhere");
}

#[test]
fn prune_cache_keeps_only_seen_paths() {
    let mut cache: Cache = Cache::new();
    for path in ["/dl/a", "/dl/b", "/media/c"] {
        cache.insert(
            path.to_string(),
            CacheEntry {
                snapshot: FileSnapshot {
                    size: 1,
                    mtime_ns: 0,
                    ctime_ns: 0,
                    dev: 1,
                    inode: 1,
                },
                hash: Some("h".into()),
                sample: None,
            },
        );
    }
    let seen: std::collections::HashSet<String> = ["/dl/a", "/media/c"]
        .iter()
        .map(|p| p.to_string())
        .collect();

    let pruned = super::cache::prune_cache(&mut cache, &seen, &[]);

    assert_eq!(pruned, 1);
    assert!(!cache.contains_key("/dl/b"));
    assert_eq!(cache.len(), 2);
}

#[test]
fn cancelled_link_run_returns_cancelled_and_changes_nothing() {
    let root = TempDir::new("cancelled_run");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("movie.mkv"), b"same-content");
    write_file(&media.join("movie.mkv"), b"same-content");
    let download_inode = fs::metadata(downloads.join("movie.mkv"))
        .expect("meta")
        .ino();
    let config = make_config(&downloads, &media, 0);
    let cancel = CancelToken::default();
    cancel.cancel();

    let err = run_scan_with_options(
        &config,
        ScanRunOptions {
            cancel,
            ..ScanRunOptions::for_test(false)
        },
    )
    .expect_err("a cancelled run returns an error");

    assert!(err.is::<Cancelled>());
    assert_eq!(
        fs::metadata(downloads.join("movie.mkv"))
            .expect("meta")
            .ino(),
        download_inode
    );
    assert!(!downloads.join(".dejaview_link_report.json").exists());
}

#[test]
fn apply_actions_stops_before_the_next_file_when_cancelled() {
    let root = TempDir::new("cancelled_apply");
    let src = root.join("downloads").join("movie.mkv");
    let tgt = root.join("media").join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let src_inode = fs::metadata(&src).expect("meta").ino();
    let action = super::types::LinkAction {
        src: src.clone(),
        tgt: tgt.clone(),
        size: 12,
        src_snapshot: snapshot_of(&src),
        tgt_snapshot: snapshot_of(&tgt),
    };
    let cancel = CancelToken::default();
    cancel.cancel();
    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![planned_entry_for(&action)];

    let outcome = apply_actions(
        &[action],
        &root.join("media"),
        &ApplyOptions {
            downloads: &root.join("downloads"),
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &cancel,
        },
        &mut warnings,
        &mut report_entries,
    );

    assert!(outcome.cancelled);
    assert_eq!(outcome.counts.applied, 0);
    assert_eq!(report_entries[0].status, EntryStatus::Planned);
    assert_eq!(fs::metadata(&src).expect("meta").ino(), src_inode);
}

#[test]
fn full_digest_stops_when_cancelled() {
    let root = TempDir::new("cancelled_digest");
    let path = root.join("big.bin");
    write_file(&path, &[1u8; 4096]);
    let cancel = CancelToken::default();
    cancel.cancel();

    let err =
        super::hashing::full_digest(&path, 4096, &cancel, &|_| {}).expect_err("cancelled digest");

    assert!(err.is::<Cancelled>());
}

#[test]
fn link_run_leaves_empty_files_alone() {
    let root = TempDir::new("empty_files");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let keep = downloads.join("Show.S01").join(".keep");
    let plexignore = media.join(".plexignore");
    write_file(&keep, b"");
    write_file(&plexignore, b"");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, None);

    let keep_meta = fs::symlink_metadata(&keep).expect("keep metadata");
    let plexignore_meta = fs::metadata(&plexignore).expect("plexignore metadata");
    assert!(
        keep_meta.file_type().is_file(),
        "empty download stays a file"
    );
    assert_ne!(
        keep_meta.ino(),
        plexignore_meta.ino(),
        "an empty download must not share the library file's inode"
    );
}

#[test]
fn cross_seeded_downloads_sharing_an_inode_are_all_linked() {
    // Two torrents seed one file through two hardlinked paths; the library
    // holds its own copy. Replacing the first path changes the shared inode's
    // link count and ctime, which must not fail the second.
    let root = TempDir::new("cross_seed");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let first = downloads.join("t1").join("movie.mkv");
    let second = downloads.join("t2").join("movie.mkv");
    let library = media.join("movie.mkv");
    write_file(&first, b"cross-seeded-bytes");
    write_file(&library, b"cross-seeded-bytes");
    fs::create_dir_all(second.parent().expect("t2")).expect("create t2");
    fs::hard_link(&first, &second).expect("cross-seed hardlink");
    let report = root.join("report.json");
    let config = make_config(&downloads, &media, 0);

    run_scan_with_options(
        &config,
        ScanRunOptions {
            strict: true,
            report_path: Some(&report),
            ..ScanRunOptions::for_test(false)
        },
    )
    .expect("a strict run links both paths");

    let library_ino = fs::metadata(&library).expect("library").ino();
    assert_eq!(fs::metadata(&first).expect("first").ino(), library_ino);
    assert_eq!(fs::metadata(&second).expect("second").ino(), library_ino);
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("read report"))
            .expect("parse report");
    assert_eq!(report["applied_actions"], 2);
    assert_eq!(report["warnings"]["link_errors"], 0);
}

#[test]
fn link_that_would_lock_out_the_download_owner_is_refused() {
    if !running_as_root() {
        eprintln!("skipped: changing file owners needs root");
        return;
    }
    // The torrent client owns its download; the library copy belongs to the
    // *arr user and is closed to everyone else. A hardlink would hand the
    // download the library's owner and mode, and seeding would stop.
    let root = TempDir::new("owner_lockout");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let download = downloads.join("movie.mkv");
    let library = media.join("movie.mkv");
    write_file(&download, b"owned-bytes");
    write_file(&library, b"owned-bytes");
    std::os::unix::fs::chown(&download, Some(1001), Some(1001)).expect("chown download");
    fs::set_permissions(&download, fs::Permissions::from_mode(0o664)).expect("chmod download");
    std::os::unix::fs::chown(&library, Some(1002), Some(1002)).expect("chown library");
    fs::set_permissions(&library, fs::Permissions::from_mode(0o640)).expect("chmod library");
    let download_ino = fs::metadata(&download).expect("download").ino();
    let report = root.join("report.json");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, Some(&report));

    assert_eq!(
        fs::metadata(&download).expect("download").ino(),
        download_ino,
        "the download is left as it was"
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("read report"))
            .expect("parse report");
    assert_eq!(report["entries"][0]["status"], "skipped");
    let error = report["entries"][0]["error"].as_str().expect("error");
    assert!(error.contains("could no longer read"), "{error}");
    assert!(
        error.contains("uid 1001"),
        "names who loses access: {error}"
    );
}

#[test]
fn read_access_lost_compares_who_reads_the_download_before_and_after() {
    use super::linking::{Access, read_access_lost};
    let access = |uid, gid, mode| Access { uid, gid, mode };

    // Different owners, library closed to others: the owner loses read.
    let who = read_access_lost(access(1001, 1001, 0o664), access(1002, 1002, 0o640));
    assert_eq!(who.as_deref(), Some("its owner (uid 1001)"));
    // A shared group that can read: nobody loses anything.
    assert!(read_access_lost(access(1001, 100, 0o664), access(1002, 100, 0o640)).is_none());
    // Same owner: compare the owner's own bits, then the group's.
    assert!(read_access_lost(access(1000, 1000, 0o644), access(1000, 1000, 0o640)).is_none());
    let who = read_access_lost(access(1000, 1000, 0o644), access(1000, 1000, 0o200));
    assert_eq!(who.as_deref(), Some("its owner (uid 1000)"));
    let who = read_access_lost(access(1000, 1000, 0o644), access(1000, 1000, 0o600));
    assert_eq!(who.as_deref(), Some("its group (gid 1000)"));
    // A world-readable library file is readable by everyone.
    assert!(read_access_lost(access(1001, 1001, 0o640), access(1002, 1002, 0o644)).is_none());
    // The download's group read it; the library file shuts it out.
    let who = read_access_lost(access(0, 1001, 0o640), access(1002, 1002, 0o640));
    assert_eq!(who.as_deref(), Some("its group (gid 1001)"));
    // Losing write alone is allowed: seeding only reads.
    assert!(read_access_lost(access(1001, 100, 0o664), access(1002, 100, 0o644)).is_none());
}

#[test]
fn a_download_found_to_be_the_library_file_at_link_time_is_already_linked() {
    // mergerfs without use_ino reports two inode numbers for one file, so the
    // plan sees a copy. rename(2) of one name over another of the same file
    // does nothing, and the temp name survives: nothing was linked.
    let root = TempDir::new("already_linked");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&tgt, b"one-file");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::hard_link(&tgt, &src).expect("existing hardlink");
    let action = super::types::LinkAction {
        src: src.clone(),
        tgt: tgt.clone(),
        size: 8,
        src_snapshot: snapshot_of(&src),
        tgt_snapshot: snapshot_of(&tgt),
    };
    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![planned_entry_for(&action)];

    let outcome = apply_actions(
        &[action],
        &media,
        &ApplyOptions {
            downloads: &downloads,
            strict: true,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    assert!(outcome.strict_error.is_none());
    assert_eq!(outcome.counts.applied, 0, "nothing was linked");
    assert_eq!(outcome.counts.already_linked, 1);
    assert_eq!(report_entries[0].status, EntryStatus::AlreadyLinked);
    assert_eq!(warnings.link_errors, 0);
    let leftovers = fs::read_dir(&downloads)
        .expect("read downloads")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
        .count();
    assert_eq!(leftovers, 0, "the surviving temp name is removed");
}

#[test]
fn a_symlink_the_filesystem_made_for_a_hardlink_is_reported_as_a_symlink() {
    // mergerfs's link-exdev option answers link(2) across disks with a symlink
    // to the library file and reports success.
    let root = TempDir::new("link_exdev");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-bytes");
    write_file(&tgt, b"same-bytes");

    let linked = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        |from, to| std::os::unix::fs::symlink(fs::canonicalize(from)?, to),
        |from, to| fs::rename(from, to),
    )
    .expect("link");

    assert_eq!(
        linked.method,
        LinkMethod::Symlink,
        "the report says what is on disk"
    );
    assert!(
        linked
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("link-exdev")),
        "{:?}",
        linked.fallback_reason
    );
    assert!(
        fs::symlink_metadata(&src)
            .expect("src")
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_library_file_replaced_while_linking_leaves_the_download_alone() {
    // Sonarr renames an upgrade over the library file between the pre-link
    // checks and the hardlink. The download must not end up a link to the new
    // file, hard or symbolic: its own bytes would be gone.
    let root = TempDir::new("replaced_target");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let upgrade = root.join("upgrade.mkv");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-bytes");
    write_file(&tgt, b"same-bytes");
    write_file(&upgrade, b"a different, longer upgrade");
    let (from_upgrade, library) = (upgrade.clone(), tgt.clone());

    let result = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        move |from, to| {
            fs::rename(&from_upgrade, &library)?;
            fs::hard_link(from, to)
        },
        |from, to| fs::rename(from, to),
    );

    let err = result.expect_err("the pair fails; no fallback");
    assert!(
        err.to_string()
            .contains("changed while it was being linked"),
        "{err}"
    );
    let meta = fs::symlink_metadata(&src).expect("src");
    assert!(meta.file_type().is_file(), "the download is still a file");
    assert_eq!(fs::read(&src).expect("read src"), b"same-bytes");
}

#[test]
fn a_library_path_swapped_for_a_symlink_while_linking_is_refused() {
    // link(2) does not follow a symlink: a library path swapped for one after
    // the checks leaves a symlink to wherever it points at the temp name. No
    // link of any kind may then replace the download.
    let root = TempDir::new("swapped_target");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let elsewhere = root.join("elsewhere").join("secret");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-bytes");
    write_file(&tgt, b"same-bytes");
    write_file(&elsewhere, b"not for the download");
    let (victim, library) = (elsewhere.clone(), tgt.clone());

    let result = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        move |from, to| {
            fs::remove_file(&library)?;
            std::os::unix::fs::symlink(&victim, &library)?;
            fs::hard_link(from, to)
        },
        |from, to| fs::rename(from, to),
    );

    assert!(result.is_err(), "{result:?}");
    let meta = fs::symlink_metadata(&src).expect("src");
    assert!(meta.file_type().is_file(), "the download is still a file");
    assert_eq!(fs::read(&src).expect("read src"), b"same-bytes");
}

#[test]
fn a_download_replaced_while_linking_is_not_overwritten() {
    // The torrent client re-creates the download after the checks: the rename
    // must not replace a file the plan never saw.
    let root = TempDir::new("replaced_source");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let fresh = root.join("fresh.mkv");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-bytes");
    write_file(&tgt, b"same-bytes");
    write_file(&fresh, b"the client's new file");
    let (from_fresh, download) = (fresh.clone(), src.clone());

    let result = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        move |from, to| {
            fs::rename(&from_fresh, &download)?;
            fs::hard_link(from, to)
        },
        |from, to| fs::rename(from, to),
    );

    assert!(result.is_err(), "{result:?}");
    assert_eq!(fs::read(&src).expect("read src"), b"the client's new file");
    let leftovers = fs::read_dir(&downloads)
        .expect("read downloads")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
        .count();
    assert_eq!(leftovers, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_folder_swapped_for_a_symlink_while_linking_cannot_redirect_the_rename() {
    let root = TempDir::new("swapped_folder");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let folder = downloads.join("Movie.2016");
    let moved = downloads.join("Movie.2016.real");
    let elsewhere = root.join("elsewhere");
    let src = folder.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-bytes");
    write_file(&tgt, b"same-bytes");
    write_file(&elsewhere.join("movie.mkv"), b"someone else's file");
    let (from_folder, to_folder, decoy) = (folder.clone(), moved.clone(), elsewhere.clone());

    let linked = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        move |from, to| {
            // After the checks: the folder moves away and a symlink to
            // somewhere else takes its name.
            fs::rename(&from_folder, &to_folder)?;
            std::os::unix::fs::symlink(&decoy, &from_folder)?;
            fs::hard_link(from, to)
        },
        |from, to| fs::rename(from, to),
    )
    .expect("link");

    assert_eq!(linked.method, LinkMethod::Hardlink);
    assert_eq!(
        fs::read(elsewhere.join("movie.mkv")).expect("decoy"),
        b"someone else's file",
        "the file the symlink points at is untouched"
    );
    assert_eq!(
        fs::metadata(moved.join("movie.mkv")).expect("moved").ino(),
        fs::metadata(&tgt).expect("tgt").ino(),
        "the download in the folder that was opened is the one linked"
    );
}

#[test]
fn a_download_whose_folder_resolves_outside_downloads_is_refused() {
    let root = TempDir::new("folder_outside");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let elsewhere = root.join("elsewhere");
    let tgt = media.join("movie.mkv");
    write_file(&tgt, b"same-bytes");
    write_file(&elsewhere.join("movie.mkv"), b"same-bytes");
    fs::create_dir_all(&downloads).expect("create downloads");
    std::os::unix::fs::symlink(&elsewhere, downloads.join("escape")).expect("folder symlink");
    let src = downloads.join("escape").join("movie.mkv");
    let before = fs::metadata(elsewhere.join("movie.mkv"))
        .expect("meta")
        .ino();

    let err = link_with(
        &src,
        &tgt,
        &downloads,
        &media,
        LinkMode::Best,
        |from, to| fs::hard_link(from, to),
        |from, to| fs::rename(from, to),
    )
    .expect_err("a folder outside downloads is refused");

    assert!(
        err.to_string().contains("not inside the downloads folder"),
        "{err}"
    );
    assert_eq!(
        fs::metadata(elsewhere.join("movie.mkv"))
            .expect("meta")
            .ino(),
        before
    );
}

#[test]
fn a_pair_left_alone_for_access_does_not_stop_a_strict_run() {
    if !running_as_root() {
        eprintln!("skipped: changing file owners needs root");
        return;
    }
    let root = TempDir::new("access_skip_strict");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // Sorts first: its library copy is closed to the download's owner.
    let locked = downloads.join("a-locked.mkv");
    let locked_lib = media.join("a-locked.mkv");
    let clean = downloads.join("b-clean.mkv");
    let clean_lib = media.join("b-clean.mkv");
    write_file(&locked, b"locked-bytes");
    write_file(&locked_lib, b"locked-bytes");
    write_file(&clean, b"clean-bytes");
    write_file(&clean_lib, b"clean-bytes");
    std::os::unix::fs::chown(&locked, Some(1001), Some(1001)).expect("chown download");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o664)).expect("chmod download");
    std::os::unix::fs::chown(&locked_lib, Some(1002), Some(1002)).expect("chown library");
    fs::set_permissions(&locked_lib, fs::Permissions::from_mode(0o640)).expect("chmod library");
    let report = root.join("report.json");
    let config = make_config(&downloads, &media, 0);

    run_scan_with_options(
        &config,
        ScanRunOptions {
            strict: true,
            report_path: Some(&report),
            ..ScanRunOptions::for_test(false)
        },
    )
    .expect("a deliberate skip is not a failure");

    assert_eq!(
        fs::metadata(&clean).expect("clean").ino(),
        fs::metadata(&clean_lib).expect("clean lib").ino(),
        "the pair after the skipped one is linked"
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("read report"))
            .expect("parse report");
    let locked_entry = report["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| {
            entry["source"]
                .as_str()
                .is_some_and(|s| s.ends_with("a-locked.mkv"))
        })
        .expect("locked entry");
    assert_eq!(locked_entry["status"], "skipped");
    assert_eq!(report["warnings"]["link_errors"], 0);
}

#[test]
fn a_warm_scan_reads_no_file_it_already_knows() {
    let root = TempDir::new("warm_reads");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // Same size, different content: the quick hashes differ, so neither file
    // is read in full and neither has a full hash to cache.
    let big = (super::hashing::SAMPLE_BYTES + 4096) as usize;
    write_file(&downloads.join("new.mkv"), &vec![1u8; big]);
    write_file(&media.join("other.mkv"), &vec![2u8; big]);
    // Small files whose quick hash is their full hash.
    write_file(&downloads.join("a.nfo"), b"aaaa");
    write_file(&media.join("b.nfo"), b"bbbb");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, true, false, None);
    assert_eq!(super::hashing::read_log::reads_under(&root), 4);
    run_scan(&config, true, false, None);
    assert_eq!(super::hashing::read_log::reads_under(&root), 4);
}

#[test]
fn excluded_folders_are_never_walked() {
    let root = TempDir::new("exclude");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // A recycle-bin copy sorts first by bytes, so without the exclude list it
    // would become the link target.
    write_file(&media.join("#recycle/movie.mkv"), b"same-content");
    write_file(&media.join("Movies/movie.mkv"), b"same-content");
    write_file(&downloads.join("movie.mkv"), b"same-content");
    write_file(&downloads.join("@eaDir/thumb.mkv"), b"same-content");
    let config = make_config(&downloads, &media, 0);
    let exclude = Exclude::new(config.effective_exclude());
    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();

    let files = collect_run_files(
        &downloads,
        &media,
        Duration::ZERO,
        CollectOptions {
            min_size: 1,
            exclude: &exclude,
            strict: true,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &test_pool(),
        &mut warnings,
        &mut metrics,
    )
    .expect("collect");
    let mut paths: Vec<_> = files.iter().map(|file| file.path.clone()).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![downloads.join("movie.mkv"), media.join("Movies/movie.mkv")]
    );
    // Each excluded folder counts once, whatever it holds.
    assert_eq!(metrics.skipped_by_exclude, 2);
}

#[test]
fn the_parallel_walk_finds_every_file_in_a_wide_tree() {
    let root = TempDir::new("wide_walk");
    let downloads = root.join("downloads");
    let media = root.join("media");
    for show in 0..12 {
        for season in 0..3 {
            for episode in 0..4 {
                let rel = format!("Show {show}/Season {season}/e{episode}.mkv");
                write_file(&downloads.join(&rel), b"download");
                write_file(&media.join(&rel), b"library!");
            }
            write_file(
                &media.join(format!("Show {show}/Season {season}/tiny.nfo")),
                b"x",
            );
        }
    }
    let config = make_config(&downloads, &media, 0);
    let exclude = Exclude::new(config.effective_exclude());
    let mut warnings = WarningSummary::default();
    let mut metrics = RunMetrics::default();

    let files = collect_run_files(
        &downloads,
        &media,
        Duration::ZERO,
        CollectOptions {
            min_size: 2,
            exclude: &exclude,
            strict: true,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &test_pool(),
        &mut warnings,
        &mut metrics,
    )
    .expect("collect");

    assert_eq!(files.iter().filter(|file| file.in_downloads).count(), 144);
    assert_eq!(files.iter().filter(|file| !file.in_downloads).count(), 144);
    assert_eq!(metrics.skipped_by_min_size, 36);
    let downloads_sorted = files
        .iter()
        .filter(|file| file.in_downloads)
        .map(|file| &file.path)
        .is_sorted();
    assert!(
        downloads_sorted,
        "each root's files come back sorted by path"
    );
}

#[test]
fn a_cancelled_walk_stops_with_cancelled() {
    let root = TempDir::new("cancelled_walk");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("a/b.mkv"), b"x");
    write_file(&media.join("a/b.mkv"), b"x");
    let cancel = CancelToken::default();
    cancel.cancel();
    let exclude = Exclude::default();

    let result = collect_run_files(
        &downloads,
        &media,
        Duration::ZERO,
        CollectOptions {
            min_size: 1,
            exclude: &exclude,
            strict: false,
            show_progress: false,
            tui_events: false,
            cancel: &cancel,
        },
        &test_pool(),
        &mut WarningSummary::default(),
        &mut RunMetrics::default(),
    );
    assert!(result.expect_err("cancelled").is::<Cancelled>());
}

#[test]
fn a_cache_with_the_old_absolute_keys_is_renamed_without_reading_anything() {
    let root = TempDir::new("cache_migrate");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("Movie/movie.mkv"), b"same-content");
    write_file(&media.join("Movies/movie.mkv"), b"same-content");
    let config = make_config(&downloads, &media, 0);
    run_scan(&config, true, false, None);
    let reads = super::hashing::read_log::reads_under(&root);

    // Write the cache as a version keyed by absolute path would have.
    let cache_file = downloads.join(".dejaview_cache.json");
    let old: Cache = load_cache(&cache_file)
        .into_iter()
        .map(|(key, entry)| {
            let path = match key.split_once('/') {
                Some(("downloads", rel)) => downloads.join(rel),
                Some(("media", rel)) => media.join(rel),
                _ => panic!("unexpected key {key}"),
            };
            (path.to_string_lossy().into_owned(), entry)
        })
        .collect();
    save_cache(&cache_file, &old).expect("write old cache");

    run_scan(&config, true, false, None);
    assert_eq!(super::hashing::read_log::reads_under(&root), reads);
    let mut keys: Vec<String> = load_cache(&cache_file).into_keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["downloads/Movie/movie.mkv", "media/Movies/movie.mkv"]
    );
}

#[test]
fn an_unchanged_cache_is_not_written_again() {
    let root = TempDir::new("cache_unchanged");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("movie.mkv"), b"same-content");
    write_file(&media.join("movie.mkv"), b"same-content");
    let config = make_config(&downloads, &media, 0);
    let cache_file = downloads.join(".dejaview_cache.json");

    run_scan(&config, true, false, None);
    let written = fs::metadata(&cache_file).expect("cache written").ino();
    run_scan(&config, true, false, None);
    // A save writes a new file and renames it into place.
    assert_eq!(fs::metadata(&cache_file).expect("cache").ino(), written);
}

#[test]
fn cache_keys_name_the_root_so_two_mounts_of_one_share_agree() {
    let host = super::cache::CacheKeys::new(
        Path::new("/mnt/pool/torrents"),
        Path::new("/mnt/pool/media"),
    );
    let container =
        super::cache::CacheKeys::new(Path::new("/data/torrents"), Path::new("/data/media"));
    assert_eq!(
        host.key(Path::new("/mnt/pool/media/Movies/a.mkv"), false),
        container.key(Path::new("/data/media/Movies/a.mkv"), false)
    );
    assert_eq!(
        host.key(Path::new("/mnt/pool/torrents/a.mkv"), true),
        "downloads/a.mkv"
    );
    // A cache kept by the old version: absolute keys, one outside both roots.
    let mut cache = Cache::new();
    let entry = CacheEntry {
        snapshot: FileSnapshot {
            size: 1,
            mtime_ns: 0,
            ctime_ns: 0,
            dev: 1,
            inode: 1,
        },
        hash: Some("h".into()),
        sample: None,
    };
    cache.insert("/data/media/Movies/a.mkv".into(), entry.clone());
    cache.insert("/elsewhere/b.mkv".into(), entry.clone());
    cache.insert("media/Movies/c.mkv".into(), entry);
    assert_eq!(container.migrate(&mut cache), 1);
    let mut keys: Vec<&String> = cache.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "/elsewhere/b.mkv",
            "media/Movies/a.mkv",
            "media/Movies/c.mkv"
        ]
    );
}

#[test]
fn hashing_saves_the_cache_as_it_goes() {
    use super::hashing::{Checkpoint, HashOptions, hash_groups_and_update_cache};
    let root = TempDir::new("checkpoint");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("movie.mkv"), b"same-content");
    write_file(&media.join("movie.mkv"), b"same-content");
    let exclude = Exclude::default();
    let mut warnings = WarningSummary::default();
    let files = collect_run_files(
        &downloads,
        &media,
        Duration::ZERO,
        CollectOptions {
            min_size: 1,
            exclude: &exclude,
            strict: true,
            show_progress: false,
            tui_events: false,
            cancel: &CancelToken::default(),
        },
        &test_pool(),
        &mut warnings,
        &mut RunMetrics::default(),
    )
    .expect("collect");
    let mut groups = group_files_by_size(files);
    let cache_file = root.join("checkpoint.json");
    let selection = ScanRunOptions {
        strict: true,
        ..ScanRunOptions::for_test(true)
    };
    let keys = super::cache::CacheKeys::new(&downloads, &media);
    let mut cache = Cache::new();

    let hashed = hash_groups_and_update_cache(
        &mut groups,
        &mut cache,
        &HashOptions {
            strict: true,
            show_progress: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            selection: &selection,
            cancel: &selection.cancel,
            full_pool: &test_pool(),
            keys: &keys,
            checkpoint: Checkpoint {
                path: &cache_file,
                every: Duration::ZERO,
            },
        },
        &mut warnings,
    );
    hashed.result.expect("hash");

    // The caller saves at the end; this file is the checkpoint's.
    let saved = load_cache(&cache_file);
    assert_eq!(saved.len(), 2, "{saved:?}");
    assert_eq!(hashed.cache_writes, 2);
}

#[test]
fn the_cache_vouches_only_for_rows_the_walk_would_plan_the_same() {
    let root = TempDir::new("vouch");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("a.mkv"), b"same-content");
    write_file(&media.join("a.mkv"), b"same-content");
    write_file(&downloads.join("#recycle/b.mkv"), b"other-content");
    write_file(&media.join("b.mkv"), b"other-content");
    let mut config = make_config(&downloads, &media, 0);
    // Scan with nothing excluded, so every file has a cached hash.
    config.exclude = Some(Vec::new());
    run_scan(&config, true, false, None);
    let cache = load_cache(&downloads.join(".dejaview_cache.json"));
    let keys = super::cache::CacheKeys::new(&downloads, &media);
    let roots = (
        RootId::of("downloads", &downloads).expect("downloads"),
        RootId::of("media", &media).expect("media"),
    );
    let exclude = Exclude::new(Config::default().effective_exclude());
    let filters = super::reviewed::Filters {
        min_size: 1,
        deadline: SystemTime::now(),
        exclude: &exclude,
    };
    let vouch = |src: &Path, tgt: &Path| {
        super::reviewed::plan_from_cache(
            &[(src.to_path_buf(), tgt.to_path_buf())],
            &roots.0,
            &roots.1,
            &cache,
            &keys,
            &filters,
        )
        .expect("no refusal")
    };

    let actions = vouch(&downloads.join("a.mkv"), &media.join("a.mkv")).expect("vouched");
    assert_eq!(actions.len(), 1);
    let why = vouch(&downloads.join("#recycle/b.mkv"), &media.join("b.mkv")).expect_err("excluded");
    assert!(why.contains("exclude list"), "{why}");
    let why = vouch(&downloads.join("a.mkv"), &media.join("b.mkv")).expect_err("different files");
    assert!(why.contains("no longer hash alike"), "{why}");

    // The download becomes a link to a library file: only a walk can tell
    // which, so the cache does not vouch for it.
    fs::remove_file(downloads.join("a.mkv")).expect("remove download");
    fs::hard_link(media.join("a.mkv"), downloads.join("a.mkv")).expect("hardlink");
    let why = vouch(&downloads.join("a.mkv"), &media.join("a.mkv")).expect_err("hardlinked");
    assert!(why.contains("other hardlinks"), "{why}");
}

#[test]
fn the_cache_does_not_vouch_for_a_download_that_is_a_library_file_through_a_mount() {
    let root = TempDir::new("vouch_mount");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("a.mkv"), b"same-content");
    write_file(&media.join("A/a.mkv"), b"same-content");
    let config = make_config(&downloads, &media, 0);
    run_scan(&config, true, false, None);
    let mut cache = load_cache(&downloads.join(".dejaview_cache.json"));
    let keys = super::cache::CacheKeys::new(&downloads, &media);
    let roots = (
        RootId::of("downloads", &downloads).expect("downloads"),
        RootId::of("media", &media).expect("media"),
    );
    let exclude = Exclude::default();
    let filters = super::reviewed::Filters {
        min_size: 1,
        deadline: SystemTime::now(),
        exclude: &exclude,
    };
    let pair = [(downloads.join("a.mkv"), media.join("A/a.mkv"))];
    let vouch = |cache: &Cache| {
        super::reviewed::plan_from_cache(&pair, &roots.0, &roots.1, cache, &keys, &filters)
            .expect("no refusal")
    };
    assert!(vouch(&cache).is_ok(), "a plain copy is vouched for");
    let download = cache["downloads/a.mkv"].clone();

    // A library folder bind-mounted inside downloads: one device, one inode.
    let mut bind = cache.clone();
    bind.insert(
        "media/B/a.mkv".into(),
        CacheEntry {
            hash: download.hash.clone(),
            ..download.clone()
        },
    );
    let why = vouch(&bind).expect_err("bind mount");
    assert!(why.contains("seen through a second mount"), "{why}");

    // A mergerfs branch and the pool: another device and inode, the same
    // size and timestamps.
    cache.insert(
        "media/B/a.mkv".into(),
        CacheEntry {
            snapshot: FileSnapshot {
                dev: download.snapshot.dev + 1,
                inode: download.snapshot.inode + 1,
                ..download.snapshot
            },
            ..download
        },
    );
    let why = vouch(&cache).expect_err("second device");
    assert!(why.contains("seen through a second mount"), "{why}");
}

/// A link run over `downloads` and `media` that writes its report to
/// `report`, which it returns parsed.
fn link_and_read_report(downloads: &Path, media: &Path, report: &Path) -> serde_json::Value {
    run_scan(
        &make_config(downloads, media, 0),
        false,
        false,
        Some(report),
    );
    serde_json::from_str(&fs::read_to_string(report).expect("read report")).expect("parse report")
}

#[test]
fn a_failed_pair_names_the_os_error_and_drops_the_planned_method() {
    let root = TempDir::new("failed_pair_chain");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    write_file(&src, b"same-content");
    write_file(&tgt, b"same-content");
    let action = super::types::LinkAction {
        src: src.clone(),
        tgt: tgt.clone(),
        size: 12,
        src_snapshot: snapshot_of(&src),
        tgt_snapshot: snapshot_of(&tgt),
    };
    let mut entry = planned_entry_for(&action);
    entry.method = Some(LinkMethod::TryHardlink);
    entry.fallback_reason = Some("tries hardlink first; may fall back to symlink".to_string());
    // Gone between the plan and the link, as when a torrent is removed.
    fs::remove_file(&src).expect("remove download");

    let mut warnings = WarningSummary::default();
    let mut report_entries = vec![entry];
    apply_actions(
        &[action],
        &media,
        &ApplyOptions {
            downloads: &downloads,
            strict: false,
            tui_events: false,
            link_mode: LinkMode::Best,
            cancel: &CancelToken::default(),
        },
        &mut warnings,
        &mut report_entries,
    );

    let entry = &report_entries[0];
    assert_eq!(entry.status, EntryStatus::Failed);
    let error = entry.error.as_deref().expect("error");
    assert!(error.contains("cannot look up"), "{error}");
    assert!(
        error.contains("No such file or directory"),
        "the OS error is kept: {error}"
    );
    assert_eq!(entry.method, None, "nothing was linked");
    assert_eq!(entry.fallback_reason, None);
}

#[test]
fn freed_bytes_count_a_cross_seeded_download_once() {
    let root = TempDir::new("freed_bytes");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // Two torrents seed one 18-byte file; a third download is its own file.
    let first = downloads.join("t1").join("movie.mkv");
    let second = downloads.join("t2").join("movie.mkv");
    write_file(&first, b"cross-seeded-bytes");
    fs::create_dir_all(second.parent().expect("t2")).expect("create t2");
    fs::hard_link(&first, &second).expect("cross-seed hardlink");
    write_file(&media.join("movie.mkv"), b"cross-seeded-bytes");
    write_file(&downloads.join("show.mkv"), b"an episode");
    write_file(&media.join("show.mkv"), b"an episode");

    let report = link_and_read_report(&downloads, &media, &root.join("report.json"));

    assert_eq!(report["applied_actions"], 3);
    assert_eq!(report["planned_bytes"], 18 + 10);
    assert_eq!(report["freed_bytes"], 18 + 10, "{report}");
    assert_eq!(report["total_saved_bytes"], 18 + 10);
}

#[test]
fn a_scan_counts_a_cross_seeded_download_once() {
    let root = TempDir::new("planned_bytes");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let first = downloads.join("t1").join("movie.mkv");
    let second = downloads.join("t2").join("movie.mkv");
    write_file(&first, b"cross-seeded-bytes");
    fs::create_dir_all(second.parent().expect("t2")).expect("create t2");
    fs::hard_link(&first, &second).expect("cross-seed hardlink");
    write_file(&media.join("movie.mkv"), b"cross-seeded-bytes");
    let report = root.join("report.json");

    run_scan(
        &make_config(&downloads, &media, 0),
        true,
        false,
        Some(&report),
    );

    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("read report"))
            .expect("parse report");
    assert_eq!(report["planned_actions"], 2);
    assert_eq!(report["planned_bytes"], 18);
    assert_eq!(report["total_saved_bytes"], 18);
    assert_eq!(report["freed_bytes"], 0);
}

#[test]
fn a_link_run_that_planned_nothing_leaves_the_last_report_alone() {
    let root = TempDir::new("report_left_alone");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("unique.mkv"), b"only in downloads");
    write_file(&media.join("other.mkv"), b"only in the library");
    let last_report = downloads.join(".dejaview_link_report.json");
    fs::write(&last_report, b"the last run that linked something").expect("write report");
    let config = make_config(&downloads, &media, 0);

    run_scan(&config, false, false, None);
    assert_eq!(
        fs::read(&last_report).expect("read report"),
        b"the last run that linked something"
    );

    // A path given with --report is always written.
    let asked = root.join("asked.json");
    run_scan(&config, false, false, Some(&asked));
    assert!(asked.exists());
}

#[test]
fn a_report_says_when_and_how_its_run_ended() {
    let root = TempDir::new("report_fields");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_file(&downloads.join("movie.mkv"), b"same-content");
    write_file(&media.join("movie.mkv"), b"same-content");

    let report = link_and_read_report(&downloads, &media, &root.join("report.json"));

    assert_eq!(report["outcome"], "complete");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    for field in ["started_at", "finished_at"] {
        let stamp = report[field].as_str().expect(field);
        assert!(
            stamp.len() == 20 && stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T',
            "{field}: {stamp}"
        );
    }
    assert!(report["started_at"].as_str() <= report["finished_at"].as_str());
    assert_eq!(report["downloads"], downloads.display().to_string());
    assert_eq!(report["media"], media.display().to_string());
    assert_eq!(report["link_mode"], "best");
    assert_eq!(report["symlinked"], 0);
    assert_eq!(report["already_linked"], 0);
}

#[test]
fn report_times_are_rfc_3339_in_utc() {
    let at = |secs| crate::human::rfc3339_utc(UNIX_EPOCH + Duration::from_secs(secs));
    assert_eq!(at(0), "1970-01-01T00:00:00Z");
    assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(at(1_762_399_020), "2025-11-06T03:17:00Z");
    assert_eq!(at(4_102_444_799), "2099-12-31T23:59:59Z");
}

#[test]
fn a_plan_comes_out_in_download_path_order() {
    let mut groups = Vec::new();
    // Many groups of one pair each: a hash map's order would show.
    for n in 0..40u64 {
        let mut library = sample_file_info(&format!("/media/{n:02}.mkv"), 1, 1000 + n, false);
        let mut download = sample_file_info(&format!("/downloads/{n:02}.mkv"), 2, 2000 + n, true);
        library.hash = Some(format!("h{n}"));
        download.hash = Some(format!("h{n}"));
        library.snapshot.size = 10 + n;
        download.snapshot.size = 10 + n;
        groups.push(vec![library, download]);
    }
    groups.reverse();

    let plan = super::linking::plan_actions(&groups);

    let sources: Vec<_> = plan
        .actions
        .iter()
        .map(|action| action.src.clone())
        .collect();
    let mut sorted = sources.clone();
    sorted.sort();
    assert_eq!(sources, sorted);
}

#[test]
fn the_plan_table_lines_up_with_colours_on() {
    colored::control::set_override(true);
    let action = super::types::LinkAction {
        src: PathBuf::from("/dl/Show.S01/e01.mkv"),
        tgt: PathBuf::from("/media/TV/Show/e01.mkv"),
        size: 3 << 20,
        src_snapshot: snapshot_of(Path::new("/")),
        tgt_snapshot: snapshot_of(Path::new("/")),
    };
    let table =
        super::linking::action_table(&[action], Path::new("/dl"), Path::new("/media")).to_string();
    colored::control::unset_override();

    let widths: Vec<usize> = table.lines().map(|line| line.chars().count()).collect();
    assert!(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        "every line as wide as the others:\n{table}"
    );
    assert!(table.contains("Show.S01/e01.mkv") && table.contains("TV/Show/e01.mkv"));
    assert!(
        !table.contains("/dl/"),
        "paths are below their folder:\n{table}"
    );
    assert!(table.contains("3.0 MiB"), "{table}");
}

#[test]
fn cache_keys_keep_bytes_that_are_not_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let keys = super::cache::CacheKeys::new(Path::new("/dl"), Path::new("/media"));
    let path = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));

    let ff = keys.key(&path(b"/dl/a\xff.mkv"), true);
    let fe = keys.key(&path(b"/dl/a\xfe.mkv"), true);
    assert_ne!(ff, fe, "names differing only in such bytes keep apart");
    assert_eq!(ff, "downloads/a\u{0}ff.mkv");
    // A UTF-8 path's key is what it always was.
    assert_eq!(
        keys.key(Path::new("/dl/Show/é.mkv"), true),
        "downloads/Show/é.mkv"
    );
}

#[test]
fn a_cache_entry_is_one_flat_object_on_disk() {
    let full = CacheEntry {
        snapshot: FileSnapshot {
            size: 42,
            mtime_ns: 1_700_000_000_123_456_789,
            ctime_ns: 1_700_000_000_987_654_321,
            dev: 2049,
            inode: 131_073,
        },
        hash: Some("ab".into()),
        sample: Some("cd".into()),
    };
    let json = serde_json::to_string(&full).expect("serialise");
    assert_eq!(
        json,
        r#"{"size":42,"mtime_ns":1700000000123456789,"ctime_ns":1700000000987654321,"dev":2049,"inode":131073,"hash":"ab","sample":"cd"}"#
    );
    assert_eq!(
        serde_json::from_str::<CacheEntry>(&json).expect("parse"),
        full
    );
    // A quick hash alone leaves the full hash out, as before.
    let quick = CacheEntry { hash: None, ..full };
    let json = serde_json::to_string(&quick).expect("serialise");
    assert_eq!(
        json,
        r#"{"size":42,"mtime_ns":1700000000123456789,"ctime_ns":1700000000987654321,"dev":2049,"inode":131073,"sample":"cd"}"#
    );
    assert_eq!(
        serde_json::from_str::<CacheEntry>(&json).expect("parse"),
        quick
    );
}
