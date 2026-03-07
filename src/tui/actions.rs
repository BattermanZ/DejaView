use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result, anyhow};

use crate::fsutil::safe_write_bytes;

use super::model::{RunReport, TuiEvent};

fn should_forward_terminal_error(status_success: bool, saw_error_event: bool) -> bool {
    !status_success && !saw_error_event
}

fn stream_subcommand_events(
    subcommand: &str,
    strict: bool,
    tx: Sender<Result<TuiEvent>>,
) -> Result<()> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg(subcommand)
        .arg("--tui-events")
        .arg("--no-progress")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if strict {
        cmd.arg("--strict");
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("execute '{subcommand}' in subprocess"))?;
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
        if matches!(event, TuiEvent::Error { .. }) {
            saw_error_event = true;
        }
        let _ = tx.send(Ok(event));
    }

    let status = child.wait().context("wait for subprocess completion")?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| anyhow!("stderr reader thread panicked"))??;

    if !status.success() {
        let stderr = stderr.trim().to_string();
        let message = if stderr.is_empty() {
            format!("{subcommand} failed with status {status}")
        } else {
            format!("{subcommand} failed: {stderr}")
        };
        if should_forward_terminal_error(status.success(), saw_error_event) {
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

pub fn run_scan(strict: bool, tx: Sender<Result<TuiEvent>>) -> Result<()> {
    stream_subcommand_events("scan", strict, tx)
}

pub fn run_link(strict: bool, tx: Sender<Result<TuiEvent>>) -> Result<()> {
    stream_subcommand_events("link", strict, tx)
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
            assert!(err.to_string().contains("refusing to overwrite symlink target"));
            assert_eq!(fs::read(&victim).expect("read victim"), b"keep");

            fs::remove_dir_all(&root).expect("cleanup temp dir");
        }
    }
}
