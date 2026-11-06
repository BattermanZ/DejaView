use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result, anyhow};

use crate::config::load_config;
use crate::engine;
use crate::fsutil::safe_write_bytes;

use super::app::LinkCheck;
use super::model::{RunReport, TuiEvent};
use crate::protocol::{PROTOCOL_VERSION, PlannedPair};

/// Try one real hardlink from the library into downloads, as `config
/// validate` does, on a thread of its own: first the walk that refuses two
/// folders that reach each other through a second mount, so the test file is
/// never made inside the library, then the link. It sends what it showed and
/// the lines to log, which name the file tried and the OS error. It changes
/// nothing in the library but one file's change time.
pub fn spawn_link_check(
    downloads: std::path::PathBuf,
    media: std::path::PathBuf,
    exclude: Vec<String>,
) -> Receiver<(LinkCheck, Vec<String>)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(check_links(&downloads, &media, exclude));
    });
    receiver
}

fn check_links(downloads: &Path, media: &Path, exclude: Vec<String>) -> (LinkCheck, Vec<String>) {
    if let Err(err) = engine::ensure_roots_not_mounted_inside(downloads, media, exclude) {
        let reason = format!("{err:#}");
        return (
            LinkCheck::Refused {
                reason: reason.clone(),
            },
            vec![format!("No test hardlink made: {reason}")],
        );
    }
    match engine::probe_hardlink(downloads, media) {
        engine::HardlinkProbe::NoFile => (
            LinkCheck::NoLibraryFile,
            vec!["No library file to test a hardlink with yet.".to_string()],
        ),
        engine::HardlinkProbe::Works { file, leftover } => {
            let mut lines = vec![format!(
                "A test hardlink of {} into downloads worked: link runs will free space.",
                file.display()
            )];
            lines.extend(leftover);
            (LinkCheck::Hardlinks, lines)
        }
        engine::HardlinkProbe::Fails { file, cause, error } => (
            LinkCheck::Symlinks { cause },
            vec![format!(
                "A test hardlink of {} into downloads failed: {cause} ({error}). Link runs will make symlinks, which break if a library file is renamed, upgraded or deleted.",
                file.display()
            )],
        ),
    }
}

/// A child that already ended its stream with a done, error or cancelled
/// event has said how it ended; its exit code (2 for a run with failures, 1
/// for a link cancelled partway) adds nothing to show.
fn should_forward_terminal_error(status_success: bool, saw_final_event: bool) -> bool {
    !status_success && !saw_final_event
}

/// Ask the child to stop when the UI sends a cancel. If the sender is dropped
/// without sending, the child's stdin just closes, which the child ignores.
fn forward_cancel<W: Write>(cancel_rx: Receiver<()>, mut stdin: W) {
    if cancel_rx.recv().is_ok() {
        let _ = stdin.write_all(b"cancel\n");
        let _ = stdin.flush();
    }
}

/// What to show when the child exits with an error and no error event: its
/// own `Error:` line, which already says what failed. The TUI adds which run
/// it was, so this adds no prefix of its own; the log keeps the rest.
fn child_failure_message(subcommand: &str, status: &str, stderr: &str) -> String {
    let error_line = stderr
        .lines()
        .find_map(|line| line.strip_prefix("Error: "))
        .map(str::trim)
        .filter(|line| !line.is_empty());
    match error_line {
        Some(line) => line.to_string(),
        None if !stderr.trim().is_empty() => stderr.trim().to_string(),
        None => format!("{subcommand} exited with {status}"),
    }
}

/// The binary to run a scan or link with: this one. After an upgrade
/// replaces it on disk, `current_exe()` names a path that is gone, while
/// `/proc/self/exe` still reaches the binary this dashboard runs, which
/// speaks its protocol.
fn own_executable() -> Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let proc_exe = Path::new("/proc/self/exe");
        if proc_exe.exists() {
            return Ok(proc_exe.to_path_buf());
        }
    }
    std::env::current_exe().context("resolve current executable")
}

