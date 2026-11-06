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
    // Test files are small; only the empty-file rule applies.
    config["min_size"] = serde_json::json!(1);
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
    // A `config validate` test link that could not be removed.
    fs::write(
        downloads.join(".dejaview_validate_link.3.4"),
        b"temp-artifact",
    )
    .expect("write probe leftover");

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
    assert!(stdout.contains("Temp artifacts: 3"));

    let out = run_cmd(&root, &["cache", "cleanup-temp"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Removed temp artifacts: 3"));
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
    // Every file here is under the 1 MiB sample, so its quick hash reads it
    // whole and is its full hash: one read per physical file, none in full.
    assert_eq!(
        cold.len(),
        4,
        "one read per physical file in a duplicate group: {cold:?}"
    );
    assert!(
        cold.iter().all(|p| p.starts_with("hashing_sample")),
        "a file no bigger than the sample is never read twice: {cold:?}"
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

#[test]
fn broken_config_file_error_names_the_file_and_the_fix() {
    let root = unique_temp_dir("broken_config");
    let config_path = root.join("xdg").join("dejaview").join("config.json");
    fs::create_dir_all(config_path.parent().expect("config dir")).expect("create config dir");
    fs::write(&config_path, b"{\"downloads\": \"/srv\",, }").expect("write broken config");

    let out = run_cmd(&root, &["config", "set-media", "/tmp"]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&config_path.display().to_string()),
        "names the file: {stderr}"
    );
    assert!(
        stderr.contains("delete it"),
        "says how to recover: {stderr}"
    );
    assert_eq!(
        fs::read(&config_path).expect("read config"),
        b"{\"downloads\": \"/srv\",, }",
        "a broken config is left for the user to fix"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// Config with downloads and media set and min_age 0, leaving min_size at its
/// default.
fn configure_default_floor(root: &Path, downloads: &Path, media: &Path) {
    run_ok(
        root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    run_ok(root, &["config", "set-min-age", "0"]);
}

#[test]
fn files_under_one_mib_are_left_alone_by_default() {
    let root = unique_temp_dir("min_size_default");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // A subtitle-sized pair below the floor, and a video-sized pair above it.
    write_seeded(&downloads.join("Show.S01").join("e01.srt"), 9_000, 3);
    write_seeded(&media.join("Show").join("e01.srt"), 9_000, 3);
    write_seeded(&downloads.join("Show.S01").join("e01.mkv"), 1_200_000, 4);
    write_seeded(&media.join("Show").join("e01.mkv"), 1_200_000, 4);
    configure_default_floor(&root, &downloads, &media);

    let out = run_cmd(&root, &["link", "--json", "--no-progress"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse link json");
    assert_eq!(report["applied_actions"], 1, "only the video is linked");
    assert_eq!(
        report["skipped_by_min_size"], 2,
        "both subtitles are skipped"
    );

    let srt = fs::metadata(downloads.join("Show.S01").join("e01.srt")).expect("srt metadata");
    let lib_srt = fs::metadata(media.join("Show").join("e01.srt")).expect("library srt");
    assert_ne!(srt.ino(), lib_srt.ino(), "the subtitle stays its own file");
    let mkv = fs::metadata(downloads.join("Show.S01").join("e01.mkv")).expect("mkv metadata");
    let lib_mkv = fs::metadata(media.join("Show").join("e01.mkv")).expect("library mkv");
    assert_eq!(mkv.ino(), lib_mkv.ino(), "the video is hardlinked");

    // The floor can be lowered, but not to zero.
    let out = run_cmd(&root, &["config", "set-min-size", "0"]);
    assert!(!out.status.success(), "a floor of 0 is refused");
    run_ok(&root, &["config", "set-min-size", "1"]);
    let out = run_cmd(&root, &["link", "--json", "--no-progress"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse link json");
    assert_eq!(report["applied_actions"], 1, "the subtitle is linked now");
    assert_eq!(report["skipped_by_min_size"], 0);

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// A bind mount that the test removes again, even when an assertion fails.
struct BindMount(PathBuf);

impl BindMount {
    /// None where this process may not mount (not root, or no CAP_SYS_ADMIN),
    /// as on a CI runner: the caller skips the test.
    fn new(source: &Path, target: &Path) -> Option<Self> {
        let status = Command::new("mount")
            .arg("--bind")
            .arg(source)
            .arg(target)
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then(|| BindMount(target.to_path_buf()))
    }
}

impl Drop for BindMount {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg(&self.0).status();
    }
}

#[test]
fn library_bind_mounted_inside_downloads_is_refused() {
    let root = unique_temp_dir("bind_mount");
    // On the host, the library sits inside the data folder. A container sees
    // the data folder as downloads and the library as media.
    let data = root.join("data");
    let copy_a = data.join("media").join("A Movies").join("m.mkv");
    let copy_b = data.join("media").join("B Movies").join("m.mkv");
    write_seeded(&copy_a, 4096, 9);
    write_seeded(&copy_b, 4096, 9);
    let downloads = root.join("c").join("downloads");
    let media = root.join("c").join("media");
    fs::create_dir_all(&downloads).expect("create downloads mount point");
    fs::create_dir_all(&media).expect("create media mount point");
    let Some(_downloads_mount) = BindMount::new(&data, &downloads) else {
        eprintln!("skipped: bind mounts need root");
        let _ = fs::remove_dir_all(&root);
        return;
    };
    let Some(_media_mount) = BindMount::new(&data.join("media"), &media) else {
        eprintln!("skipped: bind mounts need root");
        return;
    };
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

    let out = run_cmd(&root, &["config", "validate"]);
    assert!(!out.status.success(), "validate must see the overlap");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("second mount"), "stderr={stderr}");

    let out = run_cmd(&root, &["link", "--no-progress"]);
    assert!(!out.status.success(), "link must refuse to run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("second mount"), "stderr={stderr}");
    for copy in [&copy_a, &copy_b] {
        let meta = fs::symlink_metadata(copy).expect("library copy");
        assert!(
            meta.file_type().is_file(),
            "{} stays a file",
            copy.display()
        );
    }
    assert_ne!(
        fs::metadata(&copy_a).expect("a").ino(),
        fs::metadata(&copy_b).expect("b").ino(),
        "the library copies stay separate files"
    );

    drop(_media_mount);
    drop(_downloads_mount);
    let _ = fs::remove_dir_all(&root);
}

fn configure_small_files(root: &Path, downloads: &Path, media: &Path) {
    run_ok(
        root,
        &[
            "config",
            "set-downloads",
            downloads.to_str().expect("downloads str"),
        ],
    );
    run_ok(
        root,
        &["config", "set-media", media.to_str().expect("media str")],
    );
    set_min_age_zero(root);
}

#[test]
fn link_summary_says_how_many_downloads_became_symlinks() {
    let root = unique_temp_dir("symlink_summary");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("a.mkv"), 5000, 1);
    write_seeded(&media.join("a.mkv"), 5000, 1);
    write_seeded(&downloads.join("b.mkv"), 6000, 2);
    write_seeded(&media.join("b.mkv"), 6000, 2);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["link", "--symlink-only", "--no-progress"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("2 download(s) became symlinks"),
        "stdout={stdout}"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn scans_list_symlinks_into_the_library_that_no_longer_resolve() {
    let root = unique_temp_dir("dangling_symlinks");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(downloads.join("Show.S01")).expect("create downloads");
    fs::create_dir_all(media.join("Show")).expect("create media");
    // A link DejaView made, whose library file Sonarr has since upgraded away.
    std::os::unix::fs::symlink(
        "../../media/Show/e01.mkv",
        downloads.join("Show.S01").join("e01.mkv"),
    )
    .expect("dangling symlink");
    // A symlink pointing outside the library is none of DejaView's business.
    std::os::unix::fs::symlink("/nonexistent/elsewhere", downloads.join("other.lnk"))
        .expect("unrelated symlink");
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["scan", "--json"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse json");
    let dangling = report["dangling_symlinks"]
        .as_array()
        .expect("dangling list");
    assert_eq!(dangling.len(), 1, "{dangling:?}");
    assert!(
        dangling[0]
            .as_str()
            .expect("path")
            .ends_with("Show.S01/e01.mkv")
    );

    let out = run_cmd(&root, &["scan", "--no-progress"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no longer resolve"), "stderr={stderr}");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn config_validate_tries_a_real_hardlink() {
    let root = unique_temp_dir("validate_hardlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    write_seeded(&media.join("movie.mkv"), 5000, 1);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["config", "validate"]);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hardlinks work"), "stdout={stdout}");
    let leftovers: Vec<_> = fs::read_dir(&downloads).expect("read").collect();
    assert!(leftovers.is_empty(), "the probe link is removed");

    // Downloads on another filesystem: the probe says why links will be symlinks.
    let shm = Path::new("/dev/shm");
    if shm.is_dir()
        && fs::metadata(shm).expect("shm").dev() != fs::metadata(&media).expect("media").dev()
    {
        let other = shm.join(format!("dejaview_validate_{}", std::process::id()));
        fs::create_dir_all(&other).expect("create shm downloads");
        run_ok(
            &root,
            &["config", "set-downloads", other.to_str().expect("str")],
        );
        let out = run_cmd(&root, &["config", "validate"]);
        let _ = fs::remove_dir_all(&other);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "a fallback is not a failure: {stderr}"
        );
        assert!(
            stderr.contains("different filesystems"),
            "stdout={stdout}\nstderr={stderr}"
        );
    }
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_second_run_exits_3_while_another_holds_the_lock() {
    let root = unique_temp_dir("run_lock");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("a.mkv"), 5000, 1);
    write_seeded(&media.join("a.mkv"), 5000, 1);
    configure_small_files(&root, &downloads, &media);

    // Stand in for a run in progress: hold the lock it would take.
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(downloads.join(".dejaview_lock"))
        .expect("open lock file");
    held.try_lock().expect("take the lock");

    for args in [
        &["scan", "--no-progress"][..],
        &["link", "--no-progress"],
        &["cache", "cleanup-temp"],
    ] {
        let out = run_cmd(&root, args);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{args:?} stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("another DejaView run"),
            "{args:?} stderr={stderr}"
        );
    }
    let src = fs::metadata(downloads.join("a.mkv")).expect("download");
    let lib = fs::metadata(media.join("a.mkv")).expect("library");
    assert_ne!(src.ino(), lib.ino(), "the locked-out link changed nothing");

    held.unlock().expect("release");
    drop(held);
    let out = run_cmd(&root, &["link", "--no-progress"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

fn run_cmd_env(root: &Path, args: &[&str], env: &[(&str, &Path)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dejaview"));
    cmd.args(args)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .env_remove("XDG_STATE_HOME")
        .env_remove("DEJAVIEW_CONFIG")
        .env_remove("DEJAVIEW_LOG");
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run command")
}

#[test]
fn an_unwritable_log_location_warns_and_the_command_still_runs() {
    let root = unique_temp_dir("unwritable_log");
    fs::create_dir_all(&root).expect("create root");
    // A state folder under a regular file cannot be created, even by root.
    let blocker = root.join("not-a-dir");
    fs::write(&blocker, b"file").expect("write blocker");
    let state = blocker.join("state");

    let out = run_cmd_env(&root, &["config", "show"], &[("XDG_STATE_HOME", &state)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr={stderr}");
    assert!(
        stderr.contains(&state.display().to_string()),
        "the warning names the log path: {stderr}"
    );
    assert!(stderr.contains("DEJAVIEW_LOG"), "and the way out: {stderr}");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn dejaview_config_and_dejaview_log_choose_the_files() {
    let root = unique_temp_dir("env_paths");
    let downloads = root.join("downloads");
    fs::create_dir_all(&downloads).expect("create downloads");
    let config = root.join("elsewhere").join("dejaview.json");
    let log = root.join("logs").join("run.log");

    let out = run_cmd_env(
        &root,
        &["config", "set-downloads", downloads.to_str().expect("str")],
        &[("DEJAVIEW_CONFIG", &config), ("DEJAVIEW_LOG", &log)],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = fs::read_to_string(&config).expect("config written at DEJAVIEW_CONFIG");
    assert!(saved.contains("downloads"), "{saved}");
    assert!(
        !root
            .join("xdg")
            .join("dejaview")
            .join("config.json")
            .exists(),
        "the XDG config is left alone"
    );
    let logged = fs::read_to_string(&log).expect("log written at DEJAVIEW_LOG");
    assert!(logged.contains("run started"), "{logged}");

    let out = run_cmd_env(
        &root,
        &["config", "show"],
        &[
            ("DEJAVIEW_CONFIG", &config),
            ("DEJAVIEW_LOG", Path::new("-")),
        ],
    );
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(downloads.to_str().expect("str")),
        "config show reads DEJAVIEW_CONFIG"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("run started"),
        "DEJAVIEW_LOG=- logs to stderr"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn link_with_a_plan_links_only_the_reviewed_pairs() {
    let root = unique_temp_dir("plan_file");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("reviewed.mkv"), 5000, 1);
    write_seeded(&media.join("reviewed.mkv"), 5000, 1);
    // Appeared after the scan the person reviewed.
    write_seeded(&downloads.join("new.mkv"), 6000, 2);
    write_seeded(&media.join("new.mkv"), 6000, 2);
    // Reviewed with one target; the library now plans it to another.
    write_seeded(&downloads.join("moved.mkv"), 7000, 3);
    write_seeded(&media.join("moved.mkv"), 7000, 3);
    configure_small_files(&root, &downloads, &media);
    let plan = root.join("plan.json");
    fs::write(
        &plan,
        serde_json::to_vec(&serde_json::json!([
            {"source": downloads.join("reviewed.mkv"), "target": media.join("reviewed.mkv")},
            {"source": downloads.join("moved.mkv"), "target": media.join("old-name.mkv")},
        ]))
        .expect("encode plan"),
    )
    .expect("write plan");

    let out = run_cmd(
        &root,
        &["link", "--json", "--plan", plan.to_str().expect("plan str")],
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ino = |path: PathBuf| fs::metadata(path).expect("metadata").ino();
    assert_eq!(
        ino(downloads.join("reviewed.mkv")),
        ino(media.join("reviewed.mkv"))
    );
    assert_ne!(
        ino(downloads.join("new.mkv")),
        ino(media.join("new.mkv")),
        "a pair nobody reviewed is left alone"
    );
    assert_ne!(
        ino(downloads.join("moved.mkv")),
        ino(media.join("moved.mkv")),
        "a pair whose target changed is left alone"
    );

    fs::write(&plan, b"[]").expect("empty plan");
    let out = run_cmd(&root, &["link", "--plan", plan.to_str().expect("plan str")]);
    assert!(
        !out.status.success(),
        "an empty plan links nothing, not everything"
    );
    assert_ne!(ino(downloads.join("new.mkv")), ino(media.join("new.mkv")));
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_signal_stops_a_scan_and_keeps_the_hashes_it_finished() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = unique_temp_dir("signal");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // A small pair is hashed whole by its quick hash, before the full pass.
    write_seeded(&media.join("small.mkv"), 300_000, 1);
    write_seeded(&downloads.join("small.mkv"), 300_000, 1);
    // A pair of sparse files: they take no disk, and their full hashes take
    // long enough that the signal lands while they are being read.
    for path in [media.join("big.mkv"), downloads.join("big.mkv")] {
        let file = fs::File::create(&path).expect("create sparse file");
        file.set_len(4 << 30).expect("size sparse file");
    }
    configure_small_files(&root, &downloads, &media);

    let mut child = Command::new(env!("CARGO_BIN_EXE_dejaview"))
        .args(["scan", "--tui-events", "--no-progress"])
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start scan");
    let mut events = Vec::new();
    let mut signalled = false;
    for line in BufReader::new(child.stdout.take().expect("stdout")).lines() {
        let line = line.expect("read event");
        let event: serde_json::Value = serde_json::from_str(&line).expect("parse event");
        if !signalled && event["type"] == "phase" && event["phase"] == "hashing_full" {
            let status = Command::new("kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .expect("send SIGINT");
            assert!(status.success());
            signalled = true;
        }
        events.push(event);
    }
    let out = child.wait_with_output().expect("wait for scan");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(signalled, "the scan reached its full pass: {events:?}");
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert_eq!(events.last().expect("events")["type"], "cancelled");
    assert!(stderr.contains("Stopping before the next file"), "{stderr}");
    let cache: serde_json::Value = serde_json::from_slice(
        &fs::read(downloads.join(".dejaview_cache.json")).expect("cache saved"),
    )
    .expect("parse cache");
    assert!(
        cache.get("downloads/small.mkv").is_some() && cache.get("media/small.mkv").is_some(),
        "the hashes finished before the signal are kept: {cache}"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// The events of a `--tui-events` run, which must succeed.
fn events_of(root: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let mut args = args.to_vec();
    args.extend(["--tui-events", "--no-progress"]);
    let out = run_cmd(root, &args);
    assert!(
        out.status.success(),
        "args={args:?} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse event"))
        .collect()
}

fn walked(events: &[serde_json::Value]) -> bool {
    events
        .iter()
        .any(|event| event["type"] == "phase" && event["phase"] == "collecting")
}

fn logged(events: &[serde_json::Value], text: &str) -> bool {
    events.iter().any(|event| {
        event["type"] == "log" && event["message"].as_str().is_some_and(|m| m.contains(text))
    })
}

#[test]
fn reviewed_rows_the_cache_vouches_for_are_linked_without_a_walk() {
    let root = unique_temp_dir("plan_from_cache");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("Movie/movie.mkv"), 5000, 1);
    write_seeded(&media.join("Movies/movie.mkv"), 5000, 1);
    write_seeded(&downloads.join("Show/ep.mkv"), 6000, 2);
    write_seeded(&media.join("TV/ep.mkv"), 6000, 2);
    configure_small_files(&root, &downloads, &media);
    let scan = events_of(&root, &["scan"]);
    assert!(walked(&scan));

    let plan = root.join("plan.json");
    let write_plan = |pairs: serde_json::Value| {
        fs::write(&plan, serde_json::to_vec(&pairs).expect("encode plan")).expect("write plan");
    };
    write_plan(serde_json::json!([
        {"source": downloads.join("Movie/movie.mkv"), "target": media.join("Movies/movie.mkv")},
    ]));
    let link = events_of(&root, &["link", "--plan", plan.to_str().expect("plan str")]);
    assert!(!walked(&link), "the cache vouched, so no walk: {link:?}");
    assert!(
        logged(&link, "The cache vouches for all 1 reviewed row(s)"),
        "{link:?}"
    );
    let ino = |path: PathBuf| fs::metadata(path).expect("metadata").ino();
    assert_eq!(
        ino(downloads.join("Movie/movie.mkv")),
        ino(media.join("Movies/movie.mkv"))
    );

    // A download changed since the scan: the run walks and checks it again.
    let episode = downloads.join("Show/ep.mkv");
    write_seeded(&episode, 6000, 2);
    write_plan(serde_json::json!([
        {"source": episode, "target": media.join("TV/ep.mkv")},
    ]));
    let link = events_of(&root, &["link", "--plan", plan.to_str().expect("plan str")]);
    assert!(walked(&link), "{link:?}");
    assert!(
        logged(&link, "changed since it was hashed"),
        "the log says why the cache could not vouch: {link:?}"
    );
    assert_eq!(ino(episode), ino(media.join("TV/ep.mkv")));

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_walk_paces_its_progress_events() {
    let root = unique_temp_dir("paced_events");
    let downloads = root.join("downloads");
    let media = root.join("media");
    for folder in 0..20 {
        for file in 0..100 {
            write_seeded(
                &downloads.join(format!("f{folder}/{file}.mkv")),
                64,
                (folder * 100 + file) as u8,
            );
        }
    }
    fs::create_dir_all(&media).expect("create media");
    configure_small_files(&root, &downloads, &media);

    let events = events_of(&root, &["scan"]);
    let file_events = events.iter().filter(|e| e["type"] == "file").count();
    assert!(
        file_events < 200,
        "2000 files found, {file_events} file events: one per 100 ms at most"
    );
    // The walk's own last count, before hashing starts; the planning count
    // later carries the same number and must not stand in for it.
    let walk_ends = events
        .iter()
        .position(|e| e["type"] == "phase" && e["phase"] == "hashing_sample")
        .expect("the walk ends");
    let last_count = events[..walk_ends]
        .iter()
        .rev()
        .find_map(|e| e["files_seen"].as_u64())
        .expect("a count event from the walk");
    assert_eq!(last_count, 2000, "the final count is never paced away");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_reviewed_download_hardlinked_into_the_library_since_the_scan_is_left_alone() {
    let root = unique_temp_dir("plan_hardlinked_since");
    let downloads = root.join("downloads");
    let media = root.join("media");
    let download = downloads.join("movie.mkv");
    write_seeded(&download, 5000, 1);
    write_seeded(&media.join("A/movie.mkv"), 5000, 1);
    // Another copy, so later scans read the group again.
    write_seeded(&downloads.join("copy/movie.mkv"), 5000, 1);
    configure_small_files(&root, &downloads, &media);
    events_of(&root, &["scan"]);

    // Sonarr imports the download by hardlinking it into the library, and a
    // scan from cron refreshes the download's cache entry.
    fs::create_dir_all(media.join("B")).expect("create B");
    fs::hard_link(&download, media.join("B/movie.mkv")).expect("hardlink into library");
    events_of(&root, &["scan"]);

    let plan = root.join("plan.json");
    fs::write(
        &plan,
        serde_json::to_vec(&serde_json::json!([
            {"source": download, "target": media.join("A/movie.mkv")},
        ]))
        .expect("encode plan"),
    )
    .expect("write plan");
    let link = events_of(&root, &["link", "--plan", plan.to_str().expect("plan str")]);
    assert!(
        walked(&link),
        "a download with other hardlinks is checked by the walk: {link:?}"
    );
    let ino = |path: PathBuf| fs::metadata(path).expect("metadata").ino();
    assert_eq!(
        ino(download.clone()),
        ino(media.join("B/movie.mkv")),
        "the hardlink Sonarr made is kept"
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_run_under_nohup_keeps_running_when_the_terminal_hangs_up() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = unique_temp_dir("nohup");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");
    for path in [media.join("big.mkv"), downloads.join("big.mkv")] {
        let file = fs::File::create(&path).expect("create sparse file");
        file.set_len(1 << 30).expect("size sparse file");
    }
    configure_small_files(&root, &downloads, &media);

    // nohup ignores SIGHUP and then runs DejaView in its own place.
    let mut child = Command::new("nohup")
        .arg(env!("CARGO_BIN_EXE_dejaview"))
        .args(["scan", "--tui-events", "--no-progress"])
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start scan under nohup");
    let mut events = Vec::new();
    for line in BufReader::new(child.stdout.take().expect("stdout")).lines() {
        let event: serde_json::Value =
            serde_json::from_str(&line.expect("read event")).expect("parse event");
        if event["type"] == "phase" && event["phase"] == "hashing_full" {
            let status = Command::new("kill")
                .args(["-HUP", &child.id().to_string()])
                .status()
                .expect("send SIGHUP");
            assert!(status.success());
        }
        events.push(event);
    }
    let out = child.wait_with_output().expect("wait for scan");

    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(events.last().expect("events")["type"], "done");

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// `dir` bind-mounted read-only onto itself, so nothing in it can be
/// replaced, even by root. None where this process may not mount.
fn read_only(dir: &Path) -> Option<BindMount> {
    let mount = BindMount::new(dir, dir)?;
    let status = Command::new("mount")
        .args(["-o", "remount,ro,bind"])
        .arg(dir)
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(mount)
}

/// One pair that links and one in a read-only folder that cannot.
fn tree_with_one_pair_that_cannot_link(root: &Path) -> Option<(PathBuf, BindMount)> {
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("ok").join("b.mkv"), 3000, 1);
    write_seeded(&media.join("b.mkv"), 3000, 1);
    write_seeded(&downloads.join("ro").join("a.mkv"), 2000, 2);
    write_seeded(&media.join("a.mkv"), 2000, 2);
    configure_small_files(root, &downloads, &media);
    let mount = read_only(&downloads.join("ro"))?;
    Some((downloads, mount))
}

#[test]
fn a_link_run_with_a_failed_pair_exits_2_and_says_why() {
    let root = unique_temp_dir("exit_2");
    let Some((_downloads, mount)) = tree_with_one_pair_that_cannot_link(&root) else {
        eprintln!("skipped: this process may not mount");
        let _ = fs::remove_dir_all(&root);
        return;
    };

    let out = run_cmd(&root, &["link", "--json", "--no-progress"]);
    drop(mount);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr={stderr}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse report");
    assert_eq!(report["outcome"], "complete");
    assert_eq!(report["applied_actions"], 1);
    assert_eq!(report["freed_bytes"], 3000);
    let failed = &report["entries"][1];
    assert_eq!(failed["status"], "failed", "{report}");
    assert!(
        failed["error"]
            .as_str()
            .is_some_and(|error| error.contains("Read-only file system")),
        "the OS error is kept: {failed}"
    );
    assert_eq!(failed["method"], serde_json::Value::Null);
    assert_eq!(failed["fallback_reason"], serde_json::Value::Null);
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_strict_json_run_prints_its_report_before_the_error() {
    let root = unique_temp_dir("strict_json");
    let Some((_downloads, mount)) = tree_with_one_pair_that_cannot_link(&root) else {
        eprintln!("skipped: this process may not mount");
        let _ = fs::remove_dir_all(&root);
        return;
    };

    let out = run_cmd(&root, &["link", "--json", "--strict", "--no-progress"]);
    drop(mount);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    assert!(stderr.contains("--strict stopped the run"), "{stderr}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse report");
    assert_eq!(report["outcome"], "failed");
    assert_eq!(report["applied_actions"], 1);
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// The events of a `--tui-events` run, whatever its exit code.
fn stream_of(out: &std::process::Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is one event"))
        .collect()
}

/// A stream opens with hello and ends with exactly one done, error or
/// cancelled, which it returns.
fn final_event_of(events: &[serde_json::Value]) -> &serde_json::Value {
    assert_eq!(
        events.first().expect("events")["type"],
        "hello",
        "{events:?}"
    );
    assert_eq!(events[0]["protocol"], 1);
    let finals: Vec<_> = events
        .iter()
        .filter(|event| {
            ["done", "error", "cancelled"].contains(&event["type"].as_str().unwrap_or(""))
        })
        .collect();
    assert_eq!(finals.len(), 1, "exactly one final event: {events:?}");
    let last = events.last().expect("events");
    assert_eq!(finals[0], last, "the final event is the last: {events:?}");
    last
}

#[test]
fn a_config_error_ends_the_event_stream_with_one_error() {
    let root = unique_temp_dir("stream_config_error");
    fs::create_dir_all(&root).expect("create root");

    let out = run_cmd(&root, &["scan", "--tui-events", "--no-progress"]);

    assert_eq!(out.status.code(), Some(1));
    let events = stream_of(&out);
    let last = final_event_of(&events);
    assert_eq!(last["type"], "error");
    assert!(
        last["message"]
            .as_str()
            .is_some_and(|message| message.contains("set-downloads")),
        "{last}"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// Run as a user that cannot read a file of mode 000: this one, or `nobody`
/// under root, which reads anything. Under root the tree is handed to nobody.
fn run_unprivileged(root: &Path, args: &[&str]) -> std::process::Output {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(env!("CARGO_BIN_EXE_dejaview"));
    command
        .args(args)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"));
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0");
    if is_root {
        const NOBODY: u32 = 65534;
        for entry in walkdir_all(root) {
            std::os::unix::fs::lchown(&entry, Some(NOBODY), Some(NOBODY)).expect("chown");
        }
        command.uid(NOBODY).gid(NOBODY);
    }
    command.output().expect("run command")
}

/// Every path under `root`, `root` included.
fn walkdir_all(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![root.to_path_buf()];
    let mut index = 0;
    while index < paths.len() {
        if paths[index].is_dir() && !paths[index].is_symlink() {
            for entry in fs::read_dir(&paths[index]).expect("read dir") {
                paths.push(entry.expect("entry").path());
            }
        }
        index += 1;
    }
    paths
}

#[test]
fn a_strict_hash_failure_ends_the_event_stream_with_one_error() {
    use std::os::unix::fs::PermissionsExt;
    let root = unique_temp_dir("stream_hash_error");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("unreadable.mkv"), 4000, 1);
    write_seeded(&media.join("unreadable.mkv"), 4000, 1);
    configure_small_files(&root, &downloads, &media);
    fs::create_dir_all(root.join("home")).expect("create home");
    fs::set_permissions(
        downloads.join("unreadable.mkv"),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make unreadable");

    let out = run_unprivileged(
        &root,
        &["scan", "--strict", "--tui-events", "--no-progress"],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr={stderr}");
    let events = stream_of(&out);
    let last = final_event_of(&events);
    assert_eq!(last["type"], "error");
    assert!(
        last["message"]
            .as_str()
            .is_some_and(|message| message.contains("unreadable.mkv")
                && message.contains("Permission denied")),
        "the whole cause: {last}"
    );
    fs::set_permissions(
        downloads.join("unreadable.mkv"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("make readable");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_strict_link_failure_ends_the_event_stream_with_its_report() {
    let root = unique_temp_dir("stream_link_error");
    let Some((_downloads, mount)) = tree_with_one_pair_that_cannot_link(&root) else {
        eprintln!("skipped: this process may not mount");
        let _ = fs::remove_dir_all(&root);
        return;
    };

    let out = run_cmd(
        &root,
        &["link", "--strict", "--tui-events", "--no-progress"],
    );
    drop(mount);

    assert_eq!(out.status.code(), Some(1));
    let events = stream_of(&out);
    let last = final_event_of(&events);
    assert_eq!(last["type"], "error");
    let statuses: Vec<_> = last["report"]["entries"]
        .as_array()
        .expect("the report of the pairs before the failure")
        .iter()
        .map(|entry| entry["status"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(statuses, ["applied", "failed"]);
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_cancel_ends_the_event_stream_with_one_cancelled() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let root = unique_temp_dir("stream_cancel");
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");
    // Sparse files take no disk, and their full hashes take long enough for
    // the cancel to land while they are read.
    for path in [media.join("big.mkv"), downloads.join("big.mkv")] {
        let file = fs::File::create(&path).expect("create sparse file");
        file.set_len(4 << 30).expect("size sparse file");
    }
    configure_small_files(&root, &downloads, &media);

    let mut child = Command::new(env!("CARGO_BIN_EXE_dejaview"))
        .args(["scan", "--tui-events", "--no-progress"])
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start scan");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut events = Vec::new();
    for line in BufReader::new(child.stdout.take().expect("stdout")).lines() {
        let event: serde_json::Value =
            serde_json::from_str(&line.expect("read event")).expect("parse event");
        if event["type"] == "phase" && event["phase"] == "hashing_full" {
            stdin.write_all(b"cancel\n").expect("send cancel");
            stdin.flush().expect("flush cancel");
        }
        events.push(event);
    }
    let out = child.wait_with_output().expect("wait for scan");

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(final_event_of(&events)["type"], "cancelled");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_scan_names_each_folder_once_and_each_path_below_it() {
    let root = unique_temp_dir("human_scan");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("Show.S01").join("e01.mkv"), 5000, 1);
    write_seeded(&media.join("TV").join("Show").join("e01.mkv"), 5000, 1);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["scan", "--no-progress"]);

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.matches(&*downloads.to_string_lossy()).count(),
        1,
        "the downloads folder is printed once: {stdout}"
    );
    assert!(stdout.contains("Show.S01/e01.mkv"), "{stdout}");
    assert!(stdout.contains("TV/Show/e01.mkv"), "{stdout}");
    assert!(stdout.contains("Found 1 duplicate(s)"), "{stdout}");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_scan_that_finds_nothing_says_so_without_a_table() {
    let root = unique_temp_dir("human_nothing");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("a.mkv"), 5000, 1);
    write_seeded(&media.join("b.mkv"), 5000, 2);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["scan", "--no-progress"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("No duplicates found."), "{stdout}");
    assert!(!stdout.contains("Library file"), "no empty table: {stdout}");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_link_run_ends_with_what_it_linked_and_freed() {
    let root = unique_temp_dir("human_verdict");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("a.mkv"), 2048, 1);
    write_seeded(&media.join("a.mkv"), 2048, 1);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["link", "--no-progress"]);

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().last(),
        Some("✔ Linked 1 of 1, 2.0 KiB freed"),
        "{stdout}"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_quiet_run_prints_only_what_it_linked() {
    let root = unique_temp_dir("quiet");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&downloads.join("a.mkv"), 2048, 1);
    write_seeded(&media.join("a.mkv"), 2048, 1);
    configure_small_files(&root, &downloads, &media);

    let out = run_cmd(&root, &["link", "--quiet"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "✔ Linked 1 of 1, 2.0 KiB freed\n"
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");

    // Nothing left to link: nothing to say.
    let out = run_cmd(&root, &["link", "--quiet"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_quiet_run_with_a_failed_pair_says_so() {
    let root = unique_temp_dir("quiet_failed");
    let Some((downloads, mount)) = tree_with_one_pair_that_cannot_link(&root) else {
        eprintln!("skipped: this process may not mount");
        let _ = fs::remove_dir_all(&root);
        return;
    };

    let out = run_cmd(&root, &["link", "--quiet"]);
    drop(mount);

    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report = downloads.join(".dejaview_link_report.json");
    assert_eq!(
        stdout.trim_end(),
        format!(
            "⚠ Linked 1 of 2, 2.9 KiB freed, 1 failed: see {}",
            report.display()
        )
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("Read-only file system"));
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn quiet_and_json_cannot_be_combined() {
    let root = unique_temp_dir("quiet_json");
    fs::create_dir_all(&root).expect("create root");
    let out = run_cmd(&root, &["link", "--quiet", "--json"]);
    assert_eq!(out.status.code(), Some(2), "clap's usage error");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn setting_a_folder_that_does_not_exist_names_it() {
    let root = unique_temp_dir("set_missing");
    fs::create_dir_all(&root).expect("create root");
    let missing = root.join("not-mounted");

    let out = run_cmd(
        &root,
        &["config", "set-downloads", missing.to_str().expect("path")],
    );

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&missing.display().to_string()),
        "the error names the path: {stderr}"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn the_full_pass_reports_its_progress_in_bytes() {
    let root = unique_temp_dir("hash_bytes");
    let downloads = root.join("downloads");
    let media = root.join("media");
    // Bigger than the quick hash's sample, so the pair is read in full.
    write_seeded(&downloads.join("big.mkv"), 3 << 20, 7);
    write_seeded(&media.join("big.mkv"), 3 << 20, 7);
    configure_small_files(&root, &downloads, &media);

    let events = events_of(&root, &["scan"]);

    let bytes: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "hash_bytes")
        .collect();
    assert_eq!(
        bytes.first().map(|event| event["done"].clone()),
        Some(0.into())
    );
    let last = bytes.last().expect("hash_bytes events");
    assert_eq!(last["total"], 2 * (3 << 20), "{events:?}");
    assert_eq!(last["done"], last["total"], "the pass ends complete");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_leftover_temp_symlink_is_listed_and_removed_without_touching_its_target() {
    let root = unique_temp_dir("temp_symlink");
    let downloads = root.join("downloads");
    let media = root.join("media");
    write_seeded(&media.join("movie.mkv"), 4096, 3);
    fs::create_dir_all(downloads.join("Movie")).expect("create download folder");
    configure_small_files(&root, &downloads, &media);
    // What a run killed between the symlink and the rename leaves behind.
    let leftover = downloads.join("Movie").join(".dejaview_link.tmp.123.456");
    std::os::unix::fs::symlink("../../media/movie.mkv", &leftover).expect("make leftover");

    let out = run_cmd(&root, &["cache", "audit-temp"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(".dejaview_link.tmp.123.456"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    run_ok(&root, &["cache", "cleanup-temp"]);
    assert!(
        fs::symlink_metadata(&leftover).is_err(),
        "the leftover is gone"
    );
    assert_eq!(
        fs::metadata(media.join("movie.mkv"))
            .expect("library file")
            .len(),
        4096,
        "the library file it pointed at is untouched"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

/// Downloads `a\xff.mkv` and `a\xfe.mkv`, whose names differ only in a byte
/// that is not UTF-8, each a copy of its own library file.
fn tree_with_names_that_are_not_utf8(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let downloads = root.join("downloads");
    let media = root.join("media");
    let ff = downloads.join(OsStr::from_bytes(b"a\xff.mkv"));
    let fe = downloads.join(OsStr::from_bytes(b"a\xfe.mkv"));
    write_seeded(&ff, 3000, 1);
    write_seeded(&media.join("x.mkv"), 3000, 1);
    write_seeded(&fe, 3000, 2);
    write_seeded(&media.join("y.mkv"), 3000, 2);
    configure_small_files(root, &downloads, &media);
    (media, ff, fe)
}

#[test]
fn names_that_differ_only_in_bytes_that_are_not_utf8_stay_apart() {
    let root = unique_temp_dir("not_utf8");
    let (_media, _ff, _fe) = tree_with_names_that_are_not_utf8(&root);

    let out = run_cmd(&root, &["scan", "--json", "--no-progress"]);

    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse report");
    let raws: Vec<_> = report["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["source_raw"].as_str().expect("raw bytes").to_string())
        .collect();
    assert_eq!(raws.len(), 2);
    assert_ne!(raws[0], raws[1], "{report}");
    let cache: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("downloads").join(".dejaview_cache.json")).expect("cache"),
    )
    .expect("parse cache");
    assert_eq!(
        cache.as_object().expect("cache entries").len(),
        4,
        "{cache}"
    );
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
fn a_plan_row_whose_path_is_not_utf8_links_that_file() {
    let root = unique_temp_dir("not_utf8_plan");
    let (media, ff, fe) = tree_with_names_that_are_not_utf8(&root);
    let out = run_cmd(&root, &["scan", "--json", "--no-progress"]);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("parse report");
    // The row for a\xfe.mkv, as the dashboard writes it to a plan.
    let row = report["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| {
            entry["source_raw"]
                .as_str()
                .is_some_and(|raw| raw.ends_with("61fe2e6d6b76"))
        })
        .expect("the a\\xfe.mkv row")
        .clone();
    let plan = root.join("plan.json");
    fs::write(
        &plan,
        serde_json::to_vec(&serde_json::json!([{
            "source": row["source"],
            "target": row["target"],
            "source_raw": row["source_raw"],
            "target_raw": row["target_raw"],
        }]))
        .expect("encode plan"),
    )
    .expect("write plan");

    run_ok(
        &root,
        &[
            "link",
            "--plan",
            plan.to_str().expect("plan path"),
            "--no-progress",
        ],
    );

    let inode = |path: &Path| fs::metadata(path).expect("metadata").ino();
    assert_eq!(
        inode(&fe),
        inode(&media.join("y.mkv")),
        "the reviewed row is linked"
    );
    assert_ne!(inode(&ff), inode(&media.join("x.mkv")), "and nothing else");
    fs::remove_dir_all(&root).expect("cleanup temp dir");
}
