use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::tui::app::{App, ConfigEditor, ConfigField, FocusPane, Overlay};
use crate::tui::layout;
use crate::tui::model::LinkReportEntry;
use crate::tui::theme::Theme;

fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64;
    let mut idx = 0usize;
    while value >= 1024.0 && idx < units.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    format!("{value:.2} {}", units[idx])
}

fn row_style(row: &LinkReportEntry, is_selected: bool) -> Style {
    if is_selected {
        return Theme::accent();
    }
    if row.status.eq_ignore_ascii_case("failed") {
        return Theme::danger();
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
        || row.fallback_reason.is_some()
        || row.status.eq_ignore_ascii_case("fallback")
    {
        return Theme::warning();
    }
    Theme::base()
}

fn badge(label: &str, value: String, accent: bool) -> Span<'static> {
    let style = if accent {
        Theme::badge_accent()
    } else {
        Theme::badge()
    };
    Span::styled(format!(" {label}: {value} "), style)
}

fn context_text(app: &App) -> String {
    format!(
        "Paths\nDownloads\n  {}\nMedia\n  {}\n\nSettings\nMin age\n  {}s\nThreads\n  {}\n\nActions\n  e  Edit paths\n  S  Scan for duplicates\n  L  Link with confirmation\n  R  Save current report\n  J  Open JSON preview",
        app.downloads_label(),
        app.media_label(),
        app.min_age_secs(),
        app.threads_label(),
    )
}

fn empty_results_text() -> String {
    "No scan results yet.\n\nPress e to set Downloads and Media.\nThen press S to scan for duplicate groups.".to_string()
}

fn pane_block(title: &'static str, pane: FocusPane, app: &App, featured: bool) -> Block<'static> {
    let active = app.focus == pane;
    let surface = if featured {
        Theme::panel_feature()
    } else if active {
        Theme::panel_active()
    } else {
        Theme::panel()
    };

    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            Theme::title(active),
        )]))
        .style(surface)
        .border_style(if active {
            Theme::accent()
        } else {
            Theme::muted()
        })
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::app()), area);
    let l = layout::dashboard(area);

    let top = Paragraph::new(Line::from(vec![
        badge("Mode", app.mode_label().to_string(), true),
        Span::raw(" "),
        badge(
            "Strict",
            if app.strict { "On" } else { "Off" }.to_string(),
            false,
        ),
        Span::raw(" "),
        badge("Files", app.files_considered().to_string(), false),
        Span::raw(" "),
        badge("Planned", app.planned_actions().to_string(), false),
        Span::raw(" "),
        badge("Applied", app.applied_actions().to_string(), false),
        Span::raw(" "),
        badge("Saved", format_bytes(app.total_saved_bytes()), false),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .title(Line::from(vec![Span::styled(
                " Status ",
                Theme::title(true),
            )]))
            .style(Theme::app())
            .border_style(Theme::muted()),
    )
    .style(Theme::app());
    frame.render_widget(top, l.top);

    let left = Paragraph::new(context_text(app))
        .block(pane_block("Context", FocusPane::Left, app, false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(left, l.center_left);

    if app.rows().is_empty() {
        let empty = Paragraph::new(empty_results_text())
            .block(pane_block("Results", FocusPane::Center, app, true))
            .style(Theme::panel_feature())
            .wrap(Wrap { trim: true });
        frame.render_widget(empty, l.center_main);
    } else {
        let rows: Vec<Row> = app
            .rows()
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                let base_style = row_style(row, idx == app.selected_row());
                Row::new(vec![
                    Cell::from(row.status.clone()),
                    Cell::from(row.method.clone().unwrap_or_else(|| "-".to_string())),
                    Cell::from(trim_middle(&row.source, 38)),
                    Cell::from(trim_middle(&row.target, 38)),
                ])
                .style(base_style)
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Percentage(40),
                Constraint::Percentage(40),
            ],
        )
        .header(Row::new(vec!["Status", "Method", "Source", "Target"]).style(Theme::title(true)))
        .block(pane_block("Results", FocusPane::Center, app, true))
        .style(Theme::panel_feature());
        frame.render_widget(table, l.center_main);
    }

    let details = Paragraph::new(app.selected_details())
        .block(pane_block("Details", FocusPane::Right, app, false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(details, l.center_right);

    let log = Paragraph::new(app.log_lines().join("\n"))
        .block(pane_block("Run Log", FocusPane::Bottom, app, false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(log, l.bottom_log);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Theme::badge()),
        Span::styled("quit", Theme::muted()),
        Span::raw("  "),
        Span::styled(" e ", Theme::badge()),
        Span::styled("paths", Theme::muted()),
        Span::raw("  "),
        Span::styled(" S ", Theme::badge()),
        Span::styled("scan", Theme::muted()),
        Span::raw("  "),
        Span::styled(" L ", Theme::badge()),
        Span::styled("link", Theme::muted()),
        Span::raw("  "),
        Span::styled(" ? ", Theme::badge()),
        Span::styled("help", Theme::muted()),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Theme::muted()),
    )
    .style(Theme::app());
    frame.render_widget(footer, l.footer);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => render_help_overlay(frame),
        Overlay::JsonPreview(json) => render_json_overlay(frame, json),
        Overlay::ConfigEditor(editor) => render_config_overlay(frame, editor),
        Overlay::ConfirmLink => render_confirm_link_overlay(frame, app),
        Overlay::BlockingNotice(message) => render_blocking_notice_overlay(frame, message),
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);
    let help = Paragraph::new(
        "Keyboard map\n\nGlobal\nq close overlay or quit when idle\n? help\ntab next pane\nshift+tab previous pane\n\nActions\ne edit downloads/media paths\nS scan for duplicates\nL confirm and run link job\nR save report\nJ open JSON preview\n\nConfig editor\ntype directly into the active field\nenter moves forward or saves\ntab switches field\nEsc cancels"
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Line::from(vec![Span::styled(" Help ", Theme::title(true))]))
            .style(Theme::popup()),
    )
    .style(Theme::popup())
    .wrap(Wrap { trim: true });
    frame.render_widget(help, area);
}

