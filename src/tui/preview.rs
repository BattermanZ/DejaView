use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::theme::Theme;
use super::wordmark;

type PreviewTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LogoOption {
    FilledBlock,
    Outline,
    Hybrid,
}

impl LogoOption {
    fn label(self) -> &'static str {
        match self {
            Self::FilledBlock => "1. Filled Block",
            Self::Outline => "2. Technical Outline",
            Self::Hybrid => "3. Hybrid",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            Self::FilledBlock => "Boldest silhouette. Highest visual mass.",
            Self::Outline => "Sharper and cleaner. Safer for tight terminals.",
            Self::Hybrid => "Best balance of depth, clarity, and terminal fit.",
        }
    }
}

#[derive(Debug)]
struct PreviewApp {
    selected: LogoOption,
    should_quit: bool,
}

impl Default for PreviewApp {
    fn default() -> Self {
        Self {
            selected: LogoOption::Hybrid,
            should_quit: false,
        }
    }
}

fn filled_logo_lines() -> Vec<Line<'static>> {
    wordmark::filled_block_lines()
}

fn outline_logo_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "┌──┐┌─┐ ┌─┐┌┐┌┌┬┐┌─┐┬ ┬┬┌─┐┬ ┬",
            Theme::wordmark(),
        )),
        Line::from(Span::styled(
            "│  │├┤  │ ││││ │ ├─┤││││├┤ │││",
            Theme::wordmark_highlight(),
        )),
        Line::from(Span::styled(
            "└──┘└─┘ └─┘┘└┘ ┴ ┴ ┴└┴┘┴└─┘└┴┘",
            Theme::wordmark(),
        )),
        Line::from(Span::styled(
            "╶┄┄┄ technical linework / lower visual weight ╴",
            Theme::dim(),
        )),
    ]
}

fn hybrid_logo_lines() -> Vec<Line<'static>> {
    let rows = wordmark::filled_block_rows();
    let mut lines = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let style = if index == 1 || index == 2 {
            Theme::wordmark_highlight()
        } else {
            Theme::wordmark()
        };
        lines.push(Line::from(Span::styled(row.clone(), style)));
    }
    lines
}

fn logo_lines(option: LogoOption) -> Vec<Line<'static>> {
    match option {
        LogoOption::FilledBlock => filled_logo_lines(),
        LogoOption::Outline => outline_logo_lines(),
        LogoOption::Hybrid => hybrid_logo_lines(),
    }
}

pub fn run(terminal: &mut PreviewTerminal) -> Result<()> {
    let mut app = PreviewApp::default();
    while !app.should_quit {
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(Duration::from_millis(150))?
            && let CEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                KeyCode::Char('1') => app.selected = LogoOption::FilledBlock,
                KeyCode::Char('2') => app.selected = LogoOption::Outline,
                KeyCode::Char('3') => app.selected = LogoOption::Hybrid,
                _ => {}
            }
        }
    }
    Ok(())
}

fn render(frame: &mut Frame, app: &PreviewApp) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::canvas()), area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(20),
            Constraint::Length(2),
        ])
        .split(area);

    let header = Paragraph::new(vec![
        Line::from(Span::styled("DEJAVIEW Logo Preview", Theme::wordmark())),
        Line::from(Span::styled(
            "Options 1 and 3 use a shared fixed-grid alphabet. Press 1, 2, or 3 to choose. q exits.",
            Theme::muted(),
        )),
    ]);
    frame.render_widget(header, vertical[0]);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(9)])
        .split(vertical[1]);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    render_option(
        frame,
        top[0],
        LogoOption::FilledBlock,
        app.selected == LogoOption::FilledBlock,
    );
    render_option(
        frame,
        top[1],
        LogoOption::Hybrid,
        app.selected == LogoOption::Hybrid,
    );
    render_option(
        frame,
        rows[1],
        LogoOption::Outline,
        app.selected == LogoOption::Outline,
    );

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" Selected ", Theme::badge_accent()),
        Span::raw(" "),
        Span::styled(app.selected.label(), Theme::accent()),
        Span::raw("  "),
        Span::styled(app.selected.caption(), Theme::muted()),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Theme::line())
            .style(Theme::canvas()),
    );
    frame.render_widget(footer, vertical[2]);
}

fn render_option(frame: &mut Frame, area: Rect, option: LogoOption, selected: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(Span::styled(
            format!(" {} ", option.label()),
            if selected {
                Theme::title(true)
            } else {
                Theme::title(false)
            },
        )))
        .border_style(if selected {
            Theme::accent()
        } else {
            Theme::line()
        })
        .style(Theme::panel());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let logo_height = match option {
        LogoOption::Hybrid => 11,
        LogoOption::FilledBlock => 7,
        LogoOption::Outline => 4,
    };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(logo_height),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(inner);

    let logo = Paragraph::new(logo_lines(option))
        .style(Theme::panel())
        .wrap(Wrap { trim: false });
    frame.render_widget(logo, sections[0]);

    let note = Paragraph::new(vec![
        Line::from(Span::styled(option.caption(), Theme::muted())),
        Line::from(Span::styled(
            if selected {
                "Current favorite"
            } else {
                "Preview only"
            },
            if selected {
                Theme::accent_soft()
            } else {
                Theme::dim()
            },
        )),
    ])
    .style(Theme::panel())
    .wrap(Wrap { trim: true });
    frame.render_widget(note, sections[1]);

    let usage = Paragraph::new(Line::from(Span::styled(
        "How it would sit in the header",
        Theme::dim(),
    )))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Theme::line())
            .style(Theme::panel()),
    );
    frame.render_widget(usage, sections[2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_preview_has_three_distinct_options() {
        let labels = [
            LogoOption::FilledBlock,
            LogoOption::Outline,
            LogoOption::Hybrid,
        ]
        .map(LogoOption::label);
        assert_eq!(labels.len(), 3);
        assert_ne!(labels[0], labels[1]);
        assert_ne!(labels[1], labels[2]);
    }

    #[test]
    fn shared_glyphs_compose_the_full_name() {
        let rows = wordmark::filled_block_rows();
        assert_eq!(rows.len(), 5);
        assert!(rows[0].contains("███"));
        assert!(rows[0].chars().count() > 32);
    }

    #[test]
    fn logos_render_distinct_content() {
        let filled = logo_lines(LogoOption::FilledBlock)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let outline = logo_lines(LogoOption::Outline)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let hybrid = logo_lines(LogoOption::Hybrid)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(filled.contains("███"));
        assert!(outline.contains("┌──┐"));
        assert!(hybrid.contains("███"));
        assert_ne!(filled, outline);
        assert_ne!(outline, hybrid);
    }
}
