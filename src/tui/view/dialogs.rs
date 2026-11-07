//! The dialogs: help, the report as JSON, the folder editor and the
//! questions asked before a link or a rescan. Each keeps its keys on its
//! bottom border, so a terminal too small for the whole text still shows how
//! to answer.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::pairs::{LinkKind, Tally};
use super::{SYMLINK_BREAKS, format_size, pad, sentence, trim_keep_name, wrap_text};
use crate::tui::app::{App, Click, ConfigEditor, ConfigField, ConfirmLinkMode, LinkCheck};
use crate::tui::theme::Theme;

/// A dialog's frame: rounded, in the focus colour, its title on top and its
/// keys on the bottom border, the first the answer the dialog is there for
/// when `primary`.
fn frame_block(title: &str, keys: &[(&str, &str)], primary: bool) -> Block<'static> {
    let mut spans = Vec::new();
    for (index, (key, label)) in keys.iter().enumerate() {
        let style = if index == 0 && primary {
            Theme::key_primary()
        } else {
            Theme::key_chip()
        };
        // `border_key_cells` counts this padding: change both together.
        spans.push(Span::styled(format!(" {key} "), style));
        spans.push(Span::styled(format!(" {label} "), Theme::popup()));
    }
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::on_popup(Theme::focus()))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Theme::popup_strong(),
        )))
        .title_bottom(Line::from(spans).right_aligned())
        .style(Theme::popup())
}

/// The cells a key and its label take on a dialog's border, as
/// `frame_block` pads them: ` key ` then ` label `.
fn border_key_cells(key: &str, label: &str) -> usize {
    super::width_of(key) + super::width_of(label) + 4
}

/// Make the keys `frame_block` puts on the bottom border of a dialog drawn
/// in `area` clickable, each with its label. They sit right-aligned between
/// the border's corners; keys too wide for that room lose their left end, as
/// ratatui draws them, so each spot stays under the key it shows.
fn add_key_clicks(app: &App, area: Rect, keys: &[(&str, &str)]) {
    let inner_left = i32::from(area.x) + 1;
    let inner_right = i32::from(area.right()) - 1;
    let widths: Vec<i32> = keys
        .iter()
        .map(|(key, label)| i32::try_from(border_key_cells(key, label)).unwrap_or(i32::MAX / 4))
        .collect();
    let mut x = inner_right - widths.iter().sum::<i32>();
    let y = area.bottom().saturating_sub(1);
    for ((key, _), width) in keys.iter().zip(widths) {
        let (start, end) = (x.max(inner_left), (x + width).min(inner_right));
        x += width;
        if let (Some(code), Ok(start), Ok(width)) = (
            super::key_code(key),
            u16::try_from(start),
            u16::try_from(end - start),
        ) && width > 0
        {
            let spot = Rect {
                x: start,
                y,
                width,
                height: 1,
            };
            app.view_sizes().add_click(spot, Click::Key(code));
        }
    }
}

/// A dialog `width` columns wide (within the screen) and as tall as `rows`
/// lines of text, centred.
fn centred(screen: Rect, width: u16, rows: usize) -> Rect {
    let width = width
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .min(screen.height);
    Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    }
}

/// The text room inside a dialog of `width`: its border and a column of
/// padding on each side.
fn text_width(width: u16) -> usize {
    usize::from(width.saturating_sub(4))
}

/// Push the screen behind a dialog back, so the dialog stands in front.
fn dim_behind(frame: &mut Frame) {
    for cell in &mut frame.buffer_mut().content {
        cell.fg = Theme::behind_dialog(cell.fg, true);
        cell.bg = Theme::behind_dialog(cell.bg, false);
    }
}

/// Draw `lines` in a dialog, a row of padding above and below them, over
/// the screen pushed back.
fn show(frame: &mut Frame, area: Rect, block: Block<'static>, lines: Vec<Line<'static>>) {
    dim_behind(frame);
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = Rect {
        x: inner.x + 1,
        y: inner.y + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(2),
    };
    frame.render_widget(Paragraph::new(lines), text);
}

