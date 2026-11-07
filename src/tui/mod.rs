mod actions;
mod app;
mod bignum;
mod events;
mod layout;
mod model;
mod preview;
mod run_state;
mod theme;
mod view;
mod wordmark;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Write};

use crate::config::Config;

/// Whether a screen takes the mouse. The dashboard does, for its clicks and
/// the wheel; a terminal then selects text with Shift held while dragging.
/// The logo preview handles no mouse event, so it leaves the mouse to the
/// terminal.
#[derive(Copy, Clone, Default)]
enum Mouse {
    Capture,
    #[default]
    Off,
}

fn enter_screen<W: Write>(out: &mut W, mouse: Mouse) -> io::Result<()> {
    execute!(out, EnterAlternateScreen)?;
    if let Mouse::Capture = mouse {
        execute!(out, EnableMouseCapture)?;
    }
    Ok(())
}

fn leave_screen<W: Write>(out: &mut W, mouse: Mouse) -> io::Result<()> {
    if let Mouse::Capture = mouse {
        execute!(out, DisableMouseCapture)?;
    }
    execute!(out, LeaveAlternateScreen)
}

#[derive(Default)]
struct TerminalGuard {
    raw_enabled: bool,
    alt_enabled: bool,
    mouse: Mouse,
}

impl TerminalGuard {
    fn restore(&mut self) -> Result<()> {
        if self.alt_enabled {
            leave_screen(&mut io::stdout(), self.mouse)?;
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
    guard.mouse = Mouse::Capture;
    // Set first, so a screen entered partway is still left on an error.
    guard.alt_enabled = true;
    enter_screen(&mut stdout, guard.mouse)?;
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
    guard.alt_enabled = true;
    enter_screen(&mut stdout, Mouse::Off)?;
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
    fn the_dashboard_takes_the_mouse_and_gives_it_back() {
        let mut out = Vec::new();
        enter_screen(&mut out, Mouse::Capture).expect("enter screen");
        let entered = String::from_utf8(out).expect("utf8");
        assert!(
            entered.contains("\x1b[?1049h"),
            "alternate screen on: {entered:?}"
        );
        for mouse_mode in ["?1000h", "?1006h"] {
            assert!(
                entered.contains(mouse_mode),
                "mouse mode {mouse_mode} not sent: {entered:?}"
            );
        }
        let mut out = Vec::new();
        leave_screen(&mut out, Mouse::Capture).expect("leave screen");
        let left = String::from_utf8(out).expect("utf8");
        for mouse_mode in ["?1000l", "?1006l"] {
            assert!(
                left.contains(mouse_mode),
                "mouse mode {mouse_mode} not turned off: {left:?}"
            );
        }
    }

    #[test]
    fn the_logo_preview_leaves_mouse_reporting_off() {
        let mut out = Vec::new();
        enter_screen(&mut out, Mouse::Off).expect("enter screen");
        leave_screen(&mut out, Mouse::Off).expect("leave screen");
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