/// Check the child's first event: a `hello` in this dashboard's protocol.
/// Anything else is another version of DejaView, whose events this one may
/// read wrongly.
fn check_hello(event: &TuiEvent, exe: &Path) -> Result<()> {
    match event {
        TuiEvent::Hello { protocol, .. } if *protocol == PROTOCOL_VERSION => Ok(()),
        TuiEvent::Hello { protocol, version } => Err(anyhow!(
            "{} is DejaView {version}, whose event stream (protocol {protocol}) this dashboard (protocol {PROTOCOL_VERSION}) cannot read; quit and start the dashboard again",
            exe.display()
        )),
        _ => Err(anyhow!(
            "{} did not open its event stream with hello, so it is another version of DejaView; quit and start the dashboard again",
            exe.display()
        )),
    }
}

fn stream_subcommand_events(
    subcommand: &str,
    strict: bool,
    extra_args: &[std::ffi::OsString],
    tx: Sender<Result<TuiEvent>>,
    cancel_rx: Receiver<()>,
) -> Result<()> {
    let exe = own_executable()?;
    let mut cmd = Command::new(&exe);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.arg0("dejaview");
    }
    // A failing child's stderr is shown when it sends no final event; a
    // backtrace there is noise.
    cmd.env("RUST_BACKTRACE", "0")
        .env("RUST_LIB_BACKTRACE", "0")
        .arg(subcommand)
        .arg("--tui-events")
        .arg("--no-progress")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.args(extra_args);
    if strict {
        cmd.arg("--strict");
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("execute '{subcommand}' in subprocess"))?;
    let stdin = child.stdin.take().context("capture subprocess stdin")?;
    std::thread::spawn(move || forward_cancel(cancel_rx, stdin));
    let stdout = child.stdout.take().context("capture subprocess stdout")?;
    let stderr = child.stderr.take().context("capture subprocess stderr")?;

    let stderr_handle = std::thread::spawn(move || -> Result<String> {
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf)?;
        Ok(buf)
    });

    let mut saw_done = false;
    let mut saw_error_event = false;
    let mut saw_hello = false;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let message = format!("read streamed event line: {err}");
                let _ = tx.send(Err(anyhow!(message.clone())));
                return Err(anyhow!(message));
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let event: TuiEvent = match serde_json::from_str(&line).with_context(|| {
            format!(
                "parse streamed event from {subcommand} (first 200 bytes: {})",
                line.chars().take(200).collect::<String>()
            )
        }) {
            Ok(event) => event,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = tx.send(Err(anyhow!(err.to_string())));
                return Err(err);
            }
        };
        if !saw_hello {
            if let Err(err) = check_hello(&event, &exe) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = tx.send(Err(anyhow!("{err:#}")));
                return Err(err);
            }
            saw_hello = true;
            continue;
        }
        if matches!(event, TuiEvent::Done { .. }) {
            saw_done = true;
        }
        if matches!(event, TuiEvent::Error { .. } | TuiEvent::Cancelled) {
            saw_error_event = true;
        }
        let _ = tx.send(Ok(event));
    }

    let status = child.wait().context("wait for subprocess completion")?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| anyhow!("stderr reader thread panicked"))??;

    if !status.success() {
        let message = child_failure_message(subcommand, &status.to_string(), &stderr);
        if should_forward_terminal_error(status.success(), saw_done || saw_error_event) {
            let _ = tx.send(Err(anyhow!(message.clone())));
        }
        return Err(anyhow!(message));
    }

    if !saw_done && !saw_error_event {
        let message = format!("{subcommand} ended without a final done event");
        let _ = tx.send(Err(anyhow!(message.clone())));
        return Err(anyhow!(message));
    }

    Ok(())
}

pub fn run_scan(strict: bool, tx: Sender<Result<TuiEvent>>, cancel_rx: Receiver<()>) -> Result<()> {
    stream_subcommand_events("scan", strict, &[], tx, cancel_rx)
}