fn plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

/// The area of a dialog for `text`: at most `max_width` columns wide and as
/// tall as the wrapped text needs, within the screen and centred on it.
/// Returns the area and the wrapped lines.
fn dialog_for(screen: Rect, max_width: u16, text: &str) -> (Rect, Vec<String>) {
    let width = max_width
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    let lines = wrap_text(text, text_width(width));
    (centred(screen, width, lines.len()), lines)
}

/// What each drawing in the link column means, then the keys.
fn help_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "What the link column shows",
            Theme::popup_strong(),
        )),
        Line::raw(""),
    ];
    let legend = [
        (
            LinkKind::Planned,
            "Planned",
            "not linked yet: becomes a hardlink, or a symlink across filesystems",
        ),
        (
            LinkKind::PlannedSymlink,
            "Planned",
            "not linked yet, and to become a symlink: the two are on different filesystems, or the link mode is symlink-only",
        ),
        (
            LinkKind::Hardlink,
            "Hardlink",
            "one file on disk under both names; the torrent keeps seeding",
        ),
        (
            LinkKind::Symlink,
            "Symlink",
            "points at the library file; breaks if that file is renamed, upgraded or deleted",
        ),
        (
            LinkKind::Already,
            "Already linked",
            "the two names already are one file",
        ),
        (
            LinkKind::Failed,
            "Failed",
            "not linked; the details under the pairs say why",
        ),
        (
            LinkKind::Skipped,
            "Left alone",
            "on purpose; the details say why",
        ),
    ];
    let indent = 9 + 2 + 15;
    for (kind, name, meaning) in legend {
        let style = kind.style();
        let wrapped = wrap_text(meaning, width.saturating_sub(indent).max(12));
        for (index, part) in wrapped.into_iter().enumerate() {
            let mut spans = if index == 0 {
                vec![
                    Span::styled(kind.drawing(true), Theme::on_popup(style)),
                    Span::raw("  "),
                    Span::styled(pad(name, 15), Theme::popup_strong()),
                ]
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.push(Span::styled(part, Theme::popup_muted()));
            lines.push(Line::from(spans));
        }
    }
    let section = |title: &str| Line::from(Span::styled(title.to_string(), Theme::popup_strong()));
    let key_line = |key: &str, meaning: &str| {
        Line::from(vec![
            Span::styled(pad(key, 11), Theme::popup_key()),
            Span::styled(meaning.to_string(), Theme::popup()),
        ])
    };
    lines.push(Line::raw(""));
    lines.push(section("Keys"));
    lines.push(Line::raw(""));
    for (key, meaning) in [
        ("S", "scan, reading only what changed"),
        ("X  x", "clear the cache and scan everything (asks first)"),
        ("l", "link the selected pair (asks first)"),
        ("L", "link every planned pair (asks first)"),
        ("c", "cancel a running scan or link"),
        ("e", "choose the downloads and library folders"),
        ("p", "full paths, or paths below each folder"),
        ("R", "save the report"),
        ("J", "show the report as JSON"),
        ("j k  ↑ ↓", "move through the pairs"),
        ("PgUp PgDn", "move a page"),
        ("g G", "the first and the last pair"),
        ("?", "this help"),
        ("q", "close a dialog, or quit"),
    ] {
        lines.push(key_line(key, meaning));
    }
    lines.push(Line::raw(""));
    lines.push(section("Mouse"));
    lines.push(Line::raw(""));
    for (key, meaning) in [
        (
            "Click",
            "select a pair, or press a key shown at the bottom or on a dialog",
        ),
        ("Wheel", "move through the pairs, or scroll this help"),
        ("Shift-drag", "select text to copy, in most terminals"),
    ] {
        lines.push(key_line(key, meaning));
    }
    lines.push(Line::raw(""));
    lines.push(section("Settings"));
    lines.push(Line::raw(""));
    for (name, value) in [
        ("Min age", format!("{}s", app.min_age_secs())),
        ("Min size", format_size(app.min_size_bytes())),
        ("Threads", app.threads_label()),
        ("Full reads", app.full_threads_label()),
        ("Link mode", app.link_mode_label().to_string()),
    ] {
        lines.push(Line::from(vec![
            Span::styled(pad(name, 11), Theme::popup_dim()),
            Span::styled(value, Theme::popup()),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "Set with dejaview config; see dejaview config --help.",
        Theme::popup_dim(),
    )));
    lines.push(Line::raw(""));
    lines.push(section("Folder editor"));
    lines.push(Line::raw(""));
    for (key, meaning) in [
        ("Tab", "switch field"),
        ("Enter", "move on, then save"),
        ("Esc", "cancel"),
    ] {
        lines.push(key_line(key, meaning));
    }
    lines.push(Line::from(Span::styled(
        "Esc cancels without saving; ~ is your home folder.",
        Theme::popup_dim(),
    )));
    lines
}

pub(super) fn render_help(frame: &mut Frame, app: &App) {
    let screen = frame.area();
    let width = 84u16
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    let lines = help_lines(app, text_width(width));
    let area = centred(screen, width, lines.len());
    render_scrolling(
        frame,
        app,
        area,
        lines,
        "Help",
        &[("?", "or"), ("Esc", "close")],
    );
}

pub(super) fn render_json(frame: &mut Frame, app: &App, json: &str) {
    let screen = frame.area();
    let (area, lines) = dialog_for(screen, screen.width.saturating_sub(4).min(140), json);
    let lines = lines
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Theme::popup())))
        .collect();
    render_scrolling(
        frame,
        app,
        area,
        lines,
        "Report as JSON",
        &[("J", "or"), ("Esc", "close")],
    );
}

