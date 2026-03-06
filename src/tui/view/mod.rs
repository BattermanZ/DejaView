use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap};

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

fn status_chip_style(row: &LinkReportEntry, is_selected: bool) -> Style {
    if row.status.eq_ignore_ascii_case("failed") {
        return Theme::chip_danger();
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
        || row.fallback_reason.is_some()
    {
        return Theme::chip_warning();
    }
    if is_selected {
        Theme::badge_accent()
    } else {
        Theme::chip_success()
    }
}

fn row_style(row: &LinkReportEntry, is_selected: bool) -> Style {
    if is_selected {
        return Theme::panel_active();
    }
    if row.status.eq_ignore_ascii_case("failed") {
        return Theme::danger();
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
        || row.fallback_reason.is_some()
    {
        return Theme::warning();
    }
    Theme::base()
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
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
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

fn metric_card(title: &str, value: String) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from(Span::styled(format!(" {title}"), Theme::metric_label())),
        Line::from(Span::styled(format!(" {value}"), Theme::metric_value())),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .style(Theme::panel_active()),
    )
    .style(Theme::panel_active())
}

fn context_lines(app: &App) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(" PATHS", Theme::title(false))),
        Line::from(vec![
            Span::styled("Downloads", Theme::muted()),
            Span::raw(" "),
            Span::styled(trim_middle(&app.downloads_label(), 28), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Media", Theme::muted()),
            Span::raw(" "),
            Span::styled(trim_middle(&app.media_label(), 28), Theme::base()),
        ]),
        Line::raw(""),
        Line::from(Span::styled(" SETTINGS", Theme::title(false))),
        Line::from(vec![
            Span::styled("Min age", Theme::muted()),
            Span::raw(" "),
            Span::styled(format!("{}s", app.min_age_secs()), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Threads", Theme::muted()),
            Span::raw(" "),
            Span::styled(app.threads_label(), Theme::base()),
        ]),
        Line::raw(""),
        Line::from(Span::styled(" ACTIONS", Theme::title(false))),
        Line::from(vec![
            Span::styled(" e ", Theme::badge()),
            Span::styled("edit paths", Theme::base()),
        ]),
        Line::from(vec![
            Span::styled(" S ", Theme::badge()),
            Span::styled("scan duplicates", Theme::base()),
        ]),
        Line::from(vec![
            Span::styled(" L ", Theme::badge()),
            Span::styled("link confirmed", Theme::base()),
        ]),
        Line::from(vec![
            Span::styled(" R ", Theme::badge()),
            Span::styled("save report", Theme::base()),
        ]),
        Line::from(vec![
            Span::styled(" J ", Theme::badge()),
            Span::styled("json preview", Theme::base()),
        ]),
    ]
}

fn empty_results_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled("READY FOR A SCAN", Theme::title(true))),
        Line::raw(""),
        Line::from(Span::styled(
            "Set your paths, then scan for duplicate media groups.",
            Theme::base(),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" e ", Theme::badge_accent()),
            Span::styled("configure paths", Theme::base()),
        ]),
        Line::from(vec![
            Span::styled(" S ", Theme::badge()),
            Span::styled("run scan", Theme::base()),
        ]),
    ]
}

fn outcome_label(row: &LinkReportEntry) -> String {
    match row.method.as_deref() {
        Some("hardlink") => "HARDLINK".to_string(),
        Some("symlink") => "SYMLINK".to_string(),
        Some(other) => other.to_uppercase(),
        None => row.status.to_uppercase(),
    }
}

