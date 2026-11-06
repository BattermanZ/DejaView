use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "dejaview_perf_{prefix}_{}_{}",
        std::process::id(),
        ts
    ))
}

fn run_cmd(root: &Path, args: &[&str]) -> (std::process::Output, Duration) {
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_dejaview"))
        .args(args)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("HOME", root.join("home"))
        .output()
        .expect("run command");
    (output, start.elapsed())
}

fn run_ok(root: &Path, args: &[&str]) {
    let (out, _) = run_cmd(root, args);
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

fn write_pattern_file(path: &Path, size: usize, seed: u8) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let mut data = vec![0u8; size];
    for (i, b) in data.iter_mut().enumerate() {
        *b = seed.wrapping_add((i % 251) as u8);
    }
    fs::write(path, data).expect("write file");
}

#[derive(Clone, Copy)]
struct PerfProfile {
    name: &'static str,
    pairs: usize,
    bytes_per_file: usize,
}

fn threshold_ms_env(name: &str, default: u128) -> u128 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u128>().ok())
        .unwrap_or(default)
}

fn build_dataset(root: &Path, profile: PerfProfile) -> (PathBuf, PathBuf) {
    let downloads = root.join("downloads");
    let media = root.join("media");
    fs::create_dir_all(&downloads).expect("create downloads");
    fs::create_dir_all(&media).expect("create media");

    for i in 0..profile.pairs {
        let media_path = media.join(format!("m_{i:05}.bin"));
        write_pattern_file(&media_path, profile.bytes_per_file, (i % 251) as u8);

        let downloads_path = downloads.join(format!("d_{i:05}.bin"));
        if i % 2 == 0 {
            fs::copy(&media_path, &downloads_path).expect("copy duplicate to downloads");
        } else {
            write_pattern_file(
                &downloads_path,
                profile.bytes_per_file,
                ((i + 37) % 251) as u8,
            );
        }
    }

    (downloads, media)
}

fn run_profile(profile: PerfProfile) {
    let root = unique_temp_dir(profile.name);
    let (downloads, media) = build_dataset(&root, profile);

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

    let (cold_out, cold_elapsed) = run_cmd(&root, &["scan"]);
    assert!(
        cold_out.status.success(),
        "cold scan failed: stdout={} stderr={}",
        String::from_utf8_lossy(&cold_out.stdout),
        String::from_utf8_lossy(&cold_out.stderr)
    );

    let (warm_out, warm_elapsed) = run_cmd(&root, &["scan"]);
    assert!(
        warm_out.status.success(),
        "warm scan failed: stdout={} stderr={}",
        String::from_utf8_lossy(&warm_out.stdout),
        String::from_utf8_lossy(&warm_out.stderr)
    );

    let cold_var = format!("DEJAVIEW_PERF_{}_COLD_MS", profile.name.to_uppercase());
    let warm_var = format!("DEJAVIEW_PERF_{}_WARM_MS", profile.name.to_uppercase());

    let cold_default = match profile.name {
        "small" => 2_000,
        "medium" => 7_000,
        "large" => 20_000,
        _ => 10_000,
    };
    let warm_default = match profile.name {
        "small" => 1_500,
        "medium" => 4_000,
        "large" => 10_000,
        _ => 5_000,
    };

    let cold_threshold = threshold_ms_env(&cold_var, cold_default);
    let warm_threshold = threshold_ms_env(&warm_var, warm_default);

    let cold_ms = cold_elapsed.as_millis();
    let warm_ms = warm_elapsed.as_millis();
    println!(
        "profile={} cold={}ms (<= {}ms) warm={}ms (<= {}ms)",
        profile.name, cold_ms, cold_threshold, warm_ms, warm_threshold
    );

    assert!(
        cold_ms <= cold_threshold,
        "cold scan regression for {}: {}ms > {}ms",
        profile.name,
        cold_ms,
        cold_threshold
    );
    assert!(
        warm_ms <= warm_threshold,
        "warm scan regression for {}: {}ms > {}ms",
        profile.name,
        warm_ms,
        warm_threshold
    );

    fs::remove_dir_all(&root).expect("cleanup temp dir");
}

#[test]
#[ignore = "performance harness; run explicitly"]
fn perf_small_dataset() {
    run_profile(PerfProfile {
        name: "small",
        pairs: 120,
        bytes_per_file: 32 * 1024,
    });
}

#[test]
#[ignore = "performance harness; run explicitly"]
fn perf_medium_dataset() {
    run_profile(PerfProfile {
        name: "medium",
        pairs: 400,
        bytes_per_file: 64 * 1024,
    });
}

#[test]
#[ignore = "performance harness; run explicitly"]
fn perf_large_dataset() {
    run_profile(PerfProfile {
        name: "large",
        pairs: 1_000,
        bytes_per_file: 64 * 1024,
    });
}