/// A dialog that may be longer than the screen, scrolled by the app's keys:
/// the view tells the app how many lines it has and shows at once.
fn render_scrolling(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    lines: Vec<Line<'static>>,
    title: &str,
    keys: &[(&str, &str)],
) {
    let shown = usize::from(area.height.saturating_sub(4));
    let sizes = app.view_sizes();
    sizes.overlay_rows.set(shown);
    sizes.overlay_lines.set(lines.len());
    let scroll = app.overlay_scroll().min(lines.len().saturating_sub(shown));
    let mut block = frame_block(title, keys, false);
    add_key_clicks(app, area, keys);
    sizes.dialog.set(Some(area));
    if lines.len() > shown {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(
                    " {}-{} of {}, j k PgUp PgDn scroll ",
                    scroll + 1,
                    (scroll + shown).min(lines.len()),
                    lines.len()
                ),
                Theme::popup_dim(),
            ))
            .left_aligned(),
        );
    }
    // The lines on screen, cut out here: Paragraph::scroll counts in u16,
    // which a report of a few thousand entries outgrows.
    let visible = lines[scroll..(scroll + shown).min(lines.len())].to_vec();
    show(frame, area, block, visible);
}

/// What the test hardlink between the saved folders showed, for the editor.
fn link_check_lines(check: &LinkCheck, width: usize) -> Vec<Line<'static>> {
    let Some(summary) = check.summary() else {
        return plain(
            "Saving tries one hardlink between the two folders, to tell whether links will be hardlinks or symlinks.",
            width,
            Theme::popup_dim(),
        );
    };
    let (mark, style) = match check {
        LinkCheck::Hardlinks => ("✓ ", Theme::on_popup(Theme::hardlinked())),
        LinkCheck::Symlinks { .. } | LinkCheck::SymlinkOnly => {
            ("! ", Theme::on_popup(Theme::symlinked()))
        }
        LinkCheck::Refused { .. } => ("! ", Theme::on_popup(Theme::failed())),
        LinkCheck::Untested | LinkCheck::Testing | LinkCheck::NoLibraryFile => {
            ("", Theme::popup_dim())
        }
    };
    plain(&format!("{mark}Saved folders: {summary}."), width, style)
}

