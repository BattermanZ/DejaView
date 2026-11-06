use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState, Wrap,
};

use crate::tui::app::{App, ConfigEditor, ConfigField, ConfirmLinkMode, Overlay};
use crate::tui::layout;
use crate::tui::model::LinkReportEntry;
use crate::tui::theme::Theme;
use crate::tui::wordmark;

/// A size in binary units, as the CLI prints it: `4.9 KiB`.
pub(crate) fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut idx = 0usize;
    while value >= 1024.0 && idx < units.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[idx])
    }
}

/// A row's path as the table shows it: below its folder, or whole when the
/// person asked for full paths with `p`.
fn row_path<'a>(path: &'a str, root: Option<&std::path::Path>, expand: bool) -> &'a str {
    if expand {
        return path;
    }
    root.and_then(|root| std::path::Path::new(path).strip_prefix(root).ok())
        .and_then(|rel| rel.to_str())
        .filter(|rel| !rel.is_empty())
        .unwrap_or(path)
}

/// Cut `path` to `width` characters, keeping its file name, which is what
/// tells one episode from the next: the folders give way first, from the
/// left. A file name too long on its own keeps its start, where a release
/// names its title and episode; its end is a release group the rows share.
fn trim_keep_name(path: &str, width: usize) -> String {
    let count = path.chars().count();
    if count <= width {
        return path.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.chars().count() < width {
        let tail: String = path.chars().skip(count - (width - 1)).collect();
        return format!("…{tail}");
    }
    let start: String = name.chars().take(width - 1).collect();
    format!("{start}…")
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
    if row.is_failed() {
        return Theme::chip_danger();
    }
    if row.is_planned() || row.is_skipped() {
        return Theme::chip_pending();
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
    {
        return Theme::chip_success_alt();
    }
    Theme::chip_success()
}

fn row_style(row: &LinkReportEntry, is_selected: bool) -> Style {
    if row.is_failed() {
        return if is_selected {
            Theme::result_row_selected().fg(Theme::danger().fg.unwrap_or(Color::Reset))
        } else {
            Theme::danger()
        };
    }
    if row.is_planned() || row.is_skipped() {
        return if is_selected {
            Theme::result_row_selected().fg(Theme::muted().fg.unwrap_or(Color::Reset))
        } else {
            Theme::muted()
        };
    }
    if row
        .method
        .as_deref()
        .is_some_and(|method| method.eq_ignore_ascii_case("symlink"))
    {
        return if is_selected {
            Theme::result_row_selected().fg(Theme::accent_soft().fg.unwrap_or(Color::Reset))
        } else {
            Theme::accent_soft()
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
    if app.mode_label().eq_ignore_ascii_case("Cancelling") {
        return ("CANCELLING", Theme::chip_danger());
    }
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
            Span::styled("Min size", Theme::dim()),
            Span::raw("   "),
            Span::styled(format_bytes(app.min_size_bytes()), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Threads", Theme::dim()),
            Span::raw("    "),
            Span::styled(app.threads_label(), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Full reads", Theme::dim()),
            Span::raw(" "),
            Span::styled(app.full_threads_label(), Theme::base()),
        ]),
        Line::from(vec![
            Span::styled("Link mode", Theme::dim()),
            Span::raw("  "),
            Span::styled(app.link_mode_label(), Theme::base()),
        ]),
        Line::raw(""),
        Line::from(Span::styled(" ACTIONS", Theme::title(false))),
        key_hint("e", "edit paths", true),
        key_hint("S", "scan library", false),
        key_hint("X", "rescan fresh", false),
        key_hint("l", "link selected", false),
        key_hint("L", "link all", false),
        key_hint("c", "cancel run", false),
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

/// A row's status in the STATE column, at most seven letters so the column
/// stays narrow; OUTCOME and the Inspector say "ALREADY LINKED" in full.
fn state_label(row: &LinkReportEntry) -> String {
    if row.is_already_linked() {
        return "ALREADY".to_string();
    }
    row.status.to_uppercase()
}

fn outcome_label(row: &LinkReportEntry) -> String {
    if row.is_already_linked() {
        return "ALREADY LINKED".to_string();
    }
    match row.method.as_deref() {
        Some("try-hardlink") => "TRY HARDLINK".to_string(),
        Some("hardlink") => "HARDLINK".to_string(),
        Some("symlink") => "SYMLINK".to_string(),
        Some(other) => other.to_uppercase(),
        None => row.status.to_uppercase(),
    }
}

fn visible_row_window(
    total_rows: usize,
    selected_row: usize,
    available_rows: usize,
) -> (usize, usize) {
    if total_rows == 0 || available_rows == 0 {
        return (0, 0);
    }

    let window = available_rows.min(total_rows);
    let max_start = total_rows.saturating_sub(window);
    let start = selected_row
        .saturating_sub(window.saturating_sub(1))
        .min(max_start);
    (start, start + window)
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
            Line::from(Span::styled(" SELECTED SIZE", Theme::title(false))),
            Line::from(Span::styled("No selection", Theme::metric_number())),
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
        Line::from(Span::styled(" SELECTED SIZE", Theme::title(false))),
        Line::from(Span::styled(
            format_bytes(entry.size),
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

/// The footer's keys, in the order shown, each with how much it matters
/// when the terminal is too narrow for all: `q` and `?` always stay, since
/// Help lists the rest.
const FOOTER_KEYS: [(&str, &str, u8); 9] = [
    ("q", "quit", 0),
    ("e", "paths", 3),
    ("S", "scan", 1),
    ("l", "link one", 2),
    ("L", "link all", 2),
    ("c", "cancel", 1),
    ("p", "paths", 5),
    ("j/k", "rows", 4),
    ("?", "help", 0),
];

/// As many of the footer's keys as fit in `width`, dropping the least
/// needed first.
fn footer_line(width: u16) -> Line<'static> {
    let cost = |(key, label, _): &(&str, &str, u8)| key.chars().count() + label.chars().count() + 4;
    let mut shown: Vec<bool> = vec![true; FOOTER_KEYS.len()];
    let total = |shown: &[bool]| -> usize {
        FOOTER_KEYS
            .iter()
            .zip(shown)
            .filter(|(_, shown)| **shown)
            .map(|(key, _)| cost(key))
            .sum()
    };
    for level in (1..=5).rev() {
        if total(&shown) <= usize::from(width) {
            break;
        }
        for (index, (_, _, priority)) in FOOTER_KEYS.iter().enumerate() {
            if *priority == level {
                shown[index] = false;
            }
        }
    }
    let mut spans = Vec::new();
    for ((key, label, _), _) in FOOTER_KEYS.iter().zip(&shown).filter(|(_, shown)| **shown) {
        spans.push(Span::styled(
            format!(" {key} "),
            if *key == "e" {
                Theme::badge_accent()
            } else {
                Theme::badge()
            },
        ));
        spans.push(Span::styled(label.to_string(), Theme::muted()));
        spans.push(Span::raw("  "));
    }
    spans.pop();
    Line::from(spans)
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
        Line::from(Span::styled("DEJAVIEW", Theme::wordmark())),
        Line::from(Span::styled("duplicate workspace", Theme::subtitle())),
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

/// The newest log lines that fit, oldest first, one row each: a line wider than
/// the pane is cut with an ellipsis, so the newest line is never pushed out.
/// The log file keeps every line whole.
fn newest_log_lines(lines: &[String], width: u16, height: u16) -> String {
    let width = usize::from(width);
    let start = lines.len().saturating_sub(usize::from(height));
    lines[start..]
        .iter()
        .map(|line| {
            if line.chars().count() <= width {
                line.clone()
            } else {
                let mut cut: String = line.chars().take(width.saturating_sub(1)).collect();
                cut.push('…');
                cut
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::canvas()), area);
    let l = layout::dashboard(area);

    if l.compact_header {
        render_compact_header(frame, l.top, app);
    } else {
        render_header(frame, l.top, app);
    }

    if let Some(sidebar) = l.center_left {
        let left_block = pane_block("Workspace", false);
        let left_inner = left_block.inner(sidebar);
        let left = Paragraph::new(sidebar_lines(app, left_inner.width))
            .block(left_block)
            .style(Theme::panel())
            .wrap(Wrap { trim: true });
        frame.render_widget(left, sidebar);
    }

    render_results(frame, l.center_main, app);

    if let Some(inspector) = l.center_right {
        let details = Paragraph::new(inspector_lines(app))
            .block(pane_block("Inspector", false))
            .style(Theme::panel())
            .wrap(Wrap { trim: true });
        frame.render_widget(details, inspector);
    }

    let log_block = pane_block("Run Log", false);
    let log_inner = log_block.inner(l.bottom_log);
    let log = Paragraph::new(newest_log_lines(
        &app.log_lines(),
        log_inner.width,
        log_inner.height,
    ))
    .block(log_block)
    .style(Theme::panel());
    frame.render_widget(log, l.bottom_log);

    let footer_block = Block::default()
        .borders(if l.footer_border {
            Borders::TOP
        } else {
            Borders::NONE
        })
        .border_style(Theme::line())
        .style(Theme::canvas());
    let footer = Paragraph::new(footer_line(l.footer.width))
        .block(footer_block)
        .style(Theme::canvas());
    frame.render_widget(footer, l.footer);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => render_help_overlay(frame, app),
        Overlay::JsonPreview(json) => render_json_overlay(frame, app, json),
        Overlay::ConfigEditor(editor) => render_config_overlay(frame, editor),
        Overlay::ConfirmLink(mode) => render_confirm_link_overlay(frame, app, *mode),
        Overlay::ConfirmRescan => render_confirm_rescan_overlay(frame),
        Overlay::BlockingNotice(message) => render_blocking_notice_overlay(frame, message),
    }
}

/// The counts the header's cards show, as one line for a short terminal.
/// Too narrow for every count, the line drops FILES first, which the
/// activity line repeats, then APPLIED, so the space counts stay whole.
fn metrics_line(app: &App, width: u16) -> Line<'static> {
    let counts = [
        ("FILES", app.files_considered().to_string(), 2),
        ("PLANNED", app.planned_actions().to_string(), 0),
        ("APPLIED", app.applied_actions().to_string(), 1),
        ("SAVED", format_bytes(app.saved_bytes()), 0),
        ("TO SAVE", format_bytes(app.potential_saved_bytes()), 0),
    ];
    let separator = "  ";
    let fits = |most: u8| {
        let shown = counts.iter().filter(|(_, _, drop)| *drop <= most);
        let text: usize = shown
            .clone()
            .map(|(label, value, _)| label.chars().count() + 1 + value.chars().count())
            .sum();
        text + separator.len() * shown.count().saturating_sub(1) <= usize::from(width)
    };
    let most = (0..=2).rev().find(|&most| fits(most)).unwrap_or(0);
    let mut spans = Vec::new();
    for (label, value, _) in counts.into_iter().filter(|(_, _, drop)| *drop <= most) {
        spans.push(Span::styled(format!("{label} "), Theme::dim()));
        spans.push(Span::styled(value, Theme::metric_number()));
        spans.push(Span::raw(separator));
    }
    spans.pop();
    Line::from(spans)
}

/// The header of a short or narrow terminal: one block, no wordmark, the
/// counts on one line above the activity.
fn render_compact_header(frame: &mut Frame, area: Rect, app: &App) {
    let block = card_block("DejaView", true);
    let inner = block.inner(area);
    let mut lines = vec![metrics_line(app, inner.width)];
    lines.extend(header_activity_lines(app, area.width));
    lines.truncate(usize::from(inner.height));
    frame.render_widget(
        Paragraph::new(lines).block(block).style(Theme::card()),
        area,
    );
}

/// The widths of the five metric cards in `width` columns: each at least
/// wide enough for its title, the rest shared out.
fn metric_widths(width: u16) -> [u16; 5] {
    let mut widths = [11u16, 11, 11, 13, 13];
    let spare = width.saturating_sub(widths.iter().sum());
    for (index, slot) in widths.iter_mut().enumerate() {
        *slot += spare / 5 + u16::from(index < usize::from(spare % 5));
    }
    widths
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    // The cards need 59 columns for their titles; the wordmark takes what
    // is left, within reason.
    let brand_width = area.width.saturating_sub(62).clamp(24, 60);
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(brand_width), Constraint::Min(59)])
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
        .constraints(metric_widths(right[0].width).map(Constraint::Length))
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

    // The counts after the row count repeat the header's, so a narrow pane
    // leaves them out rather than cutting them mid-word.
    let phase = app.activity_phase();
    let rows = format!("{} rows", app.rows().len());
    let counts = format!(
        "{rows}  |  {} files  |  {} planned  |  {} applied",
        app.files_considered(),
        app.planned_actions(),
        app.applied_actions()
    );
    let room = usize::from(content[0].width).saturating_sub(phase.chars().count() + 2);
    let summary = Paragraph::new(Line::from(vec![
        Span::styled(phase, Theme::title(true)),
        Span::raw("  "),
        Span::styled(
            if counts.chars().count() <= room {
                counts
            } else {
                rows
            },
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

    let state_width = 10u16;
    let size_width = 9u16;
    let max_outcome_len = app
        .rows()
        .iter()
        .map(|row| outcome_label(row).chars().count())
        .max()
        .unwrap_or_else(|| "OUTCOME".chars().count());
    let outcome_width = (max_outcome_len as u16 + 1).clamp(8, 16);
    let spacing = 1u16;
    // The download is what tells rows apart, so it keeps its room: a narrow
    // table leaves the outcome to the Inspector, and only a wide one shows
    // the library file too.
    let show_outcome = content[1].width >= 66;
    let show_target = content[1].width >= 110;
    let columns = 3 + u16::from(show_outcome) + u16::from(show_target);
    let fixed = state_width + size_width + if show_outcome { outcome_width } else { 0 };
    // The selected row's marker takes a column of its own on every row.
    let marker_width = 2u16;
    let flexible = content[1]
        .width
        .saturating_sub(marker_width + fixed + spacing * (columns - 1));
    let (source_width, target_width) = if show_target {
        let source = flexible * 11 / 20;
        (source, flexible - source)
    } else {
        (flexible, 0)
    };
    let visible_capacity = usize::from(content[1].height.saturating_sub(1));
    app.view_sizes().result_rows.set(visible_capacity);
    let (start, end) = visible_row_window(app.rows().len(), app.selected_row(), visible_capacity);
    let expand = app.expand_paths();

    let rows: Vec<Row> = app
        .rows()
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(idx, row)| {
            let is_selected = idx == app.selected_row();
            let style = row_style(row, is_selected);
            let mut cells = vec![
                Cell::from(Span::styled(
                    format!(" {} ", state_label(row)),
                    status_chip_style(row, is_selected),
                )),
                Cell::from(Span::styled(
                    trim_keep_name(
                        row_path(&row.source, app.downloads_root(), expand),
                        usize::from(source_width),
                    ),
                    style,
                )),
                Cell::from(Span::styled(format_bytes(row.size), style)),
            ];
            if show_outcome {
                cells.push(Cell::from(Span::styled(outcome_label(row), style)));
            }
            if show_target {
                cells.push(Cell::from(Span::styled(
                    trim_keep_name(
                        row_path(&row.target, app.media_root(), expand),
                        usize::from(target_width),
                    ),
                    style,
                )));
            }
            Row::new(cells).style(if is_selected {
                Theme::result_row_selected()
            } else {
                Theme::result_row()
            })
        })
        .collect();

    let mut widths = vec![
        Constraint::Length(state_width),
        Constraint::Length(source_width),
        Constraint::Length(size_width),
    ];
    let mut header = vec![
        Cell::from(Span::styled("STATE", Theme::title(false))),
        Cell::from(Span::styled("SOURCE", Theme::title(false))),
        Cell::from(Span::styled("SIZE", Theme::title(false))),
    ];
    if show_outcome {
        widths.push(Constraint::Length(outcome_width));
        header.push(Cell::from(Span::styled("OUTCOME", Theme::title(false))));
    }
    if show_target {
        widths.push(Constraint::Length(target_width));
        header.push(Cell::from(Span::styled("TARGET", Theme::title(false))));
    }
    // A marker and reverse video, so the selected row stands out on any
    // terminal's colours, not only by a shade of background.
    let table = Table::new(rows, widths)
        .header(Row::new(header))
        .style(Theme::panel())
        .column_spacing(spacing)
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(Theme::result_row_selected().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(app.selected_row().checked_sub(start));
    frame.render_stateful_widget(table, content[1], &mut state);
}

const HELP_TEXT: &str = "Keyboard map\n\nGlobal\nq close overlay or quit when idle\n? help\np toggle compact/full paths\nj/k or arrows move rows\nPgUp/PgDn move a page\ng/G or Home/End first and last row\n\nActions\ne edit downloads/media paths\nS scan for duplicates\nX or x clear cache and rescan (asks first)\nl confirm and link selected row\nL confirm and run link-all job\nc cancel the running scan or link\nR save report\nJ open JSON preview\n\nConfig editor\ntype directly into the active field\nenter moves forward or saves\ntab switches field\nEsc cancels";

fn render_help_overlay(frame: &mut Frame, app: &App) {
    let (area, lines) = dialog_for(frame.area(), 60, HELP_TEXT, 0);
    render_scrolling_overlay(frame, app, area, lines, " HELP ", " ? or Esc close ");
}

fn render_json_overlay(frame: &mut Frame, app: &App, json: &str) {
    let screen = frame.area();
    let (area, lines) = dialog_for(screen, screen.width.saturating_sub(4).min(140), json, 0);
    render_scrolling_overlay(frame, app, area, lines, " REPORT JSON ", " J or Esc close ");
}

/// An overlay that may be longer than the screen, scrolled by the app's
/// keys: the view tells the app how many lines it has and shows at once.
fn render_scrolling_overlay(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    lines: Vec<String>,
    title: &str,
    close: &str,
) {
    let shown = usize::from(area.height.saturating_sub(2));
    let sizes = app.view_sizes();
    sizes.overlay_rows.set(shown);
    sizes.overlay_lines.set(lines.len());
    let scroll = app.overlay_scroll().min(lines.len().saturating_sub(shown));
    let mut hint = vec![];
    if lines.len() > shown {
        hint.push(Span::styled(
            format!(
                " {}-{} of {}  j/k PgUp/PgDn scroll ",
                scroll + 1,
                (scroll + shown).min(lines.len()),
                lines.len()
            ),
            Theme::muted(),
        ));
    }
    hint.push(Span::styled(close.to_string(), Theme::title(false)));
    frame.render_widget(Clear, area);
    // The lines on screen, cut out here: Paragraph::scroll counts in u16,
    // which a report of a few thousand entries outgrows.
    let visible = lines[scroll..(scroll + shown).min(lines.len())].join("\n");
    let body = Paragraph::new(visible)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    title.to_string(),
                    Theme::title(true),
                )]))
                .title_bottom(Line::from(hint).right_aligned())
                .style(Theme::popup())
                .border_style(Theme::accent()),
        )
        .style(Theme::popup());
    frame.render_widget(body, area);
}

fn render_config_overlay(frame: &mut Frame, editor: &ConfigEditor) {
    let screen = frame.area();
    let width = 90u16
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    // The fields sit just inside the dialog's border.
    let inner_width = usize::from(width.saturating_sub(2));
    let status = editor
        .error
        .clone()
        .unwrap_or_else(|| "Enter saves on the second field. Esc cancels.".to_string());
    // The status box grows with its text, so a long error is read whole.
    let status_lines = wrap_text(&status, inner_width.saturating_sub(2));
    let tip = format!(
        "Absolute paths to existing folders; ~ is your home folder. Both are checked before they are saved to {}.",
        crate::config::config_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "the config file".to_string())
    );
    let tips = wrap_text(&tip, inner_width);
    let status_height = u16::try_from(status_lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let tips_height = u16::try_from(tips.len()).unwrap_or(u16::MAX);
    let height = (2 + 3 + 3 + status_height + tips_height).min(screen.height);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(status_height),
            Constraint::Min(0),
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

    let field = |title: &'static str, value: &str, active: bool| {
        Paragraph::new(value.to_string())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(if active {
                        Theme::accent()
                    } else {
                        Theme::line()
                    }),
            )
            .style(Theme::input(active))
    };
    frame.render_widget(
        field(
            "Downloads",
            &editor.downloads,
            editor.active_field == ConfigField::Downloads,
        ),
        sections[0],
    );
    frame.render_widget(
        field(
            "Media",
            &editor.media,
            editor.active_field == ConfigField::Media,
        ),
        sections[1],
    );

    let status = Paragraph::new(status_lines.join("\n"))
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

    let tips = Paragraph::new(tips.join("\n")).style(Theme::muted());
    frame.render_widget(tips, sections[3]);
}

fn render_confirm_link_overlay(frame: &mut Frame, app: &App, mode: ConfirmLinkMode) {
    let (title, body) = match mode {
        ConfirmLinkMode::All => (
            " LINK ALL CONFIRMATION ",
            format!(
                "Confirm link-all run\n\nThis links the planned rows the last scan showed, each to the library file shown. Files found since that scan are left alone, and so is a row whose files changed.\n\nRows to link: {}\nRows with errors: {}",
                app.reviewed_planned_pairs().len(),
                app.rows().iter().filter(|row| row.error.is_some()).count()
            ),
        ),
        ConfirmLinkMode::Selected => (
            " LINK SELECTED CONFIRMATION ",
            format!(
                "Confirm selected-row link\n\nThis will execute filesystem changes only for the selected row.\n\nSelected source: {}\nSelected outcome: {}\nRows still planned: {}",
                app.selected_entry()
                    .map(|entry| entry.source.clone())
                    .unwrap_or_else(|| "No selection".to_string()),
                app.selected_entry()
                    .map(outcome_label)
                    .unwrap_or_else(|| "N/A".to_string()),
                app.planned_actions(),
            ),
        ),
    };
    // The prompt sits on the border, so a small terminal that cuts the body
    // short still shows how to answer.
    let prompt = Line::from(vec![
        Span::styled(" y link ", Theme::title(true)),
        Span::raw(" "),
        Span::styled(" n cancel ", Theme::title(false)),
    ])
    .right_aligned();
    let (area, lines) = dialog_for(frame.area(), 76, &body, 0);
    frame.render_widget(Clear, area);
    let body = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(title, Theme::title(true))]))
                .title_bottom(prompt)
                .style(Theme::popup())
                .border_style(Theme::accent()),
        )
        .style(Theme::popup());
    frame.render_widget(body, area);
}

