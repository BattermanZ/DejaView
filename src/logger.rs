use anyhow::{Result, anyhow};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct LoggerState {
    file: File,
    run_id: String,
}

pub struct Session;

static LOGGER: OnceLock<Mutex<Option<LoggerState>>> = OnceLock::new();

fn logger_slot() -> &'static Mutex<Option<LoggerState>> {
    LOGGER.get_or_init(|| Mutex::new(None))
}

fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn default_log_path() -> Result<PathBuf> {
    let base = dirs::state_dir()
        .or_else(dirs::config_dir)
        .ok_or_else(|| anyhow!("Could not find state or config directory"))?;
    Ok(base.join("dejaview").join("dejaview.log"))
}

fn init_at_path(path: &PathBuf, command: &str) -> Result<Session> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("log path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let run_id = format!("{}-{}", std::process::id(), timestamp_nanos());
    writeln!(
        file,
        "ts={} level=INFO run={} op={} msg=\"run started\"",
        timestamp_secs(),
        run_id,
        command
    )?;
    file.flush()?;

    if let Ok(mut slot) = logger_slot().lock() {
        *slot = Some(LoggerState { file, run_id });
    }
    Ok(Session)
}

pub fn init(command: &str) -> Result<(Session, PathBuf)> {
    let path = default_log_path()?;
    let session = init_at_path(&path, command)?;
    Ok((session, path))
}

fn write_line(level: &str, op: &str, message: &str) {
    let escaped = message.replace('"', "'");
    if let Ok(mut slot) = logger_slot().lock()
        && let Some(state) = slot.as_mut()
    {
        let _ = writeln!(
            state.file,
            "ts={} level={} run={} op={} msg=\"{}\"",
            timestamp_secs(),
            level,
            state.run_id,
            op,
            escaped
        );
        let _ = state.file.flush();
    }
}

pub fn info(op: &str, message: impl AsRef<str>) {
    write_line("INFO", op, message.as_ref());
}

pub fn warn(op: &str, message: impl AsRef<str>) {
    write_line("WARN", op, message.as_ref());
}

pub fn error(op: &str, message: impl AsRef<str>) {
    write_line("ERROR", op, message.as_ref());
}

pub fn shutdown() {
    if let Ok(mut slot) = logger_slot().lock() {
        if let Some(state) = slot.as_mut() {
            let _ = writeln!(
                state.file,
                "ts={} level=INFO run={} op=shutdown msg=\"run finished\"",
                timestamp_secs(),
                state.run_id
            );
            let _ = state.file.flush();
        }
        *slot = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        shutdown();
    }
}

#[cfg(test)]
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_test_log_persists_engine_message() {
        let root = std::env::temp_dir().join(format!(
            "dejaview_logger_test_{}_{}",
            std::process::id(),
            timestamp_nanos()
        ));
        let path = root.join("dejaview.log");
        let contents =
            write_test_log(&path, "link", "temp cleanup attempted").expect("write test log");
        assert!(contents.contains("op=link"));
        assert!(contents.contains("temp cleanup attempted"));
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }

    #[test]
    fn session_drop_writes_shutdown_marker() {
        let root = std::env::temp_dir().join(format!(
            "dejaview_logger_drop_{}_{}",
            std::process::id(),
            timestamp_nanos()
        ));
        let path = root.join("dejaview.log");
        {
            let _session = init_at_path(&path, "scan").expect("init test logger");
            info("scan", "doing work");
        }
        let contents = fs::read_to_string(&path).expect("read log");
        assert!(contents.contains("msg=\"run finished\""));
        fs::remove_dir_all(&root).expect("cleanup temp dir");
    }
}

#[cfg(test)]
pub fn write_test_log(path: &Path, command: &str, message: &str) -> Result<String> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("log path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let run_id = "test-run".to_string();
    if let Ok(mut slot) = logger_slot().lock() {
        *slot = Some(LoggerState { file, run_id });
    }
    info(command, message);
    shutdown();
    Ok(fs::read_to_string(path)?)
}