/// A field of the editor: its name and what it is for, then its text, the
/// end kept in view as it is typed.
fn field(name: &str, purpose: &str, value: &str, active: bool, width: usize) -> Vec<Line<'static>> {
    let heading = Line::from(vec![
        Span::styled(pad(name, 11), Theme::popup_strong()),
        Span::styled(
            super::cut(purpose, width.saturating_sub(11)),
            Theme::popup_dim(),
        ),
    ]);
    let room = width.saturating_sub(3);
    let shown = if super::width_of(value) > room {
        // The end of what is typed stays in view.
        match room {
            0 => String::new(),
            _ => format!("…{}", super::tail_fitting(value, room - 1)),
        }
    } else {
        value.to_string()
    };
    let mut input = vec![Span::styled(format!(" {shown}"), Theme::input(active))];
    if active {
        input.push(Span::styled("▏", Theme::cursor()));
    }
    let used = shown.chars().count() + 1 + usize::from(active);
    input.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        Theme::input(active),
    ));
    vec![heading, Line::from(input)]
}

pub(super) fn render_config_editor(frame: &mut Frame, app: &App, editor: &ConfigEditor) {
    let screen = frame.area();
    let width = 84u16
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20));
    let room = text_width(width);
    let mut lines = field(
        "Downloads",
        "where your torrent client saves finished downloads",
        &editor.downloads,
        editor.active_field == ConfigField::Downloads,
        room,
    );
    lines.push(Line::raw(""));
    lines.extend(field(
        "Library",
        "where Sonarr and Radarr import your movies and shows",
        &editor.media,
        editor.active_field == ConfigField::Media,
        room,
    ));
    lines.push(Line::raw(""));
    lines.extend(link_check_lines(app.link_check(), room));
    // An error is read whole, however long.
    if let Some(error) = &editor.error {
        lines.push(Line::raw(""));
        lines.extend(plain(error, room, Theme::on_popup(Theme::failed())));
    }
    lines.push(Line::raw(""));
    let tip = format!(
        "Full paths to existing folders; ~ is your home folder. Both are checked before they are saved to {}.",
        crate::config::config_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "the config file".to_string())
    );
    lines.extend(plain(&tip, room, Theme::popup_dim()));
    let area = centred(screen, width, lines.len());
    let saving = editor.active_field == ConfigField::Media;
    let keys = [
        ("Enter", if saving { "save" } else { "next" }),
        ("Tab", "switch"),
        ("Esc", "cancel"),
    ];
    let block = frame_block("Choose folders", &keys, saving);
    add_key_clicks(app, area, &keys);
    show(frame, area, block, lines);
}

