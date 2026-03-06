use ratatui::style::{Color, Modifier, Style};

pub struct Theme;

impl Theme {
    pub fn base() -> Style {
        Style::default().fg(Color::White)
    }

    pub fn accent() -> Style {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }

    pub fn warning() -> Style {
        Style::default().fg(Color::Yellow)
    }

    pub fn danger() -> Style {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::default().fg(Color::DarkGray)
    }
}
