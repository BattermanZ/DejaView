//! The scans and links this dashboard has started and not yet seen end, so
//! that the terminal watch can stop them from its own thread when the
//! dashboard's thread is stuck where it cannot act.

use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Every run this dashboard process started.
pub(super) static ALL: LiveRuns = LiveRuns::new();

pub(super) struct LiveRuns(Mutex<Listed>);

struct Listed {
    next_id: u64,
    /// Each run with the sender that asks it to stop, as its cancel key does.
    runs: Vec<(u64, Sender<()>)>,
    /// Set once the runs were asked to stop: a run listed later is asked at
    /// once, so one that was starting at that moment is not missed.
    stopping: bool,
}

/// A run listed in its [`LiveRuns`] until this is dropped.
pub(super) struct LiveRun<'a> {
    runs: &'a LiveRuns,
    id: u64,
}

impl LiveRuns {
    pub(super) const fn new() -> Self {
        LiveRuns(Mutex::new(Listed {
            next_id: 0,
            runs: Vec::new(),
            stopping: false,
        }))
    }

    fn listed(&self) -> MutexGuard<'_, Listed> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// List a run until the returned guard is dropped, once the run has
    /// ended and cleaned up after itself.
    pub(super) fn register(&self, stop: Sender<()>) -> LiveRun<'_> {
        let mut listed = self.listed();
        if listed.stopping {
            let _ = stop.send(());
        }
        let id = listed.next_id;
        listed.next_id += 1;
        listed.runs.push((id, stop));
        LiveRun { runs: self, id }
    }

    /// Ask every run to stop and wait up to `limit` for all of them to end.
    /// True when none is left running.
    pub(super) fn stop_all(&self, limit: Duration) -> bool {
        {
            let mut listed = self.listed();
            listed.stopping = true;
            for (_, stop) in &listed.runs {
                let _ = stop.send(());
            }
        }
        let end = Instant::now() + limit;
        loop {
            if self.listed().runs.is_empty() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for LiveRun<'_> {
    fn drop(&mut self) {
        self.runs.listed().runs.retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn stopping_asks_each_run_and_waits_for_it_to_end() {
        static RUNS: LiveRuns = LiveRuns::new();
        let (stop, asked) = mpsc::channel();
        let live = RUNS.register(stop);
        let run = std::thread::spawn(move || {
            asked.recv().expect("asked to stop");
            drop(live);
        });
        assert!(RUNS.stop_all(Duration::from_secs(5)));
        run.join().expect("run thread");
    }

    #[test]
    fn stopping_gives_up_on_a_run_that_does_not_end() {
        let runs = LiveRuns::new();
        let (stop, _asked) = mpsc::channel();
        let _live = runs.register(stop);
        assert!(!runs.stop_all(Duration::from_millis(100)));
    }

    #[test]
    fn a_run_listed_after_the_stop_is_asked_at_once() {
        let runs = LiveRuns::new();
        assert!(runs.stop_all(Duration::from_millis(10)));
        let (stop, asked) = mpsc::channel();
        let _live = runs.register(stop);
        assert!(asked.try_recv().is_ok());
    }
}
