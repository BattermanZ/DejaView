//! The dashboard: the logo and the big number on top, the pairs below with
//! the link between each download and its library file drawn in the middle,
//! the selected pair told in full under them, then the newest log lines and
//! the keys.

mod dialogs;
mod header;
mod pairs;

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::tui::app::{App, Overlay};
use crate::tui::layout;
use crate::tui::theme::Theme;

pub(crate) use crate::human::format_size;

/// A count with its thousands grouped: `3,412`.
fn group_digits(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
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

/// The cells `text` takes on screen: two for a wide character, such as a
/// CJK one, none for a combining mark.
fn width_of(text: &str) -> usize {
    Span::raw(text).width()
}

fn char_width(ch: char) -> usize {
    let mut buffer = [0; 4];
    width_of(ch.encode_utf8(&mut buffer))
}

/// The longest end of `text` that fits in `room` cells.
fn tail_fitting(text: &str, room: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        used += char_width(ch);
        if used > room {
            break;
        }
        start = index;
    }
    &text[start..]
}

/// How many of `chars` fit in `room` cells, from the start.
fn chars_fitting(chars: &[char], room: usize) -> usize {
    let mut used = 0;
    chars
        .iter()
        .take_while(|ch| {
            used += char_width(**ch);
            used <= room
        })
        .count()
}