fn render_confirm_rescan_overlay(frame: &mut Frame) {
    let text = "Clear the cache and scan again?\n\nEvery file in both folders is read again, as on a first run; on a network drive that can take hours. S scans reading only what changed.";
    let (area, lines) = dialog_for(frame.area(), 64, text, 0);
    frame.render_widget(Clear, area);
    let prompt = Line::from(vec![
        Span::styled(" y clear and scan ", Theme::title(true)),
        Span::raw(" "),
        Span::styled(" n cancel ", Theme::title(false)),
    ])
    .right_aligned();
    let body = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    " RESCAN FROM SCRATCH ",
                    Theme::title(true),
                )]))
                .title_bottom(prompt)
                .style(Theme::popup())
                .border_style(Theme::accent()),
        )
        .style(Theme::popup());
    frame.render_widget(body, area);
}

fn render_blocking_notice_overlay(frame: &mut Frame, message: &str) {
    let text = format!("{message}\n\nPress q, Esc, or Enter to dismiss.");
    let (area, lines) = dialog_for(frame.area(), 70, &text, 0);
    frame.render_widget(Clear, area);
    let body = Paragraph::new(lines.join("\n"))
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
        .style(Theme::popup());
    frame.render_widget(body, area);
}

/// `text` wrapped to `width` columns at spaces; a word longer than a line is
/// cut across lines, and each line of `text` starts a new one, keeping its
/// indent on every line it wraps to, as JSON needs.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let body = paragraph.trim_start_matches(' ');
        let indent = (paragraph.len() - body.len()).min(width / 2);
        let first = lines.len();
        let width = width - indent;
        let mut line = String::new();
        for word in body.split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            let used = line.chars().count();
            if used > 0 && used + 1 + word.len() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            loop {
                let room = width.saturating_sub(line.chars().count());
                if word.len() <= room {
                    break;
                }
                if room == 0 {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                line.extend(word.drain(..room));
                lines.push(std::mem::take(&mut line));
            }
            line.extend(word);
        }
        lines.push(line);
        for line in &mut lines[first..] {
            line.insert_str(0, &" ".repeat(indent));
        }
    }
    lines
}

