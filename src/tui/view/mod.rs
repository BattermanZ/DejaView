use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::tui::app::{App, ConfigEditor, ConfigField, Overlay};
use crate::tui::layout;
use crate::tui::model::LinkReportEntry;
use crate::tui::theme::Theme;
use crate::tui::wordmark;

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

fn trim_to_width(input: &str, width: u16) -> String {
    trim_middle(input, usize::from(width.saturating_sub(1)))
}

fn status_chip_style(row: &LinkReportEntry, _is_selected: bool) -> Style {
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
    Theme::chip_success()
}

fn row_style(row: &LinkReportEntry, is_selected: bool) -> Style {
    if row.status.eq_ignore_ascii_case("failed") {
        return if is_selected {
            Theme::result_row_selected().fg(Theme::danger().fg.unwrap_or(Color::Reset))
        } else {
            Theme::danger()
        };
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
        || row.fallback_reason.is_some()
    {
        return if is_selected {
            Theme::result_row_selected().fg(Theme::warning().fg.unwrap_or(Color::Reset))
        } else {
            Theme::warning()
        };
    }
    if is_selected {
        Theme::result_row_selected().fg(Theme::accent().fg.unwrap_or(Color::Reset))
    } else {
        Theme::base()
    }
}

fn pane_block(title: &str, accent: bool) -> Block<'static> {
    let title_style = if accent {
        Theme::title(true)
    } else {
        Theme::title(false)
    };

    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            title_style,
        )]))
        .style(Theme::panel())
        .border_style(if accent {
            Theme::accent()
        } else {
            Theme::line()
        })
}

fn card_block(title: &str, accent: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            if accent {
                Theme::title(true)
            } else {
                Theme::muted()
            },
        )]))
        .style(Theme::card())
        .border_style(if accent {
            Theme::accent()
        } else {
            Theme::line()
        })
}

fn metric_card(title: &str, value: String) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(value, Theme::metric_number())))
        .block(card_block(title, false))
        .style(Theme::card())
        .alignment(Alignment::Left)
}

fn key_hint(key: &str, label: &str, accent: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {key} "),
            if accent {
                Theme::badge_accent()
            } else {
                Theme::badge()
            },
        ),
        Span::raw(" "),
        Span::styled(label.to_string(), Theme::muted()),
    ])
}

fn display_path(input: &str, max_len: usize, expand: bool) -> String {
    if expand {
        input.to_string()
    } else {
        trim_middle(input, max_len)
    }
}

fn mode_chip(app: &App) -> (&'static str, Style) {
    if app.mode_label().eq_ignore_ascii_case("Linking") {
        return ("LINK ACTIVE", Theme::badge_accent());
    }
    if app.mode_label().eq_ignore_ascii_case("Scanning") {
        return ("SCAN ACTIVE", Theme::badge_accent());
    }
    if app.mode_label().eq_ignore_ascii_case("Link") {
        return ("DONE", Theme::chip_success());
    }
    if app.mode_label().eq_ignore_ascii_case("Scan") {
        return ("READY", Theme::badge_lavender());
    }
    ("IDLE", Theme::badge())
}

