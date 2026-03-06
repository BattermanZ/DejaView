use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};

use crate::tui::app::{App, FocusPane, Overlay};
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

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let l = layout::dashboard(area);

    let top = Paragraph::new(format!(
        "Mode: {}  |  Strict: {}  |  Files: {}  |  Planned: {}  |  Applied: {}  |  Saved: {}",
        app.mode_label(),
        app.strict,
        app.files_considered(),
        app.planned_actions(),
        app.applied_actions(),
        format_bytes(app.total_saved_bytes()),
    ))
    .block(Block::default().borders(Borders::ALL).title("Status"))
    .style(Theme::accent());
    frame.render_widget(top, l.top);

    let left_block = Block::default()
        .borders(Borders::ALL)
        .title("Context")
        .border_style(app.focus_style(FocusPane::Left));
    let left_items = vec![
        ListItem::new(format!("Downloads: {}", app.downloads_label())),
        ListItem::new(format!("Media: {}", app.media_label())),
        ListItem::new(format!("Min age: {}s", app.min_age_secs())),
        ListItem::new(format!("Threads: {}", app.threads_label())),
        ListItem::new(""),
        ListItem::new("[S] Scan  [L] Link  [R] Save report"),
        ListItem::new("[J] JSON overlay  [Tab] Next pane"),
    ];
    frame.render_widget(List::new(left_items).block(left_block), l.center_left);

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
    .header(Row::new(vec!["Status", "Method", "Source", "Target"]).style(Theme::accent()))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Results")
            .border_style(app.focus_style(FocusPane::Center)),
    );
    frame.render_widget(table, l.center_main);

    let details = Paragraph::new(app.selected_details())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Details")
                .border_style(app.focus_style(FocusPane::Right)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(details, l.center_right);

    let log_items: Vec<ListItem> = app.log_lines().iter().cloned().map(ListItem::new).collect();
    let log = List::new(log_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Run Log")
            .border_style(app.focus_style(FocusPane::Bottom)),
    );
    frame.render_widget(log, l.bottom_log);

    let footer = Paragraph::new(
        "q quit | tab pane | j/k move | S scan | L link | R save report | J json | ? help",
    )
    .style(Theme::muted());
    frame.render_widget(footer, l.footer);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => render_help_overlay(frame),
        Overlay::JsonPreview(json) => render_json_overlay(frame, json),
        Overlay::ConfirmLink => render_confirm_link_overlay(frame, app),
        Overlay::BlockingNotice(message) => render_blocking_notice_overlay(frame, message),
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);
    let help = Paragraph::new(
        "Help\n\nGlobal:\n- q: close overlay or quit when idle\n- ?: toggle help\n- tab: switch focus pane\n\nActions:\n- S: run scan (json subprocess)\n- L: open link confirmation\n- l: reserved for per-group action intent\n- j/k or arrows: move selected row\n- R: save current report\n- J: view current report JSON\n\nClose overlays with Esc, q, or the documented toggle key."
    )
    .block(Block::default().borders(Borders::ALL).title("Help"))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, area);
}

fn render_json_overlay(frame: &mut Frame, json: &str) {
    let area = centered_rect(90, 80, frame.area());
    frame.render_widget(Clear, area);
    let body = Paragraph::new(json)
        .block(Block::default().borders(Borders::ALL).title("Report JSON"))
        .wrap(Wrap { trim: false });
    frame.render_widget(body, area);
}

fn render_confirm_link_overlay(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 45, frame.area());
    frame.render_widget(Clear, area);
    let body = Paragraph::new(format!(
        "Confirm link run\n\nThis will execute filesystem changes for the current configuration.\n\nEstimated planned actions: {}\nCurrent report mode: {}\nRows with warnings or errors: {}\n\nPress y to continue or n/q to cancel.",
        app.planned_actions(),
        app.mode_label(),
        app.rows()
            .iter()
            .filter(|row| row.error.is_some() || row.fallback_reason.is_some())
            .count()
    ))
    .block(Block::default().borders(Borders::ALL).title("Link Confirmation"))
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
                .title("Action Blocked"),
        )
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
}