/// The area of a dialog for `text`: at most `max_width` columns wide and as
/// tall as the wrapped text needs plus `extra_rows`, within the screen and
/// centred on it. Returns the area and the wrapped lines.
fn dialog_for(screen: Rect, max_width: u16, text: &str, extra_rows: u16) -> (Rect, Vec<String>) {
    let width = max_width
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    let lines = wrap_text(text, usize::from(width.saturating_sub(2)));
    let wanted = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2 + extra_rows);
    let height = wanted.min(screen.height);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    };
    (area, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_rows_use_success_alt_chip() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            size: 42,
            status: "applied".to_string(),
            method: Some("symlink".to_string()),
            fallback_reason: Some("cross-device".to_string()),
            error: None,
            ..Default::default()
        };

        assert_eq!(status_chip_style(&row, false), Theme::chip_success_alt());
    }

    #[test]
    fn try_hardlink_rows_render_non_misleading_outcome() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            size: 42,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
            ..Default::default()
        };

        assert_eq!(outcome_label(&row), "TRY HARDLINK");
        assert_eq!(status_chip_style(&row, false), Theme::chip_pending());
    }

    #[test]
    fn planned_rows_use_pending_text_style() {
        let row = LinkReportEntry {
            source: "a".to_string(),
            target: "b".to_string(),
            size: 42,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
            ..Default::default()
        };

        assert_eq!(row_style(&row, false), Theme::muted());
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
            ..Default::default()
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
    fn the_sidebar_shows_both_thread_counts_without_wrapping() {
        let app = App::new(&crate::config::Config::default(), false);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(frame, &app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect();
        let row_of = |text: &str| rows.iter().position(|row| row.contains(text));
        let threads = row_of("Threads    16").expect("threads row");
        let full = row_of("Full reads 2").expect("full reads row");
        assert_eq!(full, threads + 1, "{}", rows.join("\n"));
        assert_eq!(
            row_of("Link mode"),
            Some(full + 1),
            "nothing wraps between them: {}",
            rows.join("\n")
        );
    }

    #[test]
    fn inspector_labels_show_selected_size() {
        let app = App::new(&crate::config::Config::default(), false);
        let combined = inspector_lines(&app)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(combined.contains("SELECTED SIZE"));
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
            freed_bytes: 10,
            total_saved_bytes: 10,
            skipped_by_min_age: 0,
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
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
                    ..Default::default()
                },
                LinkReportEntry {
                    source: "c".to_string(),
                    target: "d".to_string(),
                    size: 20,
                    status: "planned".to_string(),
                    method: None,
                    fallback_reason: None,
                    error: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        assert_eq!(format_bytes(app.saved_bytes()), "10 B");
        assert_eq!(format_bytes(app.potential_saved_bytes()), "20 B");
    }

    #[test]
    fn link_all_dialog_counts_only_rows_with_errors() {
        let mut app = App::new(&crate::config::Config::default(), false);
        let planned = |source: &str| LinkReportEntry {
            source: source.to_string(),
            target: "/media/x.mkv".to_string(),
            size: 10,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
            ..Default::default()
        };
        app.set_report_for_test(crate::protocol::RunReport {
            mode: "scan".to_string(),
            dry_run: true,
            strict: false,
            files_considered: 4,
            planned_actions: 2,
            applied_actions: 0,
            total_saved_bytes: 20,
            skipped_by_min_age: 0,
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
            warnings: crate::protocol::WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![planned("/downloads/a.mkv"), planned("/downloads/b.mkv")],
            ..Default::default()
        });
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_confirm_link_overlay(frame, &app, ConfirmLinkMode::All))
            .expect("draw");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(text.contains("Rows with errors: 0"), "{text}");
        assert!(!text.contains("Current mode"), "{text}");
    }

    #[test]
    fn the_link_prompt_survives_a_small_terminal() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.set_report_for_test(crate::protocol::RunReport {
            mode: "scan".to_string(),
            dry_run: true,
            strict: false,
            files_considered: 2,
            planned_actions: 1,
            applied_actions: 0,
            total_saved_bytes: 10,
            skipped_by_min_age: 0,
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
            warnings: crate::protocol::WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![LinkReportEntry {
                source: "/downloads/a.mkv".to_string(),
                target: "/media/a.mkv".to_string(),
                size: 10,
                status: "planned".to_string(),
                method: Some("try-hardlink".to_string()),
                fallback_reason: None,
                error: None,
                ..Default::default()
            }],
            ..Default::default()
        });
        for mode in [ConfirmLinkMode::All, ConfirmLinkMode::Selected] {
            let backend = ratatui::backend::TestBackend::new(80, 24);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            terminal
                .draw(|frame| render_confirm_link_overlay(frame, &app, mode))
                .expect("draw");
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("y link"), "{mode:?}: {text}");
            assert!(text.contains("n cancel"), "{mode:?}: {text}");
        }
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

    #[test]
    fn visible_row_window_shows_top_rows_initially() {
        assert_eq!(visible_row_window(10, 0, 4), (0, 4));
        assert_eq!(visible_row_window(10, 3, 4), (0, 4));
    }

    #[test]
    fn visible_row_window_scrolls_with_selection() {
        assert_eq!(visible_row_window(10, 4, 4), (1, 5));
        assert_eq!(visible_row_window(10, 7, 4), (4, 8));
    }

    #[test]
    fn visible_row_window_clamps_to_bounds() {
        assert_eq!(visible_row_window(10, 9, 4), (6, 10));
        assert_eq!(visible_row_window(3, 9, 4), (0, 3));
        assert_eq!(visible_row_window(3, 0, 0), (0, 0));
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;
    use crate::protocol::{RunReport, WarningSummary};

    #[test]
    fn a_trimmed_path_keeps_its_file_name() {
        assert_eq!(trim_keep_name("a/b.mkv", 20), "a/b.mkv");
        assert_eq!(
            trim_keep_name("Show.S01/Show.S01E05.mkv", 18),
            "…1/Show.S01E05.mkv"
        );
        // A name longer than the column keeps its start: title and episode.
        assert_eq!(
            trim_keep_name("Show/Show.Name.S01E05.1080p.WEB-DL-GROUP.mkv", 17),
            "Show.Name.S01E05…"
        );
        assert_eq!(trim_keep_name("abc", 0), "");
    }

    #[test]
    fn a_row_path_is_shown_below_its_folder_unless_expanded() {
        let root = std::path::Path::new("/srv/torrents");
        assert_eq!(
            row_path("/srv/torrents/a/b.mkv", Some(root), false),
            "a/b.mkv"
        );
        assert_eq!(
            row_path("/srv/torrents/a/b.mkv", Some(root), true),
            "/srv/torrents/a/b.mkv"
        );
        assert_eq!(
            row_path("/elsewhere/b.mkv", Some(root), false),
            "/elsewhere/b.mkv"
        );
        assert_eq!(
            row_path("/srv/torrents/a/b.mkv", None, false),
            "/srv/torrents/a/b.mkv"
        );
    }

    /// A scan of `episodes` downloads under a long downloads folder.
    pub(super) fn app_with_episodes(episodes: u32) -> App {
        let downloads = "/tmp/claude-0/-home-user/a-very-long-session-folder/downloads";
        let config = crate::config::Config {
            downloads: Some(std::path::PathBuf::from(downloads)),
            media: Some(std::path::PathBuf::from("/srv/media")),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entries = (1..=episodes)
            .map(|n| LinkReportEntry {
                source: format!(
                    "{downloads}/Show.Name.S01.1080p.WEB-DL.H.264-GROUP/Show.Name.S01E{n:02}.1080p.WEB-DL.H.264-GROUP.mkv"
                ),
                target: format!("/srv/media/TV/Show Name/Season 1/Show Name S01E{n:02}.mkv"),
                size: 1_500_000_000,
                status: "planned".to_string(),
                method: Some("try-hardlink".to_string()),
                fallback_reason: None,
                error: None,
                ..Default::default()
            })
            .collect();
        app.set_report_for_test(RunReport {
            mode: "scan".to_string(),
            planned_actions: episodes as usize,
            warnings: WarningSummary::default(),
            entries,
            ..RunReport::default()
        });
        app
    }

    pub(super) fn screen(app: &App, cols: u16, rows: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(cols, rows);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rows_of_a_season_can_be_told_apart() {
        let app = app_with_episodes(3);
        let text = screen(&app, 120, 40);
        for n in 1..=3 {
            assert!(text.contains(&format!("S01E0{n}")), "episode {n}:\n{text}");
        }
        assert!(text.contains("SIZE") && text.contains("1.4 GiB"), "{text}");
        assert!(
            row_lines(&text)
                .iter()
                .all(|row| !row.contains("a-very-long-session-folder")),
            "the downloads folder is not repeated in the rows:\n{text}"
        );
    }

    /// The table's row lines: those with a row's state chip, which the
    /// column pads with spaces (the PLANNED card's title is followed by its
    /// border).
    fn row_lines(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| line.contains(" PLANNED  "))
            .collect()
    }

    #[test]
    fn a_wide_table_shows_the_library_file_and_p_shows_full_paths() {
        let mut app = app_with_episodes(2);
        let text = screen(&app, 400, 40);
        let rows = row_lines(&text);
        assert_eq!(rows.len(), 2, "{text}");
        assert!(text.contains("TARGET"), "{text}");
        assert!(
            rows[1].contains("TV/Show Name/Season 1/Show Name S01E02.mkv"),
            "{text}"
        );
        assert!(!rows[1].contains("/srv/media"), "below its folder: {text}");

        app.toggle_path_mode_for_test();
        let text = screen(&app, 400, 40);
        let rows = row_lines(&text);
        assert!(
            rows[1].contains("/srv/media/TV/Show Name/Season 1"),
            "{text}"
        );
        assert!(rows[1].contains("/downloads/Show.Name.S01.1080p"), "{text}");
    }
}

#[cfg(test)]
mod layout_tests {
    use super::row_tests::{app_with_episodes, screen};
    use super::*;

    #[test]
    fn a_short_terminal_keeps_counts_log_and_help_and_drops_the_wordmark() {
        let app = app_with_episodes(12);
        for (cols, rows) in [(60, 20), (80, 24)] {
            let text = screen(&app, cols, rows);
            assert!(!text.contains('█'), "{cols}x{rows}: no wordmark\n{text}");
            assert!(text.contains("PLANNED 12"), "{cols}x{rows}: counts\n{text}");
            assert!(text.contains("Run Log"), "{cols}x{rows}: log\n{text}");
            assert!(text.contains("? help"), "{cols}x{rows}: footer\n{text}");
            for n in 1..=3 {
                assert!(
                    text.contains(&format!("S01E0{n}")),
                    "{cols}x{rows}: rows told apart\n{text}"
                );
            }
        }
    }

    #[test]
    fn the_sidebar_and_inspector_give_way_on_a_narrow_terminal() {
        let app = app_with_episodes(3);
        let wide = screen(&app, 120, 40);
        assert!(wide.contains("Workspace") && wide.contains("Inspector"));
        let medium = screen(&app, 80, 24);
        assert!(!medium.contains("Workspace") && medium.contains("Inspector"));
        let narrow = screen(&app, 60, 20);
        assert!(!narrow.contains("Workspace") && !narrow.contains("Inspector"));
    }

    #[test]
    fn metric_titles_are_never_cut() {
        let app = app_with_episodes(3);
        for (cols, rows) in [(100, 30), (120, 40), (200, 50)] {
            let text = screen(&app, cols, rows);
            for title in [" FILES ", " PLANNED ", " APPLIED ", " SAVED ", " TO SAVE "] {
                assert!(text.contains(title), "{cols}x{rows}: {title}\n{text}");
            }
        }
    }

    #[test]
    fn a_dialog_is_as_tall_as_its_text() {
        let screen_area = Rect::new(0, 0, 120, 40);
        let (area, lines) = dialog_for(screen_area, 70, "One short line.", 0);
        assert_eq!(lines, ["One short line."]);
        assert_eq!((area.width, area.height), (70, 3));
        // Never past the screen.
        let (area, _) = dialog_for(Rect::new(0, 0, 30, 5), 70, &"word ".repeat(80), 0);
        assert!(area.width <= 30 && area.height <= 5, "{area:?}");
    }

    #[test]
    fn text_wraps_at_spaces_and_cuts_a_word_too_long_for_a_line() {
        assert_eq!(wrap_text("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_text("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(
            wrap_text("{\n  \"key\": \"one two\"\n}", 12),
            ["{", "  \"key\":", "  \"one two\"", "}"]
        );
        assert_eq!(wrap_text("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        for line in wrap_text("media path is inside /a/very/long/path/that/goes/on", 12) {
            assert!(line.chars().count() <= 12, "{line}");
        }
    }

    #[test]
    fn the_footer_keeps_quit_and_help_however_narrow() {
        for width in [20u16, 40, 60, 80, 120] {
            let line = footer_line(width).to_string();
            assert!(
                line.contains("q quit") && line.contains("? help"),
                "{width}: {line}"
            );
            assert!(
                line.chars().count() <= usize::from(width).max(17),
                "{width}: {line}"
            );
        }
        assert!(footer_line(120).to_string().contains("j/k rows"));
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::row_tests::{app_with_episodes, screen};
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key_for_test(code, KeyModifiers::NONE);
    }

    #[test]
    fn the_selected_row_carries_a_marker() {
        let mut app = app_with_episodes(3);
        press(&mut app, KeyCode::Down);
        let text = screen(&app, 80, 24);
        let marked: Vec<_> = text.lines().filter(|line| line.contains('▶')).collect();
        assert_eq!(marked.len(), 1, "{text}");
        assert!(marked[0].contains("S01E02"), "{text}");
    }

    #[test]
    fn paging_keys_move_a_screen_and_to_either_end() {
        let mut app = app_with_episodes(50);
        screen(&app, 80, 24);
        let page = app.view_sizes().result_rows.get();
        assert!(page > 1);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.selected_row(), page);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected_row(), 49);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.selected_row(), 49 - page);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.selected_row(), 0);
        press(&mut app, KeyCode::End);
        assert_eq!(app.selected_row(), 49);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected_row(), 0);
    }

    #[test]
    fn the_json_preview_scrolls_to_its_last_entry_and_j_does_not_close_it() {
        let mut app = app_with_episodes(40);
        press(&mut app, KeyCode::Char('J'));
        let text = screen(&app, 80, 24);
        assert!(!text.contains("S01E40"), "the start first:\n{text}");
        press(&mut app, KeyCode::Char('j'));
        assert!(
            matches!(app.overlay(), Overlay::JsonPreview(_)),
            "j scrolls"
        );
        press(&mut app, KeyCode::End);
        let text = screen(&app, 80, 24);
        assert!(
            text.contains("S01E40") && text.lines().any(|line| line.contains("│  ]")),
            "the end of the report:\n{text}"
        );
        press(&mut app, KeyCode::Char('J'));
        assert!(matches!(app.overlay(), Overlay::None));
    }

    #[test]
    fn help_scrolls_to_its_end_on_a_small_terminal() {
        let mut app = app_with_episodes(1);
        press(&mut app, KeyCode::Char('?'));
        let text = screen(&app, 80, 24);
        assert!(!text.contains("Esc cancels"), "{text}");
        press(&mut app, KeyCode::Char('G'));
        let text = screen(&app, 80, 24);
        assert!(text.contains("Esc cancels"), "{text}");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.overlay(), Overlay::None));
    }
}