fn render_json_overlay(frame: &mut Frame, json: &str) {
    let area = centered_rect(90, 80, frame.area());
    frame.render_widget(Clear, area);
    let body = Paragraph::new(json)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Report JSON")
                .style(Theme::popup()),
        )
        .style(Theme::popup())
        .wrap(Wrap { trim: false });
    frame.render_widget(body, area);
}

fn render_config_overlay(frame: &mut Frame, editor: &ConfigEditor) {
    let area = centered_rect(80, 55, frame.area());
    frame.render_widget(Clear, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(4),
        ])
        .margin(1)
        .split(area);

    let shell = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            " Config Editor ",
            Theme::title(true),
        )]))
        .style(Theme::popup());
    frame.render_widget(shell, area);

    let downloads = Paragraph::new(editor.downloads.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Downloads")
                .border_style(if editor.active_field == ConfigField::Downloads {
                    Theme::accent()
                } else {
                    Theme::muted()
                }),
        )
        .style(Theme::input(editor.active_field == ConfigField::Downloads))
        .wrap(Wrap { trim: false });
    frame.render_widget(downloads, sections[0]);

    let media = Paragraph::new(editor.media.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Media")
                .border_style(if editor.active_field == ConfigField::Media {
                    Theme::accent()
                } else {
                    Theme::muted()
                }),
        )
        .style(Theme::input(editor.active_field == ConfigField::Media))
        .wrap(Wrap { trim: false });
    frame.render_widget(media, sections[1]);

    let status = Paragraph::new(
        editor
            .error
            .clone()
            .unwrap_or_else(|| "Enter saves on the second field. Esc cancels.".to_string()),
    )
    .style(if editor.error.is_some() {
        Theme::danger()
    } else {
        Theme::muted()
    })
    .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(status, sections[2]);

    let tips = Paragraph::new(
        "Use absolute paths to existing directories. The editor validates before saving to the repo config file.",
    )
    .style(Theme::muted())
    .wrap(Wrap { trim: true });
    frame.render_widget(tips, sections[3]);
}

fn render_confirm_link_overlay(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 45, frame.area());
    frame.render_widget(Clear, area);
    let body = Paragraph::new(format!(
        "Confirm link run\n\nThis will execute filesystem changes for the current configuration.\n\nEstimated planned actions: {}\nCurrent mode: {}\nRows with warnings or errors: {}\n\nPress y to continue or n/q to cancel.",
        app.planned_actions(),
        app.mode_label(),
        app.rows()
            .iter()
            .filter(|row| row.error.is_some() || row.fallback_reason.is_some())
            .count()
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Line::from(vec![Span::styled(
                " Link Confirmation ",
                Theme::title(true),
            )]))
            .style(Theme::popup()),
    )
    .style(Theme::popup())
    .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

fn render_blocking_notice_overlay(frame: &mut Frame, message: &str) {
    let area = centered_rect(70, 35, frame.area());
    frame.render_widget(Clear, area);
    let body = Paragraph::new(format!("{message}\n\nPress q, Esc, or Enter to dismiss."))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    " Action Blocked ",
                    Theme::title(true),
                )]))
                .style(Theme::popup()),
        )
        .style(Theme::popup())
        .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn trim_middle(input: &str, max_len: usize) -> String {
    if input.chars().count() <= max_len {
        return input.to_string();
    }
    if max_len < 5 {
        return input.chars().take(max_len).collect();
    }
    let keep_each = (max_len - 3) / 2;
    let start: String = input.chars().take(keep_each).collect();
    let end: String = input
        .chars()
        .rev()
        .take(keep_each)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}...{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_rows_use_warning_style() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            status: "applied".to_string(),
            method: Some("symlink".to_string()),
            fallback_reason: Some("cross-device".to_string()),
            error: None,
        };

        assert_eq!(row_style(&row, false), Theme::warning());
    }

    #[test]
    fn selected_rows_keep_selection_style() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            status: "failed".to_string(),
            method: None,
            fallback_reason: None,
            error: Some("boom".to_string()),
        };

        assert_eq!(row_style(&row, true), Theme::accent());
    }

    #[test]
    fn empty_results_copy_mentions_path_setup() {
        let text = empty_results_text();
        assert!(text.contains("Press e"));
        assert!(text.contains("Then press S"));
    }

    #[test]
    fn context_text_groups_content() {
        let app = App::new(&crate::config::Config::default(), false);
        let text = context_text(&app);
        assert!(text.contains("Paths"));
        assert!(text.contains("Settings"));
        assert!(text.contains("Actions"));
    }
}
