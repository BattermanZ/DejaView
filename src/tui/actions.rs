use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result, anyhow};

use crate::config::load_config;
use crate::engine;
use crate::fsutil::safe_write_bytes;

use super::model::{RunReport, TuiEvent};
use crate::protocol::PlannedPair;

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

fn stream_subcommand_events(
    subcommand: &str,
    strict: bool,
    extra_args: &[String],
    tx: Sender<Result<TuiEvent>>,
    cancel_rx: Receiver<()>,
) -> Result<()> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg(subcommand)
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
        &[String::from("--plan"), plan.display().to_string()],
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
        assert_eq!(report.mode, "scan");
        assert_eq!(report.files_considered, 10);
        assert_eq!(report.warning_total(), 0);
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
}