#[cfg(test)]
mod rescan_tests {
    use super::row_tests::{app_with_episodes, screen};
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn the_rescan_question_fits_a_small_terminal() {
        let mut app = app_with_episodes(1);
        app.handle_key_for_test(KeyCode::Char('x'), KeyModifiers::NONE);
        let text = screen(&app, 80, 24);
        assert!(text.contains("RESCAN FROM SCRATCH"), "{text}");
        assert!(
            text.contains("y clear and scan") && text.contains("n cancel"),
            "{text}"
        );
    }
}

#[cfg(test)]
mod editor_tests {
    use super::row_tests::screen;
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn the_path_editor_names_the_config_file_and_shows_a_long_error_whole() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.handle_key_for_test(KeyCode::Char('e'), KeyModifiers::NONE);
        let long =
            "/tmp/claude-0/-home-user/fd53d022-d0e1-4c5b-9a7e-1f2e3d4c5b6a/downloads/library";
        for c in long.chars() {
            app.handle_key_for_test(KeyCode::Char(c), KeyModifiers::NONE);
        }
        app.handle_key_for_test(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key_for_test(KeyCode::Enter, KeyModifiers::NONE);

        let text = screen(&app, 80, 24);
        assert!(text.contains("config.json"), "{text}");
        assert!(!text.contains("repo config file"), "{text}");
        let error_lines = text
            .lines()
            .skip_while(|line| !line.contains("Status"))
            .take_while(|line| !line.contains('└'))
            .collect::<Vec<_>>()
            .join("");
        assert!(error_lines.contains("Downloads: cannot use"), "{text}");
        assert!(
            error_lines.contains("library"),
            "the end of the path: {text}"
        );
    }
}