pub(super) fn render_confirm_link(frame: &mut Frame, app: &App, mode: ConfirmLinkMode) {
    let screen = frame.area();
    let (title, text, answer) = match mode {
        ConfirmLinkMode::All => {
            let pairs = app.reviewed_planned_pairs().len();
            let tally = Tally::of(app.rows());
            let (hardlinks, symlinks) = (
                tally.count(LinkKind::Planned),
                tally.count(LinkKind::PlannedSymlink),
            );
            // A scan counts a cross-seeded download once, as the header does.
            let bytes = if app.showing_link_report() {
                tally.bytes(LinkKind::Planned) + tally.bytes(LinkKind::PlannedSymlink)
            } else {
                app.potential_saved_bytes()
            };
            let with_errors = app.rows().iter().filter(|row| row.error.is_some()).count();
            let how = match (hardlinks, symlinks) {
                (_, 0) => "Each download becomes a hardlink to its library file: one file on disk, and the torrent keeps seeding. If a hardlink fails, that download becomes a symlink instead.".to_string(),
                (0, _) => format!(
                    "Each download becomes a symlink to its library file, as the plan says: the two are on different filesystems, or the link mode is symlink-only. {SYMLINK_BREAKS}"
                ),
                (hardlinks, symlinks) => format!(
                    "Hardlinks: {hardlinks} {}, one file on disk each, and the torrents keep seeding. Symlinks: {symlinks} {}, as the plan says for each: the two are on different filesystems, or the link mode is symlink-only. {SYMLINK_BREAKS}",
                    if hardlinks == 1 { "pair" } else { "pairs" },
                    if symlinks == 1 { "pair" } else { "pairs" },
                ),
            };
            let mut text = format!(
                "{how}\n\nOnly the pairs this scan showed are linked: files found since are left alone, and so is a pair whose files changed.\n\n{pairs} {}, {} to free",
                if pairs == 1 { "pair" } else { "pairs" },
                format_size(bytes)
            );
            if with_errors > 0 {
                text.push_str(&format!(
                    "\n{with_errors} {} with errors {} left alone.",
                    if with_errors == 1 { "pair" } else { "pairs" },
                    if with_errors == 1 { "is" } else { "are" }
                ));
            }
            (
                format!(
                    "Link {pairs} {}?",
                    if pairs == 1 { "download" } else { "downloads" }
                ),
                text,
                format!("link all {pairs}"),
            )
        }
        ConfirmLinkMode::Selected => {
            let entry = app.selected_entry();
            let name = entry.map_or("No selection".to_string(), |entry| {
                entry
                    .source
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entry.source)
                    .to_string()
            });
            let target = entry.map_or_else(String::new, |entry| entry.target.clone());
            let symlink =
                entry.is_some_and(|entry| LinkKind::of(entry) == LinkKind::PlannedSymlink);
            let risk = if symlink {
                let reason = entry
                    .and_then(|entry| entry.fallback_reason.as_deref())
                    .map(|reason| format!("{} ", sentence(reason)))
                    .unwrap_or_default();
                format!("{reason}{SYMLINK_BREAKS}")
            } else {
                "If the hardlink fails, it becomes a symlink instead.".to_string()
            };
            let text = format!(
                "{name}\n\nbecomes a {} to\n\n{}\n\n{risk}\n\n{} still planned in all.",
                if symlink { "symlink" } else { "hardlink" },
                trim_keep_name(&target, 76),
                app.planned_actions()
            );
            ("Link this download?".to_string(), text, "link".to_string())
        }
    };
    let (area, wrapped) = dialog_for(screen, 76, &text);
    let lines = wrapped
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Theme::popup())))
        .collect();
    let keys = [("y", answer.as_str()), ("n", "cancel")];
    let block = frame_block(&title, &keys, true);
    add_key_clicks(app, area, &keys);
    show(frame, area, block, lines);
}

pub(super) fn render_confirm_rescan(frame: &mut Frame, app: &App) {
    let text = "This clears the cache, so every file in both folders is read again, as on a first run. On a network drive that can take hours.\n\nS scans reading only what changed.";
    let (area, wrapped) = dialog_for(frame.area(), 64, text);
    let lines = wrapped
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Theme::popup())))
        .collect();
    let keys = [("y", "clear and scan"), ("n", "cancel")];
    let block = frame_block("Scan everything again?", &keys, false);
    add_key_clicks(app, area, &keys);
    show(frame, area, block, lines);
}

