use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dejaview_it_{prefix}_{}_{}",
        std::process::id(),
        ts
    ))
}

fn run_cmd(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dejaview"))
        .args(args)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .output()
        .expect("run command")
}

fn run_ok(root: &Path, args: &[&str]) {
    let out = run_cmd(root, args);
    assert!(
        out.status.success(),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn set_min_age_zero(root: &Path) {
    let config_path = root.join("xdg").join("dejaview").join("config.json");
    let raw = fs::read_to_string(&config_path).expect("read config");
    let mut config: serde_json::Value = serde_json::from_str(&raw).expect("parse config json");
    config["min_age"] = serde_json::json!(0);
    fs::write(
        &config_path,
        serde_json::to_string_pretty(&config).expect("encode config"),
    )
    .expect("write config");
}

fn set_link_mode(root: &Path, mode: &str) {
    run_ok(root, &["config", "set-link-mode", mode]);
}

#[test]
fn config_and_cache_commands_work_end_to_end() {
    let root = unique_temp_dir("config_cache");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let out = run_cmd(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_cmd(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_cmd(&root, &["config", "show"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(downloads.to_str().expect("downloads str")));
    assert!(stdout.contains(media.to_str().expect("media str")));

    let out = run_cmd(&root, &["cache", "info"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Cache location:"));
    assert!(stdout.contains("Cached entries:"));

    let out = run_cmd(&root, &["cache", "clear"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn scan_fails_with_actionable_message_when_paths_missing() {
    let root = unique_temp_dir("missing_paths");
    let missing_downloads = root.join("downloads_missing");
    let media = root.join("media");
    fs::create_dir_all(&media).expect("create media");
    fs::write(media.join("a.mkv"), b"x").expect("write media file");

    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    let config_path = root.join("xdg").join("dejaview").join("config.json");
    fs::create_dir_all(config_path.parent().expect("config parent")).expect("create config dir");
    fs::write(
        &config_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "downloads": missing_downloads,
            "media": media,
        }))
        .expect("serialize config"),
    )
    .expect("write config");

    let out = run_cmd(&root, &["scan"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("downloads path does not exist"));

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_command_prefers_hardlink_and_writes_report() {
    let root = unique_temp_dir("link_symlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    fs::write(&src, b"same-content").expect("write src");
    fs::write(&tgt, b"same-content").expect("write tgt");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);
    let report = root.join("link_report.json");
    run_ok(
        &root,
        &[
            "link",
            "--report",
            report.to_str().expect("report path str"),
        ],
    );

    let src_meta = fs::metadata(&src).expect("src metadata");
    let tgt_meta = fs::metadata(&tgt).expect("tgt metadata");
    assert_eq!(src_meta.ino(), tgt_meta.ino());
    assert_eq!(src_meta.dev(), tgt_meta.dev());
    assert!(report.exists());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_command_honors_symlink_only_mode() {
    let root = unique_temp_dir("link_symlink_only");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    fs::write(&src, b"same-content").expect("write src");
    fs::write(&tgt, b"same-content").expect("write tgt");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);
    set_link_mode(&root, "symlink-only");

    let out = run_cmd(&root, &["link", "--json", "--no-progress"]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout_json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse link stdout json");
    assert_eq!(stdout_json["entries"][0]["method"], "symlink");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn cache_audit_and_cleanup_temp_commands_work() {
    let root = unique_temp_dir("cache_temp_cleanup");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let nested = downloads.join("nested");
    fs::create_dir_all(&nested).expect("create nested downloads");
    fs::create_dir_all(&media).expect("create media");
    fs::write(downloads.join(".dejaview_hardlink.tmp.1"), b"temp-artifact")
        .expect("write temp artifact");
    fs::write(nested.join(".dejaview_link.tmp.2"), b"temp-artifact").expect("write nested temp");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );

    let out = run_cmd(&root, &["cache", "audit-temp"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Temp artifacts: 2"));

    let out = run_cmd(&root, &["cache", "cleanup-temp"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Removed temp artifacts: 2"));
    assert!(!downloads.join(".dejaview_hardlink.tmp.1").exists());
    assert!(!nested.join(".dejaview_link.tmp.2").exists());

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn scan_json_writes_machine_readable_stdout_and_report() {
    let root = unique_temp_dir("scan_json_report");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    fs::write(&src, b"same-content").expect("write src");
    fs::write(&tgt, b"same-content").expect("write tgt");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let report = root.join("scan_report.json");
    let out = run_cmd(
        &root,
        &[
            "scan",
            "--json",
            "--no-progress",
            "--report",
            report.to_str().expect("report path str"),
        ],
    );
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout_json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse scan stdout json");
    assert_eq!(stdout_json["mode"], "scan");
    assert_eq!(stdout_json["dry_run"], true);
    assert_eq!(stdout_json["planned_actions"], 1);
    assert_eq!(stdout_json["applied_actions"], 0);
    assert_eq!(stdout_json["skipped_by_min_age"], 0);
    assert!(stdout_json["entries"].is_array());

    let report_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("read report"))
            .expect("parse report json");
    assert_eq!(report_json["mode"], "scan");
    assert_eq!(report_json["planned_actions"], 1);

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_json_reports_applied_actions() {
    let root = unique_temp_dir("link_json");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    fs::write(&src, b"same-content").expect("write src");
    fs::write(&tgt, b"same-content").expect("write tgt");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let out = run_cmd(&root, &["link", "--json", "--no-progress"]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout_json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse link stdout json");
    assert_eq!(stdout_json["mode"], "link");
    assert_eq!(stdout_json["dry_run"], false);
    assert_eq!(stdout_json["planned_actions"], 1);
    assert_eq!(stdout_json["applied_actions"], 1);
    assert!(
        stdout_json["entries"][0]["method"] == "hardlink"
            || stdout_json["entries"][0]["method"] == "symlink"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_tui_events_stdout_stays_machine_parseable() {
    let root = unique_temp_dir("link_tui_events");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    let src = downloads.join("movie.mkv");
    let tgt = media.join("movie.mkv");
    fs::write(&src, b"same-content").expect("write src");
    fs::write(&tgt, b"same-content").expect("write tgt");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let out = run_cmd(&root, &["link", "--tui-events", "--no-progress"]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut saw_done = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every tui-events stdout line should be json");
        if value["type"] == "done" {
            saw_done = true;
        }
    }
    assert!(saw_done, "expected final done event in stdout:\n{stdout}");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn config_validate_reports_success_for_valid_paths() {
    let root = unique_temp_dir("config_validate");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    run_ok(&root, &["config", "set-min-age", "0"]);
    run_ok(&root, &["config", "set-threads", "2"]);

    let out = run_cmd(&root, &["config", "validate"]);
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Configuration is valid"));

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_confirm_conflicts_with_json_mode() {
    let root = unique_temp_dir("confirm_json_conflict");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");
    fs::write(downloads.join("a.txt"), b"x").expect("write download");
    fs::write(media.join("a.txt"), b"x").expect("write media");

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    run_ok(&root, &["config", "set-min-age", "0"]);

    let out = run_cmd(&root, &["link", "--json", "--confirm"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--confirm cannot be combined with --json")
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn nested_media_root_is_rejected_and_library_is_untouched() {
    let root = unique_temp_dir("nested_roots");
    let downloads = root.join("downloads");
    let media = downloads.join("media");
    fs::create_dir_all(&media).expect("create nested media");
    fs::write(media.join("a.bin"), vec![7u8; 4096]).expect("write a");
    fs::write(media.join("b.bin"), vec![7u8; 4096]).expect("write b");
    fs::write(downloads.join("dupe.bin"), vec![7u8; 4096]).expect("write dupe");
    let library_inode = fs::metadata(media.join("b.bin")).expect("meta").ino();

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let validate = run_cmd(&root, &["config", "validate"]);
    assert!(!validate.status.success());
    assert!(String::from_utf8_lossy(&validate.stderr).contains("media path is inside"));

    let link = run_cmd(&root, &["link", "--no-progress"]);
    assert!(!link.status.success());
    assert!(String::from_utf8_lossy(&link.stderr).contains("media path is inside"));
    assert_eq!(
        fs::metadata(media.join("b.bin")).expect("meta").ino(),
        library_inode,
        "library file must not be relinked"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

fn write_seeded(path: &Path, size: usize, seed: u8) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    let bytes: Vec<u8> = (0..size)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect();
    fs::write(path, bytes).expect("write seeded file");
}

fn paths_read_by_scan(root: &Path) -> Vec<String> {
    paths_read_by(root, &["scan"])
}

/// Paths a scan or link run opened for hashing, read from the TUI event stream.
fn paths_read_by(root: &Path, args: &[&str]) -> Vec<String> {
    let mut args = args.to_vec();
    args.extend(["--tui-events", "--no-progress"]);
    let out = run_cmd(root, &args);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let event: serde_json::Value = serde_json::from_str(line).expect("parse event");
        if event["type"] == "worker_activity"
            && let Some(path) = event["path"].as_str()
        {
            let phase = event["phase"].as_str().expect("phase");
            let rel = Path::new(path)
                .strip_prefix(root)
                .expect("path under root")
                .display();
            paths.push(format!("{phase} {rel}"));
        }
    }
    paths.sort();
    paths
}

#[test]
fn scans_skip_hardlinked_files_and_reuse_cache_for_every_path() {
    let root = unique_temp_dir("hash_reads");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // A real duplicate: separate copies in downloads and media.
    write_seeded(&media.join("movie.mkv"), 300_000, 1);
    write_seeded(&downloads.join("movie.mkv"), 300_000, 1);
    // Already hardlinked, as an *arr import leaves it: one physical file.
    write_seeded(&media.join("show.mkv"), 200_000, 2);
    fs::hard_link(media.join("show.mkv"), downloads.join("show.mkv")).expect("hardlink show");
    // A hardlinked pair plus a separate copy of the same content.
    write_seeded(&media.join("ep.mkv"), 100_000, 3);
    fs::hard_link(media.join("ep.mkv"), downloads.join("ep.mkv")).expect("hardlink ep");
    write_seeded(&downloads.join("ep-copy.mkv"), 100_000, 3);

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let cold = paths_read_by_scan(&root);
    assert!(
        !cold.iter().any(|p| p.contains("show.mkv")),
        "a group that is one physical file must not be read: {cold:?}"
    );
    let full_reads = cold
        .iter()
        .filter(|p| p.starts_with("hashing_full"))
        .count();
    assert_eq!(
        full_reads, 4,
        "one full read per physical file in a duplicate group: {cold:?}"
    );

    let warm = paths_read_by_scan(&root);
    assert!(
        warm.is_empty(),
        "an unchanged tree must be served from cache: {warm:?}"
    );

    run_ok(&root, &["link", "--no-progress"]);
    let after_link = paths_read_by_scan(&root);
    assert!(
        after_link.is_empty(),
        "linked pairs are one physical file and must not be reread: {after_link:?}"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn linking_one_source_hashes_only_its_size_group() {
    let root = unique_temp_dir("link_one_reads");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&media.join("a.mkv"), 300_000, 1);
    write_seeded(&downloads.join("a.mkv"), 300_000, 1);
    write_seeded(&media.join("b.mkv"), 200_000, 2);
    write_seeded(&downloads.join("b.mkv"), 200_000, 2);

    run_ok(
        &root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        &root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(&root);

    let source = downloads.join("a.mkv");
    let reads = paths_read_by(
        &root,
        &[
            "link",
            "--only-source",
            source.to_str().expect("source str"),
        ],
    );

    assert!(
        !reads.iter().any(|p| p.contains("b.mkv")),
        "an unselected size group must not be read: {reads:?}"
    );
    assert_eq!(
        fs::metadata(&source).expect("meta").ino(),
        fs::metadata(media.join("a.mkv")).expect("meta").ino(),
        "the selected source is linked"
    );
    assert_ne!(
        fs::metadata(downloads.join("b.mkv")).expect("meta").ino(),
        fs::metadata(media.join("b.mkv")).expect("meta").ino(),
        "an unselected source is left alone"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}