fn details_lines(app: &App) -> Vec<Line<'static>> {
    let Some(entry) = app.selected_entry() else {
        return vec![
            Line::from(Span::styled("NO SELECTION", Theme::title(true))),
            Line::raw(""),
            Line::from(Span::styled(
                "Run a scan and select a result to inspect the action outcome.",
                Theme::base(),
            )),
            Line::raw(""),
            Line::from(Span::styled("Run saved", Theme::muted())),
            Line::from(Span::styled(
                format_bytes(app.total_saved_bytes()),
                Theme::metric_value(),
            )),
        ];
    };

    let chip_style = status_chip_style(entry, true);
    vec![
        Line::from(Span::styled(" OUTCOME", Theme::title(true))),
        Line::from(Span::styled(
            format!(" {} ", outcome_label(entry)),
            chip_style,
        )),
        Line::raw(""),
        Line::from(Span::styled(" RUN TOTAL SAVED", Theme::muted())),
        Line::from(Span::styled(
            format_bytes(app.total_saved_bytes()),
            Theme::metric_value(),
        )),
        Line::raw(""),
        Line::from(Span::styled(" SOURCE", Theme::muted())),
        Line::from(Span::styled(trim_middle(&entry.source, 34), Theme::base())),
        Line::raw(""),
        Line::from(Span::styled(" TARGET", Theme::muted())),
        Line::from(Span::styled(trim_middle(&entry.target, 34), Theme::base())),
        Line::raw(""),
        Line::from(Span::styled(" NOTES", Theme::muted())),
        Line::from(Span::styled(
            entry
                .fallback_reason
                .clone()
                .or_else(|| entry.error.clone())
                .unwrap_or_else(|| "clean operation".to_string()),
            if entry.error.is_some() {
                Theme::danger()
            } else if entry.fallback_reason.is_some() {
                Theme::warning()
            } else {
                Theme::base()
            },
        )),
    ]
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::app()), area);
    let l = layout::dashboard(area);

    render_header(frame, l.top, app);

    let left = Paragraph::new(context_lines(app))
        .block(pane_block("Workspace", FocusPane::Left, app, false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(left, l.center_left);

    render_results(frame, l.center_main, app);

    let details = Paragraph::new(details_lines(app))
        .block(pane_block("Inspector", FocusPane::Right, app, false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(details, l.center_right);

    let log = Paragraph::new(app.log_lines().join("\n"))
        .block(pane_block("Run Log", FocusPane::Bottom, app, false).borders(Borders::ALL))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(log, l.bottom_log);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" q ", Theme::badge()),
        Span::styled("quit", Theme::muted()),
        Span::raw("  "),
        Span::styled(" tab ", Theme::badge()),
        Span::styled("focus", Theme::muted()),
        Span::raw("  "),
        Span::styled(" e ", Theme::badge_accent()),
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
            .border_style(Theme::accent()),
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

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(40)])
        .split(area);

    let brand = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3)])
        .split(top[0]);

    let wordmark = Paragraph::new(vec![
        Line::from(Span::styled(" DEJAVIEW ", Theme::banner())),
        Line::from(Span::styled(
            " media duplicate workspace ",
            Theme::badge_accent(),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Theme::accent()),
    )
    .style(Theme::app());
    frame.render_widget(wordmark, brand[0]);

    let sub = Paragraph::new(vec![
        Line::from(Span::styled(app.activity_phase(), Theme::title(true))),
        Line::from(Span::styled(app.last_log_line(), Theme::base())),
    ])
    .style(Theme::app());
    frame.render_widget(sub, brand[1]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(2),
        ])
        .split(top[1]);
    let metrics = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
            Constraint::Percentage(35),
        ])
        .split(right[0]);

    frame.render_widget(
        metric_card("FILES", app.files_considered().to_string()),
        metrics[0],
    );
    frame.render_widget(
        metric_card("PLANNED", app.planned_actions().to_string()),
        metrics[1],
    );
    frame.render_widget(
        metric_card("APPLIED", app.applied_actions().to_string()),
        metrics[2],
    );
    frame.render_widget(
        metric_card("SAVED", format_bytes(app.total_saved_bytes())),
        metrics[3],
    );

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    format!(" ACTIVITY  {} ", app.progress_label()),
                    Theme::title(true),
                )]))
                .border_style(Theme::accent())
                .style(Theme::panel_active()),
        )
        .gauge_style(Theme::banner())
        .ratio(app.progress_ratio().clamp(0.0, 1.0))
        .label(app.activity_phase());
    frame.render_widget(gauge, right[1]);

    let strip = Paragraph::new(Line::from(vec![
        Span::styled(" live ", Theme::badge_accent()),
        Span::raw(" "),
        Span::styled(app.throughput_label(), Theme::base()),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Theme::muted()),
    )
    .style(Theme::app());
    frame.render_widget(strip, right[2]);
}