/// Cut `path` to `width` cells, keeping its file name, which is what tells
/// one episode from the next: the folders give way first, whole, from the
/// left. A file name too long on its own keeps its start, where a release
/// names its title and episode; its end is a release group the rows share.
fn trim_keep_name(path: &str, width: usize) -> String {
    if width_of(path) <= width {
        return path.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if width_of(name) < width {
        // The folders that fit whole, so no folder's name shows cut.
        let tail = tail_fitting(path, width - 1);
        let tail = match tail.find('/') {
            Some(slash) if slash > 0 => &tail[slash..],
            _ => tail,
        };
        return format!("…{tail}");
    }
    cut(name, width)
}

/// `text` cut to `width` cells, an ellipsis standing for the rest.
fn cut(text: &str, width: usize) -> String {
    if width_of(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut start: String = chars[..chars_fitting(&chars, width - 1)].iter().collect();
    start.push('…');
    start
}

/// `text` cut to `width` cells and padded with spaces to it.
fn pad(text: &str, width: usize) -> String {
    let text = cut(text, width);
    let fill = width.saturating_sub(width_of(&text));
    format!("{text}{}", " ".repeat(fill))
}

/// `text` cut to `width` cells and right-aligned in it.
fn pad_left(text: &str, width: usize) -> String {
    let text = cut(text, width);
    let fill = width.saturating_sub(width_of(&text));
    format!("{}{text}", " ".repeat(fill))
}

/// The cells a line takes on screen.
fn line_width(line: &Line) -> usize {
    line.width()
}

/// `text` with a capital first letter and a full stop, as the engine's
/// reasons ("symlink-only mode enabled") read in a sentence.
fn sentence(text: &str) -> String {
    let text = text.trim().trim_end_matches('.');
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// What a person should know about every symlink DejaView makes.
const SYMLINK_BREAKS: &str =
    "A symlink breaks if its library file is renamed, upgraded or deleted.";

/// `text` wrapped to `width` cells at spaces; a word longer than a line is
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
            let used = width_of(&line);
            let word_width: usize = word.iter().map(|ch| char_width(*ch)).sum();
            if used > 0 && used + 1 + word_width > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            loop {
                let room = width.saturating_sub(width_of(&line));
                let fits = chars_fitting(&word, room);
                if fits == word.len() {
                    break;
                }
                if fits == 0 && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                // A character wider than a whole line still goes somewhere.
                line.extend(word.drain(..fits.max(1)));
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

/// The rows of a list of `total` that show in `available` rows with row
/// `selected` among them: from the top, until the selection would pass the
/// bottom, then following it.
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

/// How a key looks in the footer.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Look {
    /// The next step: on an orange chip.
    Primary,
    /// The one thing to do while a run goes: on a grey chip.
    Chip,
    Plain,
}

/// A key the footer offers, and how much it matters when the terminal is
/// too narrow for all: 0 always stays, higher gives way first.
struct Key {
    key: &'static str,
    label: String,
    drop_order: u8,
    look: Look,
}

fn key(key: &'static str, label: &str, drop_order: u8) -> Key {
    Key {
        key,
        label: label.to_string(),
        drop_order,
        look: Look::Plain,
    }
}

/// The key for the next step: it gives way only to `q` and `?`.
fn primary(key: &'static str, label: String) -> Key {
    Key {
        key,
        label,
        drop_order: 1,
        look: Look::Primary,
    }
}

/// The keys that do something on this screen, the next step first. `q` and
/// `?` always stay, since Help lists the rest.
fn footer_keys(app: &App) -> Vec<Key> {
    let quit = key("q", "quit", 0);
    let help = key("?", "help", 0);
    if app.is_running() {
        let mut keys = vec![Key {
            look: Look::Chip,
            ..key("c", "cancel", 1)
        }];
        if !app.scan_running() {
            keys.push(key("j/k", "rows", 3));
        }
        keys.extend([help, quit]);
        return keys;
    }
    if !app.has_paths() {
        return vec![primary("e", "choose folders".to_string()), help, quit];
    }
    let planned = app.reviewed_planned_pairs().len();
    let mut keys = Vec::new();
    if planned > 0 {
        keys.push(primary("L", format!("link all {planned}")));
        keys.push(key("l", "link this", 2));
        keys.push(key("S", "scan", 3));
    } else if app.has_report() {
        keys.push(primary("S", "scan again".to_string()));
    } else {
        keys.push(primary("S", "scan".to_string()));
    }
    keys.push(key("X", "rescan all", 4));
    keys.push(key("e", "folders", 3));
    if !app.rows().is_empty() {
        keys.push(key("p", "full paths", 5));
        keys.push(key("j/k", "rows", 4));
    }
    if app.has_report() {
        keys.push(key("R", "save report", 5));
    }
    keys.extend([help, quit]);
    keys
}

/// The keys that fit in `width`, dropping the least needed first.
fn footer_line(app: &App, width: u16) -> Line<'static> {
    let keys = footer_keys(app);
    let cost = |key: &Key| {
        let chip = if key.look == Look::Plain { 1 } else { 4 };
        key.key.chars().count() + chip + key.label.chars().count() + 3
    };
    let mut shown = vec![true; keys.len()];
    let total = |shown: &[bool]| -> usize {
        1 + keys
            .iter()
            .zip(shown)
            .filter(|(_, shown)| **shown)
            .map(|(key, _)| cost(key))
            .sum::<usize>()
    };
    for level in (1..=5).rev() {
        if total(&shown) <= usize::from(width) {
            break;
        }
        for (index, key) in keys.iter().enumerate() {
            if key.drop_order == level {
                shown[index] = false;
            }
        }
    }
    let mut spans = vec![Span::raw(" ")];
    for (key, _) in keys.iter().zip(&shown).filter(|(_, shown)| **shown) {
        match key.look {
            Look::Primary | Look::Chip => {
                let chip = if key.look == Look::Primary {
                    Theme::key_primary()
                } else {
                    Theme::key_chip()
                };
                spans.push(Span::styled(format!(" {} ", key.key), chip));
                spans.push(Span::styled(format!(" {}", key.label), Theme::text()));
            }
            Look::Plain => {
                spans.push(Span::styled(key.key.to_string(), Theme::key()));
                spans.push(Span::styled(format!(" {}", key.label), Theme::muted()));
            }
        }
        spans.push(Span::raw("   "));
    }
    spans.pop();
    Line::from(spans)
}

/// The newest log lines that fit, oldest first, one row each: a line wider than
/// the pane is cut with an ellipsis, so the newest line is never pushed out.
/// The log file keeps every line whole.
fn newest_log_lines(lines: &[String], width: u16, height: u16) -> Vec<Line<'static>> {
    const LABEL: &str = "Log  ";
    let width = usize::from(width).saturating_sub(LABEL.len());
    let start = lines.len().saturating_sub(usize::from(height));
    let newest = lines.len().saturating_sub(1);
    lines[start..]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let style = if start + offset == newest {
                Theme::muted()
            } else {
                Theme::faint()
            };
            let label = if offset == 0 { LABEL } else { "     " };
            Line::from(vec![
                Span::styled(label, Theme::faint()),
                Span::styled(cut(line, width), style),
            ])
        })
        .collect()
}