pub(super) fn render_notice(frame: &mut Frame, app: &App, message: &str) {
    let (area, wrapped) = dialog_for(frame.area(), 70, message);
    let lines = wrapped
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Theme::popup())))
        .collect();
    let keys: &[(&str, &str)] = if app.is_running() {
        &[("c", "cancel the run"), ("Esc", "close")]
    } else {
        &[("Esc", "close")]
    };
    let block = frame_block("Not now", keys, false);
    add_key_clicks(app, area, keys);
    show(frame, area, block, lines);
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{app_with_episodes, screen};
    use super::*;
    use crate::protocol::{LinkMethod, Mode};
    use crossterm::event::{KeyCode, KeyModifiers};

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key_for_test(code, KeyModifiers::NONE);
    }

    #[test]
    fn a_dialog_is_as_tall_as_its_text() {
        let screen_area = Rect::new(0, 0, 120, 40);
        let (area, lines) = dialog_for(screen_area, 70, "One short line.");
        assert_eq!(lines, ["One short line."]);
        assert_eq!((area.width, area.height), (70, 5));
        // Never past the screen.
        let (area, _) = dialog_for(Rect::new(0, 0, 30, 5), 70, &"word ".repeat(80));
        assert!(area.width <= 30 && area.height <= 5, "{area:?}");
    }

    #[test]
    fn the_link_prompt_survives_a_small_terminal() {
        let mut app = app_with_episodes(1);
        for key in ['L', 'l'] {
            press(&mut app, KeyCode::Char(key));
            let text = screen(&app, 80, 24);
            assert!(text.contains(" y  link"), "{key}: {text}");
            assert!(text.contains(" n  cancel"), "{key}: {text}");
            press(&mut app, KeyCode::Char('n'));
        }
    }

    #[test]
    fn link_all_says_what_it_links_and_mentions_errors_only_when_there_are_some() {
        let mut app = app_with_episodes(2);
        press(&mut app, KeyCode::Char('L'));
        let text = screen(&app, 120, 40);
        assert!(text.contains("Link 2 downloads?"), "{text}");
        assert!(text.contains("2 pairs, 2.8 GiB to free"), "{text}");
        assert!(!text.contains("with errors"), "{text}");
    }

    #[test]
    fn link_all_counts_a_cross_seeded_download_once_as_the_header_does() {
        let mut app = app_with_episodes(2);
        // Two paths of one download: the scan counts its bytes once.
        let mut report = crate::protocol::RunReport {
            mode: Mode::Scan,
            planned_actions: 2,
            total_saved_bytes: 1_500_000_000,
            entries: app.rows().to_vec(),
            ..Default::default()
        };
        report.entries[1].size = 1_500_000_000;
        app.set_report_for_test(report);
        press(&mut app, KeyCode::Char('L'));
        let text = screen(&app, 120, 40);
        assert!(text.contains("2 pairs, 1.4 GiB to free"), "{text}");
    }

    #[test]
    fn link_all_says_which_pairs_become_symlinks() {
        let mut app = app_with_episodes(2);
        let mut entries = app.rows().to_vec();
        entries[1].method = Some(LinkMethod::Symlink);
        app.set_report_for_test(crate::protocol::RunReport {
            mode: Mode::Scan,
            planned_actions: 2,
            total_saved_bytes: 3_000_000_000,
            entries,
            ..Default::default()
        });
        press(&mut app, KeyCode::Char('L'));
        let text = screen(&app, 120, 40);
        let joined: String = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("Hardlinks: 1 pair"), "{text}");
        assert!(joined.contains("Symlinks: 1 pair"), "{text}");
        assert!(joined.contains("A symlink breaks"), "{text}");
    }

    #[test]
    fn the_folder_editor_survives_a_terminal_too_narrow_for_its_fields() {
        let mut app = App::new(&crate::config::Config::default(), false);
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('/'));
        for cols in 1..=10 {
            screen(&app, cols, 12);
        }
    }

    #[test]
    fn the_rescan_question_fits_a_small_terminal() {
        let mut app = app_with_episodes(1);
        press(&mut app, KeyCode::Char('x'));
        let text = screen(&app, 80, 24);
        assert!(text.contains("Scan everything again?"), "{text}");
        assert!(
            text.contains(" y  clear and scan") && text.contains(" n  cancel"),
            "{text}"
        );
    }

    #[test]
    fn help_explains_the_link_drawings_and_scrolls_to_its_end() {
        let mut app = app_with_episodes(1);
        press(&mut app, KeyCode::Char('?'));
        let text = screen(&app, 120, 50);
        assert!(text.contains("━━━━●━━━━  Hardlink"), "{text}");
        assert!(text.contains("────────▶  Symlink"), "{text}");
        let text = screen(&app, 80, 24);
        assert!(!text.contains("Esc cancels without saving"), "{text}");
        press(&mut app, KeyCode::Char('G'));
        let text = screen(&app, 80, 24);
        assert!(text.contains("Esc cancels without saving"), "{text}");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.overlay(), crate::tui::app::Overlay::None));
    }

    #[test]
    fn the_json_preview_scrolls_to_its_last_entry_and_j_does_not_close_it() {
        let mut app = app_with_episodes(40);
        press(&mut app, KeyCode::Char('J'));
        let text = screen(&app, 80, 24);
        assert!(!text.contains("S01E40"), "the start first:\n{text}");
        press(&mut app, KeyCode::Char('j'));
        assert!(matches!(
            app.overlay(),
            crate::tui::app::Overlay::JsonPreview(_)
        ));
        press(&mut app, KeyCode::End);
        let text = screen(&app, 80, 24);
        assert!(
            text.contains("S01E40") && text.lines().any(|line| line.contains("│   ]")),
            "the end of the report:\n{text}"
        );
        press(&mut app, KeyCode::Char('J'));
        assert!(matches!(app.overlay(), crate::tui::app::Overlay::None));
    }

    #[test]
    fn a_report_longer_than_65535_lines_scrolls_to_its_end() {
        let mut app = app_with_episodes(7000);
        press(&mut app, KeyCode::Char('J'));
        screen(&app, 80, 24);
        assert!(app.view_sizes().overlay_lines.get() > usize::from(u16::MAX));
        press(&mut app, KeyCode::End);
        let text = screen(&app, 80, 24);
        assert!(text.contains("S01E7000"), "the last entry:\n{text}");
        assert!(text.lines().any(|line| line.contains("│   ]")), "{text}");
    }

    #[test]
    fn the_folder_editor_names_the_config_file_and_shows_a_long_error_whole() {
        let mut app = App::new(&crate::config::Config::default(), false);
        press(&mut app, KeyCode::Char('e'));
        let long =
            "/tmp/claude-0/-home-user/fd53d022-d0e1-4c5b-9a7e-1f2e3d4c5b6a/downloads/library";
        for c in long.chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        let text = screen(&app, 80, 24);
        assert!(text.contains("config.json"), "{text}");
        let joined: String = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("Downloads: cannot use"), "{text}");
        assert!(joined.contains("library"), "the end of the path: {text}");
    }

    #[test]
    fn the_folder_editor_says_what_the_test_hardlink_showed() {
        let mut app = App::new(&crate::config::Config::default(), false);
        press(&mut app, KeyCode::Char('e'));
        let text = screen(&app, 120, 40);
        assert!(text.contains("Saving tries one hardlink"), "{text}");
        app.set_link_check_for_test(LinkCheck::Symlinks {
            cause: "different filesystems: two disks",
        });
        let text = screen(&app, 120, 40);
        assert!(
            text.contains("A test hardlink failed (different filesystems)"),
            "{text}"
        );
        assert!(text.contains("links will be symlinks"), "{text}");
    }

    #[test]
    fn the_folder_editor_has_no_blank_rows_under_its_tip() {
        let mut app = App::new(&crate::config::Config::default(), false);
        press(&mut app, KeyCode::Char('e'));
        let text = screen(&app, 80, 24);
        let lines: Vec<&str> = text.lines().collect();
        let tip_end = lines
            .iter()
            .position(|line| line.contains("config.json"))
            .expect("the tip's last line");
        // One row of padding, then the editor's bottom border.
        assert!(lines[tip_end + 2].contains('╰'), "{text}");
    }
}