#[cfg(test)]
mod long_overlay_tests {
    use super::row_tests::{app_with_episodes, screen};
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn a_report_longer_than_65535_lines_scrolls_to_its_end() {
        let mut app = app_with_episodes(7000);
        app.handle_key_for_test(KeyCode::Char('J'), KeyModifiers::NONE);
        screen(&app, 80, 24);
        assert!(app.view_sizes().overlay_lines.get() > usize::from(u16::MAX));
        app.handle_key_for_test(KeyCode::End, KeyModifiers::NONE);
        let text = screen(&app, 80, 24);
        assert!(text.contains("S01E7000"), "the last entry:\n{text}");
        assert!(text.lines().any(|line| line.contains("│  ]")), "{text}");
    }
}

#[cfg(test)]
mod real_terminal_tests {
    use super::row_tests::{app_with_episodes, screen};
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn a_narrow_header_keeps_the_space_counts_whole() {
        let mut app = app_with_episodes(3);
        app.set_report_for_test(crate::protocol::RunReport {
            mode: "scan".to_string(),
            files_considered: 123_456,
            planned_actions: 3,
            total_saved_bytes: 4_500_000_000,
            ..Default::default()
        });
        let text = screen(&app, 60, 20);
        let counts = text.lines().nth(1).expect("the counts line");
        assert!(counts.contains("TO SAVE 4.2 GiB"), "{text}");
        assert!(counts.contains("PLANNED 3"), "{text}");
        assert!(!counts.contains("FILES"), "FILES gives way first: {counts}");
        let wide = screen(&app, 80, 24);
        assert!(
            wide.lines()
                .nth(1)
                .is_some_and(|line| line.contains("FILES")),
            "{wide}"
        );
    }

    #[test]
    fn a_narrow_results_line_drops_the_repeated_counts_rather_than_cut_them() {
        let app = app_with_episodes(3);
        let text = screen(&app, 60, 20);
        let line = text
            .lines()
            .find(|line| line.contains("READY TO REVIEW") && line.contains("rows"))
            .expect("the results line");
        assert!(!line.contains("files"), "{line}");
        let wide = screen(&app, 200, 50);
        assert!(
            wide.contains("3 rows  |  0 files  |  3 planned  |  0 applied"),
            "{wide}"
        );
    }

    #[test]
    fn the_path_editor_has_no_blank_rows_under_its_tip() {
        let mut app = App::new(&crate::config::Config::default(), false);
        app.handle_key_for_test(KeyCode::Char('e'), KeyModifiers::NONE);
        let text = screen(&app, 80, 24);
        let lines: Vec<&str> = text.lines().collect();
        let tip_end = lines
            .iter()
            .position(|line| line.contains("config.json"))
            .expect("the tip's last line");
        // The editor's bottom-left corner, in its own column, right below.
        assert_eq!(
            lines[tip_end + 1].chars().nth(2),
            Some('└'),
            "the tip's last line sits on the border:\n{text}"
        );
    }
}