/// A rule across `area`'s first row.
fn rule(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "─".repeat(usize::from(area.width)),
        Theme::rule(),
    ));
    frame.render_widget(Paragraph::new(line), Rect { height: 1, ..area });
}

/// Split off `rows` rows at the top of `area`.
fn take_top(area: Rect, rows: u16) -> (Rect, Rect) {
    let rows = rows.min(area.height);
    (
        Rect {
            height: rows,
            ..area
        },
        Rect {
            y: area.y + rows,
            height: area.height - rows,
            ..area
        },
    )
}

/// `area` less `columns` columns on the left and right.
fn inset(area: Rect, columns: u16) -> Rect {
    let columns = columns.min(area.width / 2);
    Rect {
        x: area.x + columns,
        width: area.width - 2 * columns,
        ..area
    }
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Theme::canvas()), area);
    let l = layout::dashboard(area);

    if l.compact_header {
        header::render_compact(frame, l.header, app);
    } else {
        header::render_full(frame, l.header, app);
    }

    // The details panel tells the selected pair; with none, or while a scan
    // shows its progress, its rows go to the body.
    let mut body = l.body;
    let mut details = l.details;
    if (app.rows().is_empty() || app.scan_running())
        && let Some(rows) = details.take()
    {
        body = body.union(rows);
    }
    if app.scan_running() {
        pairs::render_scan(frame, body, app);
    } else if app.rows().is_empty() {
        pairs::render_empty(frame, body, app);
    } else {
        pairs::render_pairs(frame, body, app);
    }
    if let Some(area) = details {
        pairs::render_details(frame, area, app);
    }

    let log = inset(l.log, 2);
    frame.render_widget(
        Paragraph::new(newest_log_lines(&app.log_lines(), log.width, log.height)),
        log,
    );

    let keys = if l.footer_rule {
        rule(frame, l.footer);
        take_top(l.footer, 1).1
    } else {
        l.footer
    };
    frame.render_widget(Paragraph::new(footer_line(app, keys.width)), keys);

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => dialogs::render_help(frame, app),
        Overlay::JsonPreview(json) => dialogs::render_json(frame, app, json),
        Overlay::ConfigEditor(editor) => dialogs::render_config_editor(frame, app, editor),
        Overlay::ConfirmLink(mode) => dialogs::render_confirm_link(frame, app, *mode),
        Overlay::ConfirmRescan => dialogs::render_confirm_rescan(frame),
        Overlay::BlockingNotice(message) => dialogs::render_notice(frame, app, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trimmed_path_keeps_its_file_name() {
        let path = "/downloads/Show.Name.S01.1080p.WEB-DL/Show.Name.S01E05.1080p.WEB-DL.mkv";
        // The folders give way whole, from the left.
        assert_eq!(
            trim_keep_name(path, 40),
            "…/Show.Name.S01E05.1080p.WEB-DL.mkv"
        );
        assert_eq!(
            trim_keep_name(path, 70),
            "…/Show.Name.S01.1080p.WEB-DL/Show.Name.S01E05.1080p.WEB-DL.mkv"
        );
        assert_eq!(trim_keep_name(path, 17), "Show.Name.S01E05…");
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
    }

    #[test]
    fn wide_characters_take_two_cells_in_a_column() {
        let name = "葬送のフリーレン 01.mkv";
        assert_eq!(width_of(name), 23);
        assert_eq!(width_of(&pad(name, 30)), 30);
        assert_eq!(width_of(&pad_left(name, 30)), 30);
        let cut_name = cut(name, 10);
        assert!(width_of(&cut_name) <= 10, "{cut_name}");
        assert!(cut_name.ends_with('…'), "{cut_name}");
        let trimmed = trim_keep_name(&format!("/dl/{name}"), 12);
        assert!(width_of(&trimmed) <= 12, "{trimmed}");
        for line in wrap_text("葬送のフリーレン 葬送のフリーレン", 9) {
            assert!(width_of(&line) <= 9, "{line}");
        }
    }

    #[test]
    fn an_engine_reason_reads_as_a_sentence() {
        assert_eq!(
            sentence("symlink-only mode enabled"),
            "Symlink-only mode enabled."
        );
        assert_eq!(
            sentence("cross-device: no hardlink."),
            "Cross-device: no hardlink."
        );
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn counts_group_their_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(3_412), "3,412");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn text_is_cut_and_padded_to_its_column() {
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad("abcdef", 4), "abc…");
    }

    #[test]
    fn visible_row_window_follows_the_selection() {
        assert_eq!(visible_row_window(10, 0, 4), (0, 4));
        assert_eq!(visible_row_window(10, 5, 4), (2, 6));
        assert_eq!(visible_row_window(3, 2, 10), (0, 3));
        assert_eq!(visible_row_window(0, 0, 4), (0, 0));
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
        let app = test_support::app_with_episodes(3);
        for width in [20u16, 40, 60, 80, 120] {
            let line = footer_line(&app, width).to_string();
            assert!(
                line.contains("q quit") && line.contains("? help"),
                "{width}: {line}"
            );
            assert!(
                line.chars().count() <= usize::from(width).max(17),
                "{width}: {line}"
            );
        }
        let wide = footer_line(&app, 120).to_string();
        assert!(wide.contains(" L  link all 3"), "{wide}");
        assert!(wide.contains("j/k rows"), "{wide}");
    }

    #[test]
    fn the_footer_offers_the_next_step() {
        let first_run = App::new(&crate::config::Config::default(), false);
        let line = footer_line(&first_run, 120).to_string();
        assert!(line.contains(" e  choose folders"), "{line}");
        assert!(!line.contains("link"), "{line}");

        let linked = test_support::app_with_link_report();
        let line = footer_line(&linked, 120).to_string();
        assert!(line.contains(" S  scan again"), "{line}");
        assert!(!line.contains("link all"), "{line}");
    }
}

