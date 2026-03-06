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
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;

use crate::config::Config;

pub fn run(config: &Config, strict: bool) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    run_result
}
