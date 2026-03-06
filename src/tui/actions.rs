use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};

use super::model::RunReport;

fn run_subcommand_json(subcommand: &str, strict: bool) -> Result<RunReport> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg(subcommand).arg("--json").arg("--no-progress");
    if strict {
        cmd.arg("--strict");
    }

    let output = cmd
        .output()
        .with_context(|| format!("execute '{subcommand}' in subprocess"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            format!("{subcommand} failed with status {}", output.status)
        } else {
            format!("{subcommand} failed: {stderr}")
        };
        return Err(anyhow!(message));
    }

    let stdout = String::from_utf8(output.stdout).context("decode json stdout")?;
    let report: RunReport = serde_json::from_str(&stdout).with_context(|| {
        format!(
            "parse {subcommand} json output (first 200 bytes: {})",
            stdout.chars().take(200).collect::<String>()
        )
    })?;
    Ok(report)
}

pub fn run_scan(strict: bool) -> Result<RunReport> {
    run_subcommand_json("scan", strict)
}

pub fn run_link(strict: bool) -> Result<RunReport> {
    run_subcommand_json("link", strict)
}

pub fn save_report(report: &RunReport, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(report)?;
    std::fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