/// Apps and screens the view's tests draw.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::protocol::{EntryStatus, LinkMethod, Mode, WarningSummary};
    use crate::tui::model::{LinkReportEntry, RunReport};

    /// A scan of `episodes` downloads under a long downloads folder.
    pub(crate) fn app_with_episodes(episodes: u32) -> App {
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
                status: EntryStatus::Planned,
                method: Some(LinkMethod::TryHardlink),
                fallback_reason: None,
                error: None,
                ..Default::default()
            })
            .collect();
        app.set_report_for_test(RunReport {
            mode: Mode::Scan,
            planned_actions: episodes as usize,
            total_saved_bytes: u64::from(episodes) * 1_500_000_000,
            warnings: WarningSummary::default(),
            entries,
            ..RunReport::default()
        });
        app
    }

    /// A link run over three pairs: a hardlink, a symlink and a failure.
    pub(crate) fn app_with_link_report() -> App {
        let config = crate::config::Config {
            downloads: Some(std::path::PathBuf::from("/downloads")),
            media: Some(std::path::PathBuf::from("/media")),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entry = |name: &str, status: EntryStatus, method: LinkMethod| LinkReportEntry {
            source: format!("/downloads/{name}/{name}.mkv"),
            target: format!("/media/Movies/{name}.mkv"),
            size: 2_000_000_000,
            status,
            method: Some(method),
            ..Default::default()
        };
        let mut symlink = entry("Beta.2021", EntryStatus::Applied, LinkMethod::Symlink);
        symlink.fallback_reason = Some("different filesystems".to_string());
        let mut failed = entry("Gamma.2022", EntryStatus::Failed, LinkMethod::Hardlink);
        failed.error = Some("Permission denied (os error 13)".to_string());
        app.set_report_for_test(RunReport {
            mode: Mode::Link,
            files_considered: 9,
            applied_actions: 2,
            symlinked: 1,
            freed_bytes: 4_000_000_000,
            total_saved_bytes: 4_000_000_000,
            entries: vec![
                entry("Alpha.2020", EntryStatus::Applied, LinkMethod::Hardlink),
                symlink,
                failed,
            ],
            ..RunReport::default()
        });
        app
    }

    /// The screen `app` draws at `cols` by `rows`, as text.
    pub(crate) fn screen(app: &App, cols: u16, rows: u16) -> String {
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
}