fn sidebar_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let path_width = usize::from(width.saturating_sub(14)).max(12);
    vec![
        Line::from(Span::styled(" PATHS", Theme::title(false))),
        Line::from(vec![
            Span::styled("Downloads", Theme::dim()),
            Span::raw("  "),
            Span::styled(
                display_path(&app.downloads_label(), path_width, app.expand_paths()),
                Theme::base(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Media", Theme::dim()),
            Span::raw("      "),
            Span::styled(
                display_path(&app.media_label(), path_width, app.expand_paths()),
                Theme::base(),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(" SETTINGS", Theme::title(false))),
        Line::from(vec![
            Span::styled("Min age", Theme::dim()),
            Span::raw("    "),
            Span::styled(format!("{}s", app.min_age_secs()), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Threads", Theme::dim()),
            Span::raw("    "),
            Span::styled(app.threads_label(), Theme::base()),
        ]),
        Line::raw(""),
        Line::from(Span::styled(" ACTIONS", Theme::title(false))),
        key_hint("e", "edit paths", true),
        key_hint("S", "scan library", false),
        key_hint("L", "link confirmed", false),
        key_hint("R", "save report", false),
        key_hint("J", "json preview", false),
        key_hint("p", "toggle paths", false),
    ]
}

fn empty_results_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled("READY TO SCAN", Theme::title(true))),
        Line::raw(""),
        Line::from(Span::styled(
            "No duplicate actions are loaded yet.",
            Theme::base(),
        )),
        Line::raw(""),
        key_hint("e", "set paths", true),
        key_hint("S", "run scan", false),
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

fn notes_text(entry: &LinkReportEntry) -> String {
    entry
        .fallback_reason
        .clone()
        .or_else(|| entry.error.clone())
        .unwrap_or_else(|| "Clean operation".to_string())
}

fn inspector_lines(app: &App) -> Vec<Line<'static>> {
    let Some(entry) = app.selected_entry() else {
        return vec![
            Line::from(Span::styled(" OUTCOME", Theme::title(false))),
            Line::from(Span::styled("No selection", Theme::inspector_value())),
            Line::raw(""),
            Line::from(Span::styled(" RUN TOTAL", Theme::title(false))),
            Line::from(Span::styled(
                format_bytes(app.total_saved_bytes()),
                Theme::metric_number(),
            )),
            Line::raw(""),
            Line::from(Span::styled(" NOTES", Theme::title(false))),
            Line::from(Span::styled(
                "Run a scan and select a result to inspect it.",
                Theme::muted(),
            )),
        ];
    };

    let notes_style = if entry.error.is_some() {
        Theme::danger()
    } else if entry.fallback_reason.is_some() {
        Theme::warning()
    } else {
        Theme::base()
    };

    vec![
        Line::from(Span::styled(" OUTCOME", Theme::title(false))),
        Line::from(Span::styled(
            format!(" {} ", outcome_label(entry)),
            status_chip_style(entry, true),
        )),
        Line::raw(""),
        Line::from(Span::styled(" RUN TOTAL", Theme::title(false))),
        Line::from(Span::styled(
            format_bytes(app.total_saved_bytes()),
            Theme::metric_number(),
        )),
        Line::raw(""),
        Line::from(Span::styled(" SOURCE", Theme::title(false))),
        Line::from(Span::styled(entry.source.clone(), Theme::base())),
        Line::raw(""),
        Line::from(Span::styled(" TARGET", Theme::title(false))),
        Line::from(Span::styled(entry.target.clone(), Theme::base())),
        Line::raw(""),
        Line::from(Span::styled(" NOTES", Theme::title(false))),
        Line::from(Span::styled(notes_text(entry), notes_style)),
    ]
}

fn header_activity_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let line_width = width.saturating_sub(4);
    let (chip_label, chip_style) = mode_chip(app);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!(" {chip_label} "), chip_style),
            Span::raw(" "),
            Span::styled(app.activity_phase(), Theme::accent_soft()),
            Span::raw("  "),
            Span::styled(format!("elapsed {}", app.elapsed_label()), Theme::muted()),
        ]),
        Line::from(Span::styled(app.throughput_label(), Theme::muted())),
    ];
    let message = app.activity_message();
    if !message.is_empty() {
        lines.push(Line::from(Span::styled(
            trim_to_width(&message, line_width),
            Theme::dim(),
        )));
    }
    if !app.active_worker_paths().is_empty() {
        for (worker, path) in app.active_worker_paths().into_iter().take(3) {
            lines.push(Line::from(vec![
                Span::styled(format!("worker {worker}: "), Theme::dim()),
                Span::styled(
                    display_path(path, usize::from(line_width).max(12), app.expand_paths()),
                    Theme::base(),
                ),
            ]));
        }
    } else if let Some(path) = app.current_path() {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", app.current_path_label()), Theme::dim()),
            Span::styled(
                display_path(path, usize::from(line_width).max(12), app.expand_paths()),
                Theme::base(),
            ),
        ]));
    }
    lines
}

