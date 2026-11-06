mod actions;
mod app;
mod bignum;
mod events;
mod layout;
mod model;
mod preview;
mod theme;
mod view;
mod wordmark;

use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Write};

use crate::config::Config;

/// Switch to the alternate screen. Mouse reporting stays off: the TUI handles
/// no mouse events, and capturing them would stop the user selecting text.
fn enter_screen<W: Write>(out: &mut W) -> io::Result<()> {
    execute!(out, EnterAlternateScreen)
}

fn leave_screen<W: Write>(out: &mut W) -> io::Result<()> {
    execute!(out, LeaveAlternateScreen)
}

#[derive(Default)]
struct TerminalGuard {
    raw_enabled: bool,
    alt_enabled: bool,
}

impl TerminalGuard {
    fn restore(&mut self) -> Result<()> {
        if self.alt_enabled {
            leave_screen(&mut io::stdout())?;
            self.alt_enabled = false;
        }
        if self.raw_enabled {
            disable_raw_mode()?;
            self.raw_enabled = false;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn run(config: &Config, strict: bool) -> Result<()> {
    let mut guard = TerminalGuard::default();
    enable_raw_mode()?;
    guard.raw_enabled = true;

    let mut stdout = io::stdout();
    enter_screen(&mut stdout)?;
    guard.alt_enabled = true;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = app::App::new(config, strict);
    let run_result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| view::render(f, &app))?;
            app.step()?;
            if app.should_quit() {
                break Ok(());
            }
        }
    })();

    terminal.show_cursor()?;
    guard.restore()?;

    run_result
}

pub fn run_logo_preview() -> Result<()> {
    let mut guard = TerminalGuard::default();
    enable_raw_mode()?;
    guard.raw_enabled = true;

    let mut stdout = io::stdout();
    enter_screen(&mut stdout)?;
    guard.alt_enabled = true;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let run_result = preview::run(&mut terminal);

    terminal.show_cursor()?;
    guard.restore()?;

    run_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entering_the_screen_leaves_mouse_reporting_off() {
        let mut out = Vec::new();
        enter_screen(&mut out).expect("enter screen");
        leave_screen(&mut out).expect("leave screen");
        let out = String::from_utf8(out).expect("utf8");
        assert!(out.contains("\x1b[?1049h"), "alternate screen on: {out:?}");
        for mouse_mode in ["?1000h", "?1002h", "?1003h", "?1006h"] {
            assert!(
                !out.contains(mouse_mode),
                "mouse mode {mouse_mode} sent: {out:?}"
            );
        }
    }

    #[test]
    fn terminal_guard_restore_noop_is_ok() {
        let mut guard = TerminalGuard::default();
        guard.restore().expect("restore no-op should succeed");
    }
}
