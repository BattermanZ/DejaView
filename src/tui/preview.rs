//! `dejaview tui-logo-preview`: the dashboard's logo on its own, as the
//! header draws it, to judge in a real terminal and font.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::theme::Theme;
use super::wordmark;

type PreviewTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub fn run(terminal: &mut PreviewTerminal) -> Result<()> {
    loop {
        terminal.draw(render)?;
        if event::poll(Duration::from_millis(150))?
            && let CEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(());
        }
    }
}

fn render(frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::canvas()), area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(wordmark::HEIGHT),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(wordmark::lines())
            .style(Theme::canvas())
            .alignment(Alignment::Center),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("q quits", Theme::dim())))
            .style(Theme::canvas())
            .alignment(Alignment::Center),
        rows[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_shows_the_logo() {
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(render).expect("draw");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains('▀') && text.contains('█'), "{text}");
        assert!(text.contains("q quits"), "{text}");
    }
}
