//! Cooperative cancellation for a run. The TUI runs scan and link in a child
//! process and asks it to stop by writing `cancel` to the child's stdin; the
//! engine checks the token between files and between read chunks.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(super) fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Cancel when `input` delivers a `cancel` line. End of input is not a
    /// cancel, so a run whose stdin is closed or /dev/null runs to completion.
    pub fn watch<R: Read + Send + 'static>(&self, input: R) -> JoinHandle<()> {
        let token = self.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(input).lines() {
                match line {
                    Ok(line) if line.trim() == "cancel" => {
                        token.cancel();
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        })
    }
}

/// The first error that stops a phase many workers run at once (a strict
/// failure, another root reached, a cancel), shared so the others stop
/// before their next step.
#[derive(Default)]
pub(super) struct FirstError {
    stopped: AtomicBool,
    error: Mutex<Option<anyhow::Error>>,
}

impl FirstError {
    pub(super) fn is_set(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// Keep `err` unless an earlier error is already kept.
    pub(super) fn set(&self, err: anyhow::Error) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Ok(mut slot) = self.error.lock()
            && slot.is_none()
        {
            *slot = Some(err);
        }
    }

    /// Whether a worker should stop before its next step: an error is kept,
    /// or the run was cancelled, which is then kept as the error.
    pub(super) fn should_stop(&self, cancel: &CancelToken) -> bool {
        if self.is_set() {
            return true;
        }
        if let Err(cancelled) = cancel.check() {
            self.set(cancelled.into());
            return true;
        }
        false
    }

    pub(super) fn into_result(self) -> anyhow::Result<()> {
        match self.error.into_inner() {
            Ok(Some(err)) => Err(err),
            _ => Ok(()),
        }
    }
}

/// The error a run returns when it stops because it was cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled by user")
    }
}

impl std::error::Error for Cancelled {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn watched(input: &'static str) -> CancelToken {
        let token = CancelToken::default();
        token
            .watch(Cursor::new(input.as_bytes()))
            .join()
            .expect("watcher thread");
        token
    }

    #[test]
    fn cancel_line_cancels() {
        assert!(watched("cancel\n").is_cancelled());
    }

    #[test]
    fn end_of_input_does_not_cancel() {
        assert!(!watched("").is_cancelled());
    }

    #[test]
    fn other_lines_do_not_cancel() {
        assert!(!watched("hello\ncancelled\n").is_cancelled());
    }

    #[test]
    fn check_reports_cancellation() {
        let token = CancelToken::default();
        assert!(token.check().is_ok());
        token.cancel();
        assert!(token.check().is_err());
    }
}
