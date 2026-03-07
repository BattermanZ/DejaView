mod actions;
mod app;
mod events;
mod layout;
mod model;
mod theme;
mod view;

use anyhow::Result;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;

use crate::config::Config;

#[derive(Default)]
struct TerminalGuard {
    raw_enabled: bool,
    alt_enabled: bool,
}

impl TerminalGuard {
    fn restore(&mut self) -> Result<()> {
        if self.alt_enabled {
            let mut stdout = io::stdout();
            execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_guard_restore_noop_is_ok() {
        let mut guard = TerminalGuard::default();
        guard.restore().expect("restore no-op should succeed");
    }
}