fn render_results(frame: &mut Frame, area: Rect, app: &App) {
    if app.rows().is_empty() {
        let empty = Paragraph::new(empty_results_lines())
            .block(pane_block("Results", FocusPane::Center, app, true).borders(Borders::ALL))
            .style(Theme::panel_feature())
            .wrap(Wrap { trim: true });
        frame.render_widget(empty, area);
        return;
    }

    let rows: Vec<Row> = app
        .rows()
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let is_selected = idx == app.selected_row();
            Row::new(vec![
                Cell::from(Span::styled(
                    format!(" {} ", row.status.to_uppercase()),
                    status_chip_style(row, is_selected),
                )),
                Cell::from(Span::styled(
                    trim_middle(&row.source, 34),
                    row_style(row, is_selected),
                )),
                Cell::from(Span::styled(
                    outcome_label(row),
                    row_style(row, is_selected),
                )),
            ])
            .style(if is_selected {
                Theme::panel_active()
            } else {
                Theme::panel_feature()
            })
            .height(2)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Percentage(64),
            Constraint::Percentage(24),
        ],
    )
    .header(
        Row::new(vec!["STATE", "SOURCE", "OUTCOME"])
            .style(Theme::title(true))
            .height(2),
    )
    .block(pane_block("Results", FocusPane::Center, app, true).borders(Borders::ALL))
    .style(Theme::panel_feature())
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(78, 72, frame.area());
    frame.render_widget(Clear, area);
    let help = Paragraph::new(
        "Keyboard map\n\nGlobal\nq close overlay or quit when idle\n? help\ntab next pane\nshift+tab previous pane\n\nActions\ne edit downloads/media paths\nS scan for duplicates\nL confirm and run link job\nR save report\nJ open JSON preview\n\nConfig editor\ntype directly into the active field\nenter moves forward or saves\ntab switches field\nEsc cancels",
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Line::from(vec![Span::styled(" HELP ", Theme::title(true))]))
            .style(Theme::popup())
            .border_style(Theme::accent()),
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
                .title(Line::from(vec![Span::styled(
                    " REPORT JSON ",
                    Theme::title(true),
                )]))
                .style(Theme::popup())
                .border_style(Theme::accent()),
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
            " CONFIG EDITOR ",
            Theme::title(true),
        )]))
        .style(Theme::popup())
        .border_style(Theme::accent());
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
                " LINK CONFIRMATION ",
                Theme::title(true),
            )]))
            .style(Theme::popup())
            .border_style(Theme::accent()),
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
                    " ACTION BLOCKED ",
                    Theme::title(true),
                )]))
                .style(Theme::popup())
                .border_style(Theme::accent()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_rows_use_warning_chip() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            status: "applied".to_string(),
            method: Some("symlink".to_string()),
            fallback_reason: Some("cross-device".to_string()),
            error: None,
        };

        assert_eq!(status_chip_style(&row, false), Theme::chip_warning());
    }

    #[test]
    fn empty_results_copy_mentions_scan() {
        let lines = empty_results_lines();
        let combined = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("READY FOR A SCAN"));
        assert!(combined.contains("run scan"));
    }

    #[test]
    fn context_lines_keep_sections() {
        let app = App::new(&crate::config::Config::default(), false);
        let combined = context_lines(&app)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("PATHS"));
        assert!(combined.contains("SETTINGS"));
        assert!(combined.contains("ACTIONS"));
    }
}