/// Link exactly the rows a person reviewed: write them to a plan file in the
/// downloads folder, run `link --plan` on it, and remove the file afterwards.
/// The child links only these (download, library file) pairs, so files found
/// since the scan, or a library file that changed, are left alone.
pub fn run_link_pairs(
    strict: bool,
    downloads: &Path,
    pairs: &[PlannedPair],
    tx: Sender<Result<TuiEvent>>,
    cancel_rx: Receiver<()>,
) -> Result<()> {
    let plan = downloads.join(format!(
        ".dejaview_tui_plan.{}.{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let written = serde_json::to_vec(pairs)
        .map_err(anyhow::Error::from)
        .and_then(|json| safe_write_bytes(&plan, &json))
        .with_context(|| format!("write the link plan {}", plan.display()));
    if let Err(err) = written {
        let _ = tx.send(Err(anyhow!("{err:#}")));
        return Err(err);
    }
    let result = stream_subcommand_events(
        "link",
        strict,
        // The path as it is, not as a string: a downloads folder whose name
        // is not UTF-8 must still be found.
        &["--plan".into(), plan.clone().into_os_string()],
        tx,
        cancel_rx,
    );
    let _ = std::fs::remove_file(&plan);
    result
}

pub fn clear_cache() -> Result<()> {
    let config = load_config()?;
    engine::clear_cache(&config)
}

pub fn save_report(report: &RunReport, path: &Path) -> Result<()> {
    let json = serde_json::to_vec_pretty(report)?;
    safe_write_bytes(path, &json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Mode, Phase};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dejaview_tui_actions_{prefix}_{}_{}",
            std::process::id(),
            ts
        ))
    }

    fn sample_report() -> RunReport {
        serde_json::from_str(
            r#"{
                "mode": "scan",
                "dry_run": true,
                "strict": false,
                "files_considered": 1,
                "planned_actions": 0,
                "applied_actions": 0,
                "total_saved_bytes": 0,
                "skipped_by_min_age": 0,
                "warnings": {
                    "walk_errors": 0,
                    "metadata_errors": 0,
                    "sample_hash_errors": 0,
                    "full_hash_errors": 0,
                    "link_errors": 0
                },
                "entries": []
            }"#,
        )
        .expect("parse sample report")
    }

    #[test]
    fn parse_run_report_from_json() {
        let json = r#"{
            "mode": "scan",
            "dry_run": true,
            "strict": false,
            "files_considered": 10,
            "planned_actions": 2,
            "applied_actions": 0,
            "total_saved_bytes": 1024,
            "skipped_by_min_age": 1,
            "warnings": {
                "walk_errors": 0,
                "metadata_errors": 0,
                "sample_hash_errors": 0,
                "full_hash_errors": 0,
                "link_errors": 0
            },
            "entries": []
        }"#;

        let report: RunReport = serde_json::from_str(json).expect("parse run report");
        assert_eq!(report.mode, Mode::Scan);
        assert_eq!(report.files_considered, 10);
        assert_eq!(report.warnings.total(), 0);
    }

    #[test]
    fn forward_cancel_writes_cancel_line_when_asked() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(()).expect("send cancel");
        let mut written = Vec::new();
        forward_cancel(rx, &mut written);
        assert_eq!(written, b"cancel\n");
    }

    #[test]
    fn forward_cancel_writes_nothing_when_job_ends() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(tx);
        let mut written = Vec::new();
        forward_cancel(rx, &mut written);
        assert!(written.is_empty());
    }

    #[test]
    fn a_failed_child_is_reported_by_its_own_error_line() {
        let stderr = "Error: another DejaView run holds the lock /d/.dejaview_lock, so this run did nothing\n";
        assert_eq!(
            child_failure_message("link", "exit status: 3", stderr),
            "another DejaView run holds the lock /d/.dejaview_lock, so this run did nothing"
        );
        assert_eq!(
            child_failure_message("scan", "exit status: 1", "  \n"),
            "scan exited with exit status: 1"
        );
    }

    #[test]
    fn a_child_that_speaks_another_protocol_is_refused() {
        let exe = Path::new("/usr/bin/dejaview");
        let hello = |protocol| TuiEvent::Hello {
            protocol,
            version: "9.9.9".to_string(),
        };
        assert!(check_hello(&hello(PROTOCOL_VERSION), exe).is_ok());
        let err = check_hello(&hello(PROTOCOL_VERSION + 1), exe).expect_err("newer protocol");
        assert!(
            err.to_string().contains("start the dashboard again"),
            "{err}"
        );
        // A binary from before the handshake opens with a phase.
        let older = TuiEvent::Phase {
            phase: Phase::Collecting,
        };
        assert!(check_hello(&older, exe).is_err());
    }

    #[test]
    fn structured_error_suppresses_terminal_error_forwarding() {
        assert!(!should_forward_terminal_error(false, true));
        assert!(should_forward_terminal_error(false, false));
    }

    #[test]
    fn save_report_rejects_symlink_target() {
        #[cfg(not(unix))]
        {
            return;
        }

        #[cfg(unix)]
        {
            let root = unique_temp_dir("save_report_symlink");
            fs::create_dir_all(&root).expect("create root");
            let victim = root.join("victim.json");
            let report_path = root.join("report.json");
            fs::write(&victim, b"keep").expect("write victim");
            symlink(&victim, &report_path).expect("create report symlink");

            let err = save_report(&sample_report(), &report_path)
                .expect_err("symlink report path should fail");
            assert!(
                err.to_string()
                    .contains("refusing to overwrite symlink target")
            );
            assert_eq!(fs::read(&victim).expect("read victim"), b"keep");

            fs::remove_dir_all(&root).expect("cleanup temp dir");
        }
    }

    /// Every file under `root`, for a test that must leave nothing behind.
    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn a_test_hardlink_between_two_folders_works_and_leaves_nothing() {
        let root = unique_temp_dir("link_check");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("downloads");
        fs::create_dir_all(media.join("Movies")).expect("media");
        fs::write(media.join("Movies").join("m.mkv"), b"movie").expect("library file");

        let (check, lines) = check_links(&downloads, &media, Vec::new());

        assert_eq!(check, LinkCheck::Hardlinks);
        assert!(
            lines[0].contains("m.mkv"),
            "names the file tried: {lines:?}"
        );
        assert!(
            files_under(&downloads).is_empty(),
            "the test link is removed"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_test_hardlink_is_made_when_the_library_is_reached_through_a_mount() {
        let root = unique_temp_dir("link_check_mount");
        // On the host the library sits inside the data folder; downloads is
        // the data folder seen through a bind mount.
        let data = root.join("data");
        fs::create_dir_all(data.join("media").join("Movies")).expect("library");
        fs::write(data.join("media").join("Movies").join("m.mkv"), b"movie").expect("file");
        let downloads = root.join("c").join("downloads");
        let media = root.join("c").join("media");
        fs::create_dir_all(&downloads).expect("downloads mount point");
        fs::create_dir_all(&media).expect("media mount point");
        let mount = |source: &Path, target: &Path| {
            std::process::Command::new("mount")
                .arg("--bind")
                .arg(source)
                .arg(target)
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        if !mount(&data, &downloads) {
            eprintln!("skipped: bind mounts need root");
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let mounted_media = mount(&data.join("media"), &media);

        let result = mounted_media.then(|| check_links(&downloads, &media, Vec::new()));
        let left = files_under(&data);

        for target in [&media, &downloads] {
            let _ = std::process::Command::new("umount").arg(target).status();
        }
        let _ = fs::remove_dir_all(&root);
        let Some((check, lines)) = result else {
            eprintln!("skipped: the second bind mount failed");
            return;
        };
        assert!(matches!(check, LinkCheck::Refused { .. }), "{check:?}");
        assert!(lines[0].contains("No test hardlink made"), "{lines:?}");
        assert_eq!(left.len(), 1, "no test file in the library: {left:?}");
    }
}