fn footer_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(" q ", Theme::badge()),
        Span::styled("quit", Theme::muted()),
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
        Span::styled(" p ", Theme::badge()),
        Span::styled("paths", Theme::muted()),
        Span::raw("  "),
        Span::styled(" j/k ", Theme::badge()),
        Span::styled("rows", Theme::muted()),
        Span::raw("  "),
        Span::styled(" ? ", Theme::badge()),
        Span::styled("help", Theme::muted()),
    ])
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WordmarkVariant {
    Large,
    Compact,
    Minimal,
}

fn wordmark_variant(width: u16) -> WordmarkVariant {
    if width >= 56 {
        WordmarkVariant::Large
    } else if width >= 24 {
        WordmarkVariant::Compact
    } else {
        WordmarkVariant::Minimal
    }
}

fn large_wordmark_lines() -> Vec<Line<'static>> {
    wordmark::filled_block_lines()
}

fn compact_wordmark_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled("DEJAVIEW", Theme::wordmark()),
            Span::raw(" "),
            Span::styled("DEJAVIEW", Theme::wordmark_shadow()),
        ]),
        Line::from(vec![
            Span::styled("duplicate workspace", Theme::subtitle()),
            Span::raw(" "),
            Span::styled("media utility", Theme::wordmark_shadow()),
        ]),
    ]
}

fn minimal_wordmark_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled("DEJAVIEW", Theme::wordmark())),
        Line::from(Span::styled("media duplicate workspace", Theme::subtitle())),
    ]
}

fn wordmark_lines(width: u16) -> Vec<Line<'static>> {
    match wordmark_variant(width) {
        WordmarkVariant::Large => large_wordmark_lines(),
        WordmarkVariant::Compact => compact_wordmark_lines(),
        WordmarkVariant::Minimal => minimal_wordmark_lines(),
    }
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::canvas()), area);
    let l = layout::dashboard(area);

    render_header(frame, l.top, app);

    let left_block = pane_block("Workspace", false);
    let left_inner = left_block.inner(l.center_left);
    let left = Paragraph::new(sidebar_lines(app, left_inner.width))
        .block(left_block)
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(left, l.center_left);

    render_results(frame, l.center_main, app);

    let details = Paragraph::new(inspector_lines(app))
        .block(pane_block("Inspector", false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(details, l.center_right);

    let log = Paragraph::new(app.log_lines().join("\n"))
        .block(pane_block("Run Log", false))
        .style(Theme::panel())
        .wrap(Wrap { trim: true });
    frame.render_widget(log, l.bottom_log);

    let footer = Paragraph::new(footer_line())
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Theme::line())
                .style(Theme::canvas()),
        )
        .style(Theme::canvas());
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
    let brand_width = area.width.saturating_sub(52).clamp(28, 60);
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(brand_width), Constraint::Min(52)])
        .split(area);

    let brand_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Theme::line())
        .style(Theme::canvas());
    let brand_inner = brand_block.inner(split[0]);
    frame.render_widget(brand_block, split[0]);
    let wordmark_lines = wordmark_lines(brand_inner.width);
    let wordmark_height = u16::try_from(wordmark_lines.len()).unwrap_or(brand_inner.height);
    let brand_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(wordmark_height.min(brand_inner.height)),
            Constraint::Min(0),
        ])
        .split(brand_inner);
    frame.render_widget(
        Paragraph::new(wordmark_lines)
            .style(Theme::canvas())
            .alignment(Alignment::Center),
        brand_layout[1],
    );

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(split[1]);

    let metrics = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(16),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(28),
            Constraint::Percentage(28),
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
        metric_card("SAVED", format_bytes(app.saved_bytes())),
        metrics[3],
    );
    frame.render_widget(
        metric_card("TO SAVE", format_bytes(app.potential_saved_bytes())),
        metrics[4],
    );

    let activity = Paragraph::new(header_activity_lines(app, right[1].width))
        .block(card_block("Activity", true))
        .style(Theme::card())
        .wrap(Wrap { trim: true });
    frame.render_widget(activity, right[1]);
}

