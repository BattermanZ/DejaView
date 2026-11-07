//! Ends the dashboard once its terminal is gone.
//!
//! A terminal that closes sends SIGHUP, which ends the dashboard. When that
//! signal never arrives (the shell was killed along with the window, or
//! SIGHUP is ignored, as under `nohup`), the dashboard used to spin a core
//! for as long as nobody killed it. crossterm 0.29's event reader retries a
//! read from a dead terminal for ever, so `event::poll` never returns to the
//! dashboard's loop: <https://github.com/crossterm-rs/crossterm/issues/793>.
//! The check therefore runs on a thread of its own. Once a crossterm release
//! reports the dead terminal as an error, the thread can go; the check after
//! an error stays, for a draw that fails on the dead terminal.

use std::fs::File;
use std::io::{self, IsTerminal};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::logger;

use super::live_runs;

const CHECK_EVERY: Duration = Duration::from_secs(1);

/// How long a scan or link gets to stop before the dashboard exits without
/// it. A run stops before its next file, or between chunks of a large one,
/// so only a read stuck on a mount that stopped answering takes this long.
const RUN_STOP_LIMIT: Duration = Duration::from_secs(30);

/// The exit status of a process ended by SIGHUP.
const HUNG_UP: i32 = 129;

/// What the watch looks at: the controlling terminal, which crossterm reads
/// keys from when stdin is not a terminal, or else stdout.
enum Watched {
    ControllingTerminal(File),
    Stdout,
}

static WATCHED: OnceLock<Option<Watched>> = OnceLock::new();

impl Watched {
    fn find() -> Option<Self> {
        match File::open("/dev/tty") {
            Ok(tty) if tty.is_terminal() => Some(Watched::ControllingTerminal(tty)),
            _ if io::stdout().is_terminal() => Some(Watched::Stdout),
            _ => None,
        }
    }

    /// A hung-up terminal answers every terminal query with an error, so it
    /// no longer counts as one.
    fn is_gone(&self) -> bool {
        match self {
            Watched::ControllingTerminal(tty) => !tty.is_terminal(),
            Watched::Stdout => !io::stdout().is_terminal(),
        }
    }
}

/// Watch the dashboard's terminal on a thread of its own. Nothing to watch
/// when the dashboard has no terminal to begin with.
pub(super) fn start() {
    if WATCHED.get_or_init(Watched::find).is_none() {
        return;
    }
    thread::spawn(|| {
        loop {
            thread::sleep(CHECK_EVERY);
            exit_if_gone();
        }
    });
}

/// When the terminal is gone, stop what the dashboard started and exit,
/// whether the dashboard's own loop failed on the dead terminal or is stuck
/// in it. Returns while the terminal is still there.
pub(super) fn exit_if_gone() {
    if let Some(Some(watched)) = WATCHED.get()
        && watched.is_gone()
    {
        terminal_gone();
    }
}

/// Set by the first thread to find the terminal gone: the watch and a
/// failed draw can find it at once, and one of them does the stopping.
static EXITING: AtomicBool = AtomicBool::new(false);

/// The screen is not restored: there is no terminal left to restore it on.
fn terminal_gone() -> ! {
    if EXITING.swap(true, Ordering::SeqCst) {
        loop {
            thread::park();
        }
    }
    logger::warn(
        "tui",
        "the terminal is gone; stopping any run the dashboard started, then exiting",
    );
    if !live_runs::ALL.stop_all(RUN_STOP_LIMIT) {
        logger::warn(
            "tui",
            format!(
                "a run the dashboard started has not stopped after {} s, likely stuck on a read from a mount that stopped answering; exiting without it. It was asked to stop, and does once that read returns: `ps -C dejaview` shows whether it is still running",
                RUN_STOP_LIMIT.as_secs()
            ),
        );
    }
    logger::shutdown();
    std::process::exit(HUNG_UP);
}
