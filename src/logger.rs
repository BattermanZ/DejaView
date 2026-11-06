use anyhow::{Result, anyhow};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct LoggerState {
    sink: Box<dyn Write + Send>,
    run_id: String,
}

pub struct Session;

static LOGGER: OnceLock<Mutex<Option<LoggerState>>> = OnceLock::new();

fn logger_slot() -> &'static Mutex<Option<LoggerState>> {
    LOGGER.get_or_init(|| Mutex::new(None))
}

/// Now, in UTC to the millisecond: `2026-11-06T03:17:00.123Z`.
fn timestamp() -> String {
    crate::human::rfc3339_utc_millis(SystemTime::now())
}

/// A message as `msg="..."` holds it: backslashes, quotes and control
/// characters escaped, so every entry is one line whatever it quotes, such
/// as an OS error or a file name with a line break in it.
fn escape_message(message: &str) -> String {
    let mut escaped = String::with_capacity(message.len());
    for ch in message.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{{{:x}}}", u32::from(ch))),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Where the log goes: `DEJAVIEW_LOG` when set (`-` for stderr), else
/// `dejaview/dejaview.log` under the platform's state folder.
enum LogTarget {
    File(PathBuf),
    Stderr,
}

fn log_target() -> Result<LogTarget> {
    if let Some(value) = std::env::var_os("DEJAVIEW_LOG").filter(|value| !value.is_empty()) {
        return Ok(if value == "-" {
            LogTarget::Stderr
        } else {
            LogTarget::File(PathBuf::from(value))
        });
    }
    let base = dirs::state_dir().or_else(dirs::config_dir).ok_or_else(|| {
        anyhow!("cannot find a state or config folder (neither XDG_STATE_HOME nor HOME is set)")
    })?;
    Ok(LogTarget::File(base.join("dejaview").join("dejaview.log")))
}

/// Past this size the log moves to `dejaview.log.1` and a new one starts, so the
/// log never takes more than about twice this.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Move a log that has reached `max_bytes` to `<path>.1`, replacing the previous
/// rotation. A missing log is not an error.
fn rotate_if_large(path: &Path, max_bytes: u64) -> Result<()> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() >= max_bytes => {
            let mut rotated = path.as_os_str().to_owned();
            rotated.push(".1");
            fs::rename(path, PathBuf::from(rotated))?;
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn open_log_file(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("log path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    // Best effort: a log that cannot rotate must not stop the command.
    let _ = rotate_if_large(path, MAX_LOG_BYTES);
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

fn install(mut sink: Box<dyn Write + Send>, command: &str) -> Result<Session> {
    let run_id = format!("{}-{}", std::process::id(), timestamp_nanos());
    writeln!(
        sink,
        "ts={} level=INFO run={} op={} msg=\"run started\"",
        timestamp(),
        run_id,
        command
    )?;
    sink.flush()?;

    if let Ok(mut slot) = logger_slot().lock() {
        *slot = Some(LoggerState { sink, run_id });
    }
    Ok(Session)
}

#[cfg(test)]
fn init_at_path(path: &Path, command: &str) -> Result<Session> {
    install(Box::new(open_log_file(path)?), command)
}

/// Start the log for this command. A log that cannot be opened never stops the
/// command: the second value is then a warning for the person, naming the path
/// and the way out, and the command runs without a log. Otherwise it says where
/// the log goes.
pub fn init(command: &str) -> (Session, Result<String, String>) {
    let started = log_target()
        .map_err(|err| format!("{err:#}"))
        .and_then(|target| match target {
            LogTarget::Stderr => install(Box::new(std::io::stderr()), command)
                .map(|session| (session, "stderr".to_string()))
                .map_err(|err| format!("cannot log to stderr ({err:#})")),
            LogTarget::File(path) => open_log_file(&path)
                .and_then(|file| install(Box::new(file), command))
                .map(|session| (session, path.display().to_string()))
                .map_err(|err| format!("cannot write the log at {} ({err:#})", path.display())),
        });
    match started {
        Ok((session, place)) => (session, Ok(place)),
        Err(problem) => (
            Session,
            Err(format!(
                "{problem}; running without a log. Set DEJAVIEW_LOG to a writable file, or to - for stderr"
            )),
        ),
    }
}

fn write_line(level: &str, op: &str, message: &str) {
    let escaped = escape_message(message);
    if let Ok(mut slot) = logger_slot().lock()
        && let Some(state) = slot.as_mut()
    {
        // One write per line: the file is opened for appending, so a line
        // lands whole, and a log on a network drive costs one round trip.
        let line = format!(
            "ts={} level={} run={} op={} msg=\"{}\"\n",
            timestamp(),
            level,
            state.run_id,
            op,
            escaped
        );
        let _ = state.sink.write_all(line.as_bytes());
        let _ = state.sink.flush();
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
                state.sink,
                "ts={} level=INFO run={} op=shutdown msg=\"run finished\"",
                timestamp(),
                state.run_id
            );
            let _ = state.sink.flush();
        }
        *slot = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        shutdown();
    }
}

/// Tests that install the global logger hold this, so two of them running at
/// once cannot write into each other's files.
#[cfg(test)]
static TEST_LOGGER_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
fn lock_test_logger() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOGGER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
pub fn write_test_log(path: &Path, command: &str, message: &str) -> Result<String> {
    let _guard = lock_test_logger();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("log path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let run_id = "test-run".to_string();
    if let Ok(mut slot) = logger_slot().lock() {
        *slot = Some(LoggerState {
            sink: Box::new(file),
            run_id,
        });
    }
    info(command, message);
    shutdown();
    Ok(fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_common::TempDir;

    /// One line per entry, stamped in UTC: a message with a line break, a
    /// quote or a backslash in it, such as an OS error or a file name, is
    /// written escaped, so it cannot split its entry or end its `msg`.
    #[test]
    fn a_line_is_stamped_in_utc_and_its_message_stays_on_it() {
        let root = TempDir::new("logger_escape");
        let path = root.join("dejaview.log");
        let contents = write_test_log(
            &path,
            "escape_check",
            "first\nsecond \"quoted\" back\\slash\ttab\u{7}",
        )
        .expect("write test log");
        // Found by its op: other tests running at once log through the same
        // global logger, so their lines may land in this file too.
        let line = contents
            .lines()
            .find(|line| line.contains(" op=escape_check "))
            .unwrap_or_else(|| panic!("the entry: {contents}"));
        assert!(
            line.ends_with(r#"msg="first\nsecond \"quoted\" back\\slash\ttab\u{7}""#),
            "{line}"
        );
        let ts = line
            .strip_prefix("ts=")
            .and_then(|rest| rest.split(' ').next())
            .expect("a timestamp");
        let shape = ts
            .bytes()
            .map(|byte| if byte.is_ascii_digit() { b'0' } else { byte })
            .collect::<Vec<_>>();
        assert_eq!(shape, b"0000-00-00T00:00:00.000Z", "{ts}");
    }

    #[test]
    fn write_test_log_persists_engine_message() {
        let root = TempDir::new("logger_test");
        let path = root.join("dejaview.log");
        let contents =
            write_test_log(&path, "link", "temp cleanup attempted").expect("write test log");
        assert!(contents.contains("op=link"));
        assert!(contents.contains("temp cleanup attempted"));
    }

    #[test]
    fn log_past_the_limit_moves_to_dot_one() {
        let root = TempDir::new("logger_rotate");
        let path = root.join("dejaview.log");
        let rotated = root.join("dejaview.log.1");
        fs::write(&rotated, b"oldest").expect("write previous rotation");
        fs::write(&path, vec![b'x'; 100]).expect("write full log");

        rotate_if_large(&path, 100).expect("rotate");

        assert!(!path.exists(), "the full log moves out of the way");
        assert_eq!(fs::read(&rotated).expect("read rotated").len(), 100);
    }

    #[test]
    fn log_under_the_limit_stays() {
        let root = TempDir::new("logger_keep");
        let path = root.join("dejaview.log");
        fs::write(&path, vec![b'x'; 99]).expect("write log");

        rotate_if_large(&path, 100).expect("rotate");

        assert_eq!(fs::read(&path).expect("read log").len(), 99);
        assert!(!root.join("dejaview.log.1").exists());
    }

    #[test]
    fn session_drop_writes_shutdown_marker() {
        let root = TempDir::new("logger_drop");
        let path = root.join("dejaview.log");
        let _guard = lock_test_logger();
        {
            let _session = init_at_path(&path, "scan").expect("init test logger");
            info("scan", "doing work");
        }
        let contents = fs::read_to_string(&path).expect("read log");
        assert!(contents.contains("msg=\"run finished\""));
    }
}