fn render_results(frame: &mut Frame, area: Rect, app: &App) {
    let block = pane_block("Results", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    let summary = Paragraph::new(Line::from(vec![
        Span::styled(app.activity_phase(), Theme::title(true)),
        Span::raw("  "),
        Span::styled(
            format!(
                "{} rows  |  {} files  |  {} planned  |  {} applied",
                app.rows().len(),
                app.files_considered(),
                app.planned_actions(),
                app.applied_actions()
            ),
            Theme::muted(),
        ),
    ]))
    .style(Theme::panel());
    frame.render_widget(summary, content[0]);

    if app.rows().is_empty() {
        let empty = Paragraph::new(empty_results_lines())
            .style(Theme::panel())
            .wrap(Wrap { trim: true });
        frame.render_widget(empty, content[1]);
        return;
    }

    let state_width = 12u16;
    let max_outcome_len = app
        .rows()
        .iter()
        .map(|row| outcome_label(row).chars().count())
        .max()
        .unwrap_or_else(|| "OUTCOME".chars().count());
    let outcome_width = (max_outcome_len as u16 + 2).clamp(12, 18);
    let source_width = content[1]
        .width
        .saturating_sub(state_width)
        .saturating_sub(outcome_width)
        .saturating_sub(4);

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
                    trim_to_width(&row.source, source_width),
                    row_style(row, is_selected),
                )),
                Cell::from(Span::styled(
                    outcome_label(row),
                    row_style(row, is_selected),
                )),
            ])
            .style(if is_selected {
                Theme::result_row_selected()
            } else {
                Theme::result_row()
            })
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(state_width),
            Constraint::Min(source_width.max(12)),
            Constraint::Length(outcome_width),
        ],
    )
    .header(Row::new(vec![
        Cell::from(Span::styled("STATE", Theme::title(false))),
        Cell::from(Span::styled("SOURCE", Theme::title(false))),
        Cell::from(Span::styled("OUTCOME", Theme::title(false))),
    ]))
    .style(Theme::panel())
    .column_spacing(2);
    frame.render_widget(table, content[1]);
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(78, 72, frame.area());
    frame.render_widget(Clear, area);
    let help = Paragraph::new(
        "Keyboard map\n\nGlobal\nq close overlay or quit when idle\n? help\np toggle compact/full paths\nj/k move rows\n\nActions\ne edit downloads/media paths\nS scan for duplicates\nL confirm and run link job\nR save report\nJ open JSON preview\n\nConfig editor\ntype directly into the active field\nenter moves forward or saves\ntab switches field\nEsc cancels",
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
                    Theme::line()
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
                    Theme::line()
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
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Status")
            .border_style(Theme::line()),
    );
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
            size: 42,
            status: "applied".to_string(),
            method: Some("symlink".to_string()),
            fallback_reason: Some("cross-device".to_string()),
            error: None,
        };

        assert_eq!(status_chip_style(&row, false), Theme::chip_warning());
    }

    #[test]
    fn selected_success_rows_keep_selected_background() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            size: 42,
            status: "applied".to_string(),
            method: Some("hardlink".to_string()),
            fallback_reason: None,
            error: None,
        };

        assert_eq!(row_style(&row, true).bg, Theme::result_row_selected().bg);
    }

    #[test]
    fn empty_results_copy_mentions_scan() {
        let lines = empty_results_lines();
        let combined = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("READY TO SCAN"));
        assert!(combined.contains("run scan"));
    }

    #[test]
    fn sidebar_lines_keep_sections() {
        let app = App::new(&crate::config::Config::default(), false);
        let combined = sidebar_lines(&app, 30)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("PATHS"));
        assert!(combined.contains("SETTINGS"));
        assert!(combined.contains("ACTIONS"));
    }

    #[test]
    fn inspector_labels_run_total_not_item_saved() {
        let app = App::new(&crate::config::Config::default(), false);
        let combined = inspector_lines(&app)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("RUN TOTAL"));
    }

    #[test]
    fn header_activity_lines_use_truthful_labels() {
        let app = App::new(&crate::config::Config::default(), false);
        let combined = header_activity_lines(&app, 80)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("IDLE"));
        assert!(combined.contains("READY TO SCAN"));
    }

    #[test]
    fn header_activity_lines_show_worker_paths() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.set_hash_activity_for_test(
            "hashing_sample",
            &[(2, "/downloads/alpha.mkv"), (4, "/downloads/beta.mkv")],
        );

        let combined = header_activity_lines(&app, 120)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("worker 2:"));
        assert!(combined.contains("worker 4:"));
        assert!(combined.contains("alpha.mkv"));
    }

    #[test]
    fn header_activity_lines_do_not_repeat_hashing_copy() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.set_running_scan_for_test();
        app.set_hash_activity_for_test("hashing_full", &[(3, "/downloads/dupe.mkv")]);
        app.set_hash_progress_for_test(10, 12);

        let combined = header_activity_lines(&app, 120)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(combined.contains("VERIFYING DUPLICATES"));
        assert!(combined.contains("10 / 12 duplicate files verified"));
        assert!(!combined.contains("Verifying duplicate files: 10 / 12"));
    }

    #[test]
    fn metric_split_distinguishes_saved_from_to_save() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.set_report_for_test(crate::protocol::RunReport {
            mode: "link".to_string(),
            dry_run: false,
            strict: false,
            files_considered: 2,
            planned_actions: 2,
            applied_actions: 1,
            total_saved_bytes: 30,
            skipped_by_min_age: 0,
            warnings: crate::protocol::WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![
                LinkReportEntry {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    size: 10,
                    status: "applied".to_string(),
                    method: Some("hardlink".to_string()),
                    fallback_reason: None,
                    error: None,
                },
                LinkReportEntry {
                    source: "c".to_string(),
                    target: "d".to_string(),
                    size: 20,
                    status: "planned".to_string(),
                    method: None,
                    fallback_reason: None,
                    error: None,
                },
            ],
        });

        assert_eq!(format_bytes(app.saved_bytes()), "10.00 B");
        assert_eq!(format_bytes(app.potential_saved_bytes()), "20.00 B");
    }

    #[test]
    fn wordmark_uses_large_variant_for_wide_headers() {
        let combined = wordmark_lines(60)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(wordmark_variant(60), WordmarkVariant::Large));
        assert!(combined.contains("████"));
    }

    #[test]
    fn wordmark_uses_compact_variant_for_medium_headers() {
        let combined = wordmark_lines(28)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(wordmark_variant(28), WordmarkVariant::Compact));
        assert!(combined.contains("DEJAVIEW"));
        assert!(combined.contains("duplicate workspace"));
    }

    #[test]
    fn wordmark_uses_minimal_variant_for_narrow_headers() {
        let combined = wordmark_lines(18)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(wordmark_variant(18), WordmarkVariant::Minimal));
        assert!(combined.contains("DEJAVIEW"));
        assert!(combined.contains("media duplicate workspace"));
    }

    #[test]
    fn display_path_respects_expanded_mode() {
        let path = "/very/long/path/that/should/remain/fully/visible/source-file.mkv";
        assert_eq!(display_path(path, 20, true), path);
        assert!(display_path(path, 20, false).contains("..."));
    }

    #[test]
    fn trim_to_width_uses_available_width() {
        let path = "/this/is/a/very/long/path/example.mkv";
        assert!(trim_to_width(path, 10).chars().count() <= 9);
    }
}
