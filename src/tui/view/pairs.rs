//! The pairs: each download beside the library file it is a copy of, with
//! the link between them drawn in the middle column. The drawing's shape
//! says what the link is (a thick line for a hardlink, one file under two
//! names; an arrow for a symlink, which points at the library file) and its
//! colour how it went.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{
    SYMLINK_BREAKS, cut, format_size, group_digits, line_width, pad, pad_left, row_path, rule,
    sentence, take_top, trim_keep_name, visible_row_window,
};
use crate::protocol::{LinkMethod, Phase};
use crate::tui::app::{App, Click};
use crate::tui::layout;
use crate::tui::model::LinkReportEntry;
use crate::tui::theme::Theme;

/// What became of a pair, as its link column draws it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum LinkKind {
    /// Not linked yet: a hardlink is tried first.
    Planned,
    /// Not linked yet, and to become a symlink: the two files are on
    /// different devices, or the link mode is symlink-only.
    PlannedSymlink,
    /// One file on disk under both names.
    Hardlink,
    /// The download points at the library file.
    Symlink,
    Failed,
    /// The two names already were one file.
    Already,
    /// Left alone on purpose; the row says why.
    Skipped,
}

impl LinkKind {
    const ALL: usize = 7;

    pub(super) fn of(entry: &LinkReportEntry) -> Self {
        if entry.is_failed() {
            Self::Failed
        } else if entry.is_already_linked() {
            Self::Already
        } else if entry.is_skipped() {
            Self::Skipped
        } else if entry.is_applied_symlink() {
            Self::Symlink
        } else if entry.is_applied() {
            Self::Hardlink
        } else if entry.method == Some(LinkMethod::Symlink) {
            Self::PlannedSymlink
        } else {
            Self::Planned
        }
    }

    /// Not linked yet, whatever it will become.
    pub(super) fn is_planned(self) -> bool {
        matches!(self, Self::Planned | Self::PlannedSymlink)
    }

    /// The link column's drawing: nine cells wide, or five on a narrow
    /// terminal.
    pub(super) fn drawing(self, wide: bool) -> &'static str {
        match (self, wide) {
            (Self::Planned, true) => "╌╌╌╌○╌╌╌╌",
            (Self::Planned, false) => "╌╌○╌╌",
            (Self::PlannedSymlink, true) => "╌╌╌╌╌╌╌╌▷",
            (Self::PlannedSymlink, false) => "╌╌╌╌▷",
            (Self::Hardlink | Self::Already, true) => "━━━━●━━━━",
            (Self::Hardlink | Self::Already, false) => "━━●━━",
            (Self::Symlink, true) => "────────▶",
            (Self::Symlink, false) => "────▶",
            (Self::Failed, true) => "────✗    ",
            (Self::Failed, false) => "──✗  ",
            (Self::Skipped, true) => "    –    ",
            (Self::Skipped, false) => "  –  ",
        }
    }

    /// Three cells of the drawing, for the header's counts.
    pub(super) fn glyph(self) -> &'static str {
        match self {
            Self::Planned => "╌○╌",
            Self::PlannedSymlink => "╌╌▷",
            Self::Hardlink | Self::Already => "━●━",
            Self::Symlink => "──▶",
            Self::Failed => "─✗ ",
            Self::Skipped => " – ",
        }
    }

    pub(super) fn style(self) -> Style {
        match self {
            Self::Planned | Self::PlannedSymlink => Theme::planned(),
            Self::Hardlink => Theme::hardlinked(),
            Self::Symlink => Theme::symlinked(),
            Self::Failed => Theme::failed(),
            Self::Already | Self::Skipped => Theme::faint(),
        }
    }

    /// The row's text.
    fn text_style(self) -> Style {
        match self {
            Self::Failed => Theme::failed(),
            Self::Already | Self::Skipped => Theme::dim(),
            _ => Theme::text(),
        }
    }
}

/// How many pairs, and how many bytes, each kind holds.
#[derive(Default)]
pub(super) struct Tally {
    counts: [(usize, u64); LinkKind::ALL],
}

impl Tally {
    pub(super) fn of(rows: &[LinkReportEntry]) -> Self {
        let mut tally = Self::default();
        for row in rows {
            let slot = &mut tally.counts[LinkKind::of(row) as usize];
            slot.0 += 1;
            slot.1 += row.size;
        }
        tally
    }

    pub(super) fn count(&self, kind: LinkKind) -> usize {
        self.counts[kind as usize].0
    }

    pub(super) fn bytes(&self, kind: LinkKind) -> u64 {
        self.counts[kind as usize].1
    }
}

/// A line of the pairs: a release folder's heading, or a pair, indented
/// under its heading when it has one.
#[derive(Debug, PartialEq)]
enum Item {
    Folder {
        download: String,
        library: String,
        bytes: u64,
    },
    Pair {
        index: usize,
        nested: bool,
    },
}

/// The release folder a download sits in: its first folder below the
/// downloads folder, if it is in one. Rows come in path order, so every
/// file of a release, in subfolders too, comes in one run.
fn release_folder<'a>(app: &App, entry: &'a LinkReportEntry) -> Option<&'a str> {
    row_path(&entry.source, app.downloads_root(), false)
        .split_once('/')
        .map(|(folder, _)| folder)
}

/// The folder the library files share, below the media folder.
fn shared_library_folder(app: &App, rows: &[LinkReportEntry]) -> String {
    let folders: Vec<Vec<&str>> = rows
        .iter()
        .map(|row| {
            let path = row_path(&row.target, app.media_root(), false);
            let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
            folder.split('/').collect()
        })
        .collect();
    let Some(first) = folders.first() else {
        return String::new();
    };
    let shared = first
        .iter()
        .enumerate()
        .take_while(|(depth, part)| folders.iter().all(|other| other.get(*depth) == Some(part)))
        .count();
    first[..shared].join("/")
}

/// The pairs in order, a season's episodes under their release folder: two
/// or more rows in a row that share one. Full paths show no folders, since
/// each row then names its own.
fn items(app: &App) -> Vec<Item> {
    let rows = app.rows();
    let mut items = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let folder = (!app.expand_paths())
            .then(|| release_folder(app, &rows[start]))
            .flatten();
        let mut end = start + 1;
        if let Some(folder) = folder {
            while end < rows.len() && release_folder(app, &rows[end]) == Some(folder) {
                end += 1;
            }
        }
        match folder {
            Some(folder) if end - start >= 2 => {
                items.push(Item::Folder {
                    download: folder.to_string(),
                    library: shared_library_folder(app, &rows[start..end]),
                    bytes: rows[start..end].iter().map(|row| row.size).sum(),
                });
                items.extend((start..end).map(|index| Item::Pair {
                    index,
                    nested: true,
                }));
            }
            _ => {
                end = start + 1;
                items.push(Item::Pair {
                    index: start,
                    nested: false,
                });
            }
        }
        start = end;
    }
    items
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The download as its row names it: under a release folder's heading, its
/// path below that folder; else the file name when its folder only repeats
/// it, as a release's does, or its path below the downloads folder when that
/// fits in `width`, or else the file name; the whole path with `p`.
fn download_text(app: &App, entry: &LinkReportEntry, nested: bool, width: usize) -> String {
    let path = row_path(&entry.source, app.downloads_root(), app.expand_paths());
    if app.expand_paths() {
        return path.to_string();
    }
    let name = file_name(path);
    if nested {
        // Below its release folder, which the heading above names.
        return path
            .split_once('/')
            .map_or(path, |(_, below)| below)
            .to_string();
    }
    if super::width_of(path) > width {
        return name.to_string();
    }
    match path.rsplit_once('/') {
        Some((folder, _)) => {
            let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            if file_name(folder) == stem {
                name.to_string()
            } else {
                path.to_string()
            }
        }
        None => name.to_string(),
    }
}

/// The library file as its row names it: its path below the media folder
/// when that fits in `width`, else its file name, which Sonarr and Radarr
/// make tell one file from another; the whole path with `p`.
fn library_text(app: &App, entry: &LinkReportEntry, nested: bool, width: usize) -> String {
    let path = row_path(&entry.target, app.media_root(), app.expand_paths());
    if app.expand_paths() {
        return path.to_string();
    }
    if nested || super::width_of(path) > width {
        file_name(path).to_string()
    } else {
        path.to_string()
    }
}

/// The widths of the pairs' columns: the marker and a space, the download,
/// the link, the library file (none on a narrow terminal), the size, with
/// two spaces between each and one spare at the right.
struct Columns {
    download: usize,
    wide_link: bool,
    library: usize,
}

const SIZE_WIDTH: usize = 9;
const GAP: &str = "  ";

impl Columns {
    fn for_width(width: u16) -> Self {
        let wide_link = width >= 70;
        let link = if wide_link { 9 } else { 5 };
        let fixed = 2 + GAP.len() + link + GAP.len() + SIZE_WIDTH + 1;
        let width = usize::from(width);
        if width >= usize::from(layout::LIBRARY_COLUMNS) {
            let rest = width.saturating_sub(fixed + GAP.len());
            let download = rest * 58 / 100;
            Self {
                download,
                wide_link,
                library: rest - download,
            }
        } else {
            Self {
                download: width.saturating_sub(fixed),
                wide_link,
                library: 0,
            }
        }
    }

    fn link(&self) -> usize {
        if self.wide_link { 9 } else { 5 }
    }
}

/// A row of the pairs, padded to `width` and shaded when selected.
fn row(
    width: u16,
    columns: &Columns,
    selected: bool,
    download: Span<'static>,
    link: Span<'static>,
    library: Span<'static>,
    size: Span<'static>,
) -> Line<'static> {
    let marker = if selected {
        Span::styled("▌", Theme::marker())
    } else {
        Span::raw(" ")
    };
    let mut spans = vec![marker, Span::raw(" "), download, Span::raw(GAP), link];
    if columns.library > 0 {
        spans.push(Span::raw(GAP));
        spans.push(library);
    }
    spans.push(Span::raw(GAP));
    spans.push(size);
    let mut line = Line::from(spans);
    let used = line_width(&line);
    line.spans.push(Span::raw(
        " ".repeat(usize::from(width).saturating_sub(used)),
    ));
    if selected {
        for span in line.spans.iter_mut().skip(1) {
            span.style = span.style.patch(Theme::selected());
        }
    }
    line
}

/// The column headings: each folder named with its path, the link, the size.
fn headings(app: &App, width: u16, columns: &Columns) -> Line<'static> {
    let named = |name: &str, root: Option<&std::path::Path>, width: usize| -> Span<'static> {
        let path = root.map_or_else(|| "not set".to_string(), |root| root.display().to_string());
        let room = width.saturating_sub(name.chars().count() + 1);
        Span::styled(pad(&trim_keep_name(&path, room), room), Theme::dim())
    };
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("Downloads ", Theme::heading()),
        named("Downloads", app.downloads_root(), columns.download),
        Span::raw(GAP),
        Span::styled(
            pad(
                &format!("{:^width$}", "Link", width = columns.link()),
                columns.link(),
            ),
            Theme::dim(),
        ),
    ];
    if columns.library > 0 {
        spans.push(Span::raw(GAP));
        spans.push(Span::styled("Library ", Theme::heading()));
        spans.push(named("Library", app.media_root(), columns.library));
    }
    spans.push(Span::raw(GAP));
    spans.push(Span::styled(pad_left("Size", SIZE_WIDTH), Theme::dim()));
    let mut line = Line::from(spans);
    let used = line_width(&line);
    line.spans.push(Span::raw(
        " ".repeat(usize::from(width).saturating_sub(used)),
    ));
    line
}

/// The pairs, scrolled so the selected one shows, with its release folder's
/// heading when it is the first under it.
pub(super) fn render_pairs(frame: &mut Frame, area: Rect, app: &App) {
    let columns = Columns::for_width(area.width);
    let (top, list) = take_top(area, 2);
    frame.render_widget(
        Paragraph::new(headings(app, area.width, &columns)),
        Rect { height: 1, ..top },
    );
    if top.height > 1 {
        rule(
            frame,
            Rect {
                y: top.y + 1,
                height: 1,
                ..top
            },
        );
    }

    let items = items(app);
    let selected = items
        .iter()
        .position(|item| matches!(item, Item::Pair { index, .. } if *index == app.selected_row()))
        .unwrap_or(0);
    let (start, end) = visible_row_window(items.len(), selected, usize::from(list.height));
    // PgUp and PgDn move by the pairs on screen, not the lines, so a page
    // of release headings skips no pair.
    let pairs_shown = items[start..end]
        .iter()
        .filter(|item| matches!(item, Item::Pair { .. }))
        .count();
    app.view_sizes().result_rows.set(pairs_shown.max(1));

    // A click on a pair selects it, and one on a release heading selects
    // the first episode under it.
    for offset in 0..end - start {
        let index = items[start + offset..].iter().find_map(|item| match item {
            Item::Pair { index, .. } => Some(*index),
            Item::Folder { .. } => None,
        });
        if let (Some(index), Ok(line_offset)) = (index, u16::try_from(offset)) {
            let line = Rect {
                y: list.y + line_offset,
                height: 1,
                ..list
            };
            app.view_sizes().add_click(line, Click::Row(index));
        }
    }

    let rows = app.rows();
    let lines: Vec<Line> = items[start..end]
        .iter()
        .map(|item| match item {
            Item::Folder {
                download,
                library,
                bytes,
            } => row(
                area.width,
                &columns,
                false,
                Span::styled(
                    pad(
                        &format!(
                            "▾ {}",
                            trim_keep_name(download, columns.download.saturating_sub(2))
                        ),
                        columns.download,
                    ),
                    Theme::muted(),
                ),
                Span::raw(" ".repeat(columns.link())),
                Span::styled(
                    pad(&trim_keep_name(library, columns.library), columns.library),
                    Theme::muted(),
                ),
                Span::styled(pad_left(&format_size(*bytes), SIZE_WIDTH), Theme::muted()),
            ),
            Item::Pair { index, nested } => {
                let entry = &rows[*index];
                let kind = LinkKind::of(entry);
                let selected = *index == app.selected_row();
                let indent = if *nested { "  " } else { "" };
                let text = |text: String, width: usize| {
                    let room = width.saturating_sub(indent.len());
                    pad(&format!("{indent}{}", trim_keep_name(&text, room)), width)
                };
                let mut style = kind.text_style();
                if selected {
                    style = style.add_modifier(Modifier::BOLD);
                }
                let library_style = if kind.is_planned() && !selected {
                    Theme::muted()
                } else {
                    kind.text_style()
                };
                row(
                    area.width,
                    &columns,
                    selected,
                    Span::styled(
                        text(
                            download_text(app, entry, *nested, columns.download),
                            columns.download,
                        ),
                        style,
                    ),
                    Span::styled(kind.drawing(columns.wide_link), kind.style()),
                    Span::styled(
                        text(
                            library_text(app, entry, *nested, columns.library),
                            columns.library,
                        ),
                        library_style,
                    ),
                    Span::styled(
                        pad_left(&format_size(entry.size), SIZE_WIDTH),
                        kind.text_style(),
                    ),
                )
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

/// What the selected pair's link is, or will be.
fn link_sentence(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Planned => "becomes a hardlink to",
        LinkKind::PlannedSymlink => "becomes a symlink to",
        LinkKind::Hardlink => "is a hardlink to",
        LinkKind::Symlink => "is a symlink to",
        LinkKind::Failed => "was not linked to",
        LinkKind::Already => "already is",
        LinkKind::Skipped => "was left alone beside",
    }
}

/// What the person should know about the selected pair, and in what
/// colour: each symlink's reason, as the plan or the run gave it, comes with
/// what a symlink risks.
fn note(entry: &LinkReportEntry, kind: LinkKind) -> (String, Style) {
    let reason = entry.fallback_reason.as_deref().map(sentence);
    let reason_and = |rest: &str| match &reason {
        Some(reason) => format!("{reason} {rest}"),
        None => rest.to_string(),
    };
    match kind {
        LinkKind::Planned => (
            "One file on disk under both names, and the torrent keeps seeding. If the hardlink fails, it becomes a symlink.".to_string(),
            Theme::dim(),
        ),
        LinkKind::Hardlink => (
            "One file on disk under both names, and the torrent keeps seeding.".to_string(),
            Theme::dim(),
        ),
        LinkKind::PlannedSymlink | LinkKind::Symlink => {
            (reason_and(SYMLINK_BREAKS), Theme::symlinked())
        }
        LinkKind::Failed => (
            entry
                .error
                .as_deref()
                .map(sentence)
                .unwrap_or_else(|| "The link failed; the log says why.".to_string()),
            Theme::failed(),
        ),
        LinkKind::Already => (
            "Nothing to do: the two names already are one file.".to_string(),
            Theme::dim(),
        ),
        LinkKind::Skipped => (
            entry
                .error
                .as_deref()
                .map(sentence)
                .unwrap_or_else(|| "Left alone on purpose.".to_string()),
            Theme::dim(),
        ),
    }
}

/// The selected pair in full: its download, the link it has or will have to
/// which library file, and what to know about it.
pub(super) fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    rule(frame, area);
    let Some(entry) = app.selected_entry() else {
        return;
    };
    let body = super::inset(take_top(area, 1).1, 2);
    let width = usize::from(body.width);
    let kind = LinkKind::of(entry);
    let (folder, name) = entry
        .source
        .rsplit_once('/')
        .map_or(("", entry.source.as_str()), |(folder, name)| (folder, name));
    let size = format_size(entry.size);
    let says = link_sentence(kind);
    let drawing = kind.drawing(true);
    let target_room = width.saturating_sub(drawing.chars().count() + says.len() + 2);
    let (note, note_style) = note(entry, kind);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                pad(name, width.saturating_sub(size.len() + 2)),
                Theme::strong(),
            ),
            Span::raw("  "),
            Span::styled(size, Theme::strong()),
        ]),
        Line::from(Span::styled(
            trim_keep_name(&format!("{folder}/"), width),
            Theme::dim(),
        )),
        Line::from(vec![
            Span::styled(drawing, kind.style()),
            Span::styled(format!(" {says} "), Theme::muted()),
            Span::styled(trim_keep_name(&entry.target, target_room), Theme::text()),
        ]),
    ];
    // The note takes the rows left, cut on the last.
    let rows = usize::from(body.height).saturating_sub(lines.len()).max(1);
    let mut lines = lines;
    let mut wrapped = super::wrap_text(&note, width);
    if wrapped.len() > rows {
        let rest = wrapped.split_off(rows - 1).join(" ");
        wrapped.push(cut(&rest, width));
    }
    lines.extend(
        wrapped
            .into_iter()
            .map(|line| Line::from(Span::styled(line, note_style))),
    );
    frame.render_widget(Paragraph::new(lines), body);
}

/// The steps of a scan, which run in this order.
const SCAN_STEPS: [(Phase, &str); 4] = [
    (Phase::Collecting, "Find files"),
    (Phase::HashingSample, "Hash"),
    (Phase::HashingFull, "Verify matches"),
    (Phase::Planning, "Plan links"),
];

fn steps_line(app: &App) -> Line<'static> {
    let current = app
        .phase()
        .and_then(|phase| SCAN_STEPS.iter().position(|(step, _)| *step == phase))
        .unwrap_or(0);
    let mut spans = vec![Span::raw("  ")];
    for (index, (_, label)) in SCAN_STEPS.iter().enumerate() {
        let (mark, mark_style, label_style) = match index.cmp(&current) {
            std::cmp::Ordering::Less => ("✓", Theme::hardlinked(), Theme::muted()),
            std::cmp::Ordering::Equal => ("●", Theme::progress(), Theme::strong()),
            std::cmp::Ordering::Greater => ("○", Theme::faint(), Theme::faint()),
        };
        spans.push(Span::styled(format!("{mark} "), mark_style));
        spans.push(Span::styled(label.to_string(), label_style));
        if index + 1 < SCAN_STEPS.len() {
            let joint = if index < current {
                Theme::hardlinked()
            } else {
                Theme::rule()
            };
            spans.push(Span::styled(" ──── ", joint));
        }
    }
    Line::from(spans)
}

/// Which folder `path` is in, as a row heading names it.
fn root_name(app: &App, path: &str) -> &'static str {
    let path = std::path::Path::new(path);
    if app
        .downloads_root()
        .is_some_and(|root| path.starts_with(root))
    {
        "Downloads"
    } else if app.media_root().is_some_and(|root| path.starts_with(root)) {
        "Library"
    } else {
        ""
    }
}

/// A scan's steps and the files it reads now, above the pairs it has
/// found so far.
pub(super) fn render_scan(frame: &mut Frame, area: Rect, app: &App) {
    let width = usize::from(area.width).saturating_sub(16);
    let mut reading: Vec<(&str, String)> = app
        .active_worker_paths()
        .into_iter()
        .take(3)
        .map(|(_, path)| {
            let name = root_name(app, path);
            let root = if name == "Downloads" {
                app.downloads_root()
            } else {
                app.media_root()
            };
            (
                name,
                trim_keep_name(row_path(path, root, app.expand_paths()), width),
            )
        })
        .collect();
    if reading.is_empty()
        && let Some(path) = app.current_path()
    {
        reading.push((root_name(app, path), trim_keep_name(path, width)));
    }
    let mut lines = vec![Line::raw(""), steps_line(app), Line::raw("")];
    if !reading.is_empty() {
        lines.push(Line::from(Span::styled("  Reading now", Theme::dim())));
        for (root, path) in reading {
            lines.push(Line::from(vec![
                Span::styled(format!("  {}", pad(root, 11)), Theme::dim()),
                Span::styled(path, Theme::text()),
            ]));
        }
    }
    let used = u16::try_from(lines.len() + 1).unwrap_or(u16::MAX);
    let (top, rest) = take_top(area, used);
    frame.render_widget(Paragraph::new(lines), top);
    if rest.height < 3 {
        return;
    }
    if app.rows().is_empty() {
        let hint = Line::from(Span::styled(
            "  The pairs it finds show here as it goes.",
            Theme::faint(),
        ));
        frame.render_widget(Paragraph::new(hint), take_top(rest, 1).0);
    } else {
        render_pairs(frame, rest, app);
    }
}

/// What stands in for the pairs before there are any: the two folders
/// explained on a first run, the next step after, and what a scan that
/// found nothing means.
pub(super) fn render_empty(frame: &mut Frame, area: Rect, app: &App) {
    let columns = Columns::for_width(area.width);
    let (top, rest) = take_top(area, 2);
    frame.render_widget(
        Paragraph::new(headings(app, area.width, &columns)),
        Rect { height: 1, ..top },
    );
    if top.height > 1 {
        rule(
            frame,
            Rect {
                y: top.y + 1,
                height: 1,
                ..top
            },
        );
    }
    let mut lines = vec![Line::raw("")];
    if !app.has_paths() {
        let pair = |left: &str, right: &str, linked: bool| {
            let link = match (linked, columns.wide_link) {
                (false, _) => " ".repeat(columns.link()),
                (true, true) => "╌╌╌╌?╌╌╌╌".to_string(),
                (true, false) => "╌╌?╌╌".to_string(),
            };
            row(
                area.width,
                &columns,
                false,
                Span::styled(pad(left, columns.download), Theme::muted()),
                Span::styled(link, Theme::faint()),
                Span::styled(pad(right, columns.library), Theme::muted()),
                Span::raw(" ".repeat(SIZE_WIDTH)),
            )
        };
        if columns.library > 0 {
            lines.push(pair(
                "Where your torrent client saves",
                "Where Sonarr and Radarr import",
                true,
            ));
            lines.push(pair("finished downloads", "your movies and shows", false));
        } else {
            lines.push(Line::from(Span::styled(
                "  Downloads: where your torrent client saves finished downloads.",
                Theme::muted(),
            )));
            lines.push(Line::from(Span::styled(
                "  Library: where Sonarr and Radarr import your movies and shows.",
                Theme::muted(),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(""));
        lines.push(
            Line::from(vec![
                Span::styled("Press ", Theme::text()),
                Span::styled(" e ", Theme::key_primary()),
                Span::styled(" to choose the two folders.", Theme::text()),
            ])
            .centered(),
        );
        lines.push(Line::raw(""));
        lines.push(
            Line::from(Span::styled(
                "Then S scans. Nothing changes on disk until you link.",
                Theme::dim(),
            ))
            .centered(),
        );
    } else if !app.has_report() {
        lines.push(Line::raw(""));
        lines.push(
            Line::from(vec![
                Span::styled("Press ", Theme::text()),
                Span::styled(" S ", Theme::key_primary()),
                Span::styled(
                    " to find the downloads that are copies of library files.",
                    Theme::text(),
                ),
            ])
            .centered(),
        );
        lines.push(Line::raw(""));
        lines.push(
            Line::from(Span::styled(
                "A scan only reads. Nothing changes on disk until you link.",
                Theme::dim(),
            ))
            .centered(),
        );
    } else {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("No duplicates found.", Theme::strong())).centered());
        lines.push(Line::raw(""));
        lines.push(
            Line::from(Span::styled(
                format!(
                    "Of {} files, no download is a copy of a library file that is not linked to it already.",
                    group_digits(app.files_considered() as u64)
                ),
                Theme::dim(),
            ))
            .centered(),
        );
    }
    frame.render_widget(Paragraph::new(lines), rest);
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{app_with_episodes, app_with_link_report, screen};
    use super::*;
    use crate::protocol::{EntryStatus, LinkMethod, Mode};
    use crate::tui::app::LinkCheck;

    /// The lines that show a pair: those with a link drawing.
    fn pair_lines(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| {
                ["○", "●", "▶", "▷", "✗", "–"]
                    .iter()
                    .any(|mark| line.contains(mark))
            })
            .filter(|line| line.contains("iB"))
            .collect()
    }

    #[test]
    fn each_kind_of_link_has_its_own_drawing() {
        let kinds = [
            LinkKind::Planned,
            LinkKind::PlannedSymlink,
            LinkKind::Hardlink,
            LinkKind::Symlink,
            LinkKind::Failed,
            LinkKind::Skipped,
        ];
        for wide in [true, false] {
            let drawings: Vec<&str> = kinds.iter().map(|kind| kind.drawing(wide)).collect();
            for (index, drawing) in drawings.iter().enumerate() {
                assert_eq!(drawing.chars().count(), if wide { 9 } else { 5 });
                assert!(!drawings[index + 1..].contains(drawing), "{drawing}");
            }
        }
        // A hardlink and a symlink differ in shape, not only in colour.
        assert!(LinkKind::Hardlink.drawing(true).contains('━'));
        assert!(LinkKind::Symlink.drawing(true).ends_with('▶'));
        assert_ne!(LinkKind::Hardlink.style(), LinkKind::Symlink.style());
    }

    #[test]
    fn a_link_report_draws_hardlinks_and_symlinks_apart() {
        let app = app_with_link_report();
        let text = screen(&app, 120, 36);
        let rows = pair_lines(&text);
        let row = |name: &str| rows.iter().find(|row| row.contains(name)).copied();
        assert!(
            row("Alpha.2020").is_some_and(|row| row.contains("━━━━●━━━━")),
            "{text}"
        );
        assert!(
            row("Beta.2021").is_some_and(|row| row.contains("────────▶")),
            "{text}"
        );
        assert!(
            row("Gamma.2022").is_some_and(|row| row.contains('✗')),
            "{text}"
        );
    }

    #[test]
    fn rows_of_a_season_can_be_told_apart() {
        let app = app_with_episodes(3);
        let text = screen(&app, 120, 40);
        for n in 1..=3 {
            assert!(text.contains(&format!("S01E0{n}")), "episode {n}:\n{text}");
        }
        assert!(text.contains("Size") && text.contains("1.4 GiB"), "{text}");
        let rows = pair_lines(&text);
        assert_eq!(rows.len(), 3, "{text}");
        assert!(
            rows.iter()
                .all(|row| !row.contains("a-very-long-session-folder")),
            "the downloads folder is not repeated in the rows:\n{text}"
        );
    }

    #[test]
    fn a_season_sits_under_its_release_folder() {
        let app = app_with_episodes(3);
        let text = screen(&app, 120, 40);
        let heading = text
            .lines()
            .find(|line| line.contains("▾ Show.Name.S01.1080p.WEB-DL.H.264-GROUP"))
            .unwrap_or_else(|| panic!("the release folder:\n{text}"));
        assert!(heading.contains("TV/Show Name/Season 1"), "{heading}");
        assert!(heading.contains("4.2 GiB"), "the season's size: {heading}");
    }

    #[test]
    fn a_wide_table_shows_the_library_file_and_p_shows_full_paths() {
        let mut app = app_with_episodes(2);
        let text = screen(&app, 200, 40);
        let rows = pair_lines(&text);
        assert_eq!(rows.len(), 2, "{text}");
        assert!(text.contains("Library"), "{text}");
        assert!(rows[1].contains("Show Name S01E02.mkv"), "{text}");
        assert!(!rows[1].contains("/srv/media"), "below its folder: {text}");

        app.toggle_path_mode_for_test();
        let text = screen(&app, 200, 40);
        let rows = pair_lines(&text);
        assert!(
            rows[1].contains("/srv/media/TV/Show Name/Season 1"),
            "{text}"
        );
        assert!(rows[1].contains("/downloads/Show.Name.S01.1080p"), "{text}");
        assert!(
            !text.contains('▾'),
            "full paths need no folder headings:\n{text}"
        );
    }

    #[test]
    fn a_narrow_table_leaves_the_library_file_to_the_details() {
        let app = app_with_episodes(2);
        let text = screen(&app, 80, 24);
        let rows = pair_lines(&text);
        assert!(
            rows.iter().all(|row| !row.contains("Show Name S01E0")),
            "{text}"
        );
        let details = text
            .lines()
            .find(|line| line.contains("becomes a hardlink to"))
            .unwrap_or_else(|| panic!("the details:\n{text}"));
        assert!(details.contains("Show Name S01E01.mkv"), "{details}");
    }

    #[test]
    fn the_details_say_why_a_pair_is_a_symlink_or_failed() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut app = app_with_link_report();
        app.handle_key_for_test(KeyCode::Down, KeyModifiers::NONE);
        let text = screen(&app, 120, 36);
        assert!(
            text.contains("is a symlink to /media/Movies/Beta.2021.mkv"),
            "{text}"
        );
        assert!(
            text.contains("Different filesystems. A symlink breaks if its library file is renamed"),
            "{text}"
        );
        app.handle_key_for_test(KeyCode::Down, KeyModifiers::NONE);
        let text = screen(&app, 120, 36);
        assert!(text.contains("was not linked to"), "{text}");
        assert!(text.contains("Permission denied (os error 13)"), "{text}");
    }

    /// A scan with one pair planned as a symlink, as symlink-only mode or
    /// two devices plan it, and one planned as a hardlink.
    fn app_with_a_planned_symlink() -> App {
        let config = crate::config::Config {
            downloads: Some("/downloads".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let planned = |name: &str, method: LinkMethod, reason: &str| LinkReportEntry {
            source: format!("/downloads/{name}/{name}.mkv"),
            target: format!("/media/Movies/{name}.mkv"),
            size: 1_000_000_000,
            status: EntryStatus::Planned,
            method: Some(method),
            fallback_reason: Some(reason.to_string()),
            ..Default::default()
        };
        app.set_report_for_test(crate::tui::model::RunReport {
            mode: Mode::Scan,
            planned_actions: 2,
            total_saved_bytes: 2_000_000_000,
            entries: vec![
                planned("Alpha", LinkMethod::Symlink, "symlink-only mode enabled"),
                planned(
                    "Beta",
                    LinkMethod::TryHardlink,
                    "tries hardlink first; may fall back to symlink",
                ),
            ],
            ..Default::default()
        });
        app
    }

    #[test]
    fn a_pair_planned_as_a_symlink_says_so_and_why() {
        let mut app = app_with_a_planned_symlink();
        // The test hardlink speaks for the folders, not for this pair.
        app.set_link_check_for_test(LinkCheck::Hardlinks);
        let text = screen(&app, 120, 36);
        let row = pair_lines(&text)
            .into_iter()
            .find(|row| row.contains("Alpha"))
            .unwrap_or_else(|| panic!("the Alpha row:\n{text}"));
        assert!(row.contains("╌╌╌╌╌╌╌╌▷"), "{row}");
        assert!(
            text.contains("becomes a symlink to /media/Movies/Alpha.mkv"),
            "{text}"
        );
        assert!(
            text.contains("Symlink-only mode enabled. A symlink breaks"),
            "{text}"
        );
        assert!(
            !text.contains("becomes a hardlink to /media/Movies/Alpha"),
            "{text}"
        );
    }

    #[test]
    fn a_wide_name_keeps_the_columns_in_line() {
        let config = crate::config::Config {
            downloads: Some("/dl".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entry = |name: &str| LinkReportEntry {
            source: format!("/dl/{name}"),
            target: format!("/media/{name}"),
            size: 1_000_000_000,
            status: EntryStatus::Planned,
            method: Some(LinkMethod::TryHardlink),
            ..Default::default()
        };
        app.set_report_for_test(crate::tui::model::RunReport {
            mode: Mode::Scan,
            planned_actions: 2,
            entries: vec![entry("葬送のフリーレン 01.mkv"), entry("Plain 01.mkv")],
            ..Default::default()
        });
        let backend = ratatui::backend::TestBackend::new(120, 36);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::super::render(frame, &app))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let size_column = |needle: &str| {
            (0..36u16)
                .find_map(|y| {
                    let row: String = (0..120u16).map(|x| buffer[(x, y)].symbol()).collect();
                    (row.contains(needle) && row.contains("MiB"))
                        .then(|| (0..120u16).find(|&x| buffer[(x, y)].symbol() == "M"))
                        .flatten()
                })
                .unwrap_or_else(|| panic!("the {needle} row's size"))
        };
        assert_eq!(size_column("Plain"), size_column("フ"));
    }

    #[test]
    fn paging_shows_every_pair_past_release_headings() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let config = crate::config::Config {
            downloads: Some("/dl".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entries = (0..30)
            .flat_map(|group| {
                (0..2).map(move |episode| LinkReportEntry {
                    source: format!("/dl/G{group:02}/G{group:02}E{episode}.mkv"),
                    target: format!("/media/G{group:02}/G{group:02}E{episode}.mkv"),
                    size: 1,
                    status: EntryStatus::Planned,
                    ..Default::default()
                })
            })
            .collect();
        app.set_report_for_test(crate::tui::model::RunReport {
            mode: Mode::Scan,
            entries,
            ..Default::default()
        });
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..30 {
            let text = screen(&app, 80, 24);
            for group in 0..30 {
                for episode in 0..2 {
                    let name = format!("G{group:02}E{episode}.mkv");
                    if text.contains(&name) {
                        seen.insert(name);
                    }
                }
            }
            app.handle_key_for_test(KeyCode::PageDown, KeyModifiers::NONE);
        }
        assert_eq!(seen.len(), 60, "pages skipped pairs: {seen:?}");
    }

    #[test]
    fn a_release_with_a_subfolder_has_one_heading() {
        let config = crate::config::Config {
            downloads: Some("/dl".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entries = [
            "Show/a1.mkv",
            "Show/a2.mkv",
            "Show/Extras/c.mkv",
            "Show/z1.mkv",
        ]
        .into_iter()
        .map(|path| LinkReportEntry {
            source: format!("/dl/{path}"),
            target: format!("/media/TV/{path}"),
            size: 1,
            status: EntryStatus::Planned,
            ..Default::default()
        })
        .collect();
        app.set_report_for_test(crate::tui::model::RunReport {
            mode: Mode::Scan,
            entries,
            ..Default::default()
        });
        let text = screen(&app, 120, 36);
        assert_eq!(text.matches("▾ Show").count(), 1, "{text}");
        assert!(
            text.contains("  Extras/c.mkv"),
            "the path below the release: {text}"
        );
    }

    #[test]
    fn a_first_run_explains_both_folders_and_offers_e() {
        let app = App::new(&crate::config::Config::default(), false);
        let text = screen(&app, 120, 36);
        assert!(text.contains("Where your torrent client saves"), "{text}");
        assert!(text.contains("Where Sonarr and Radarr import"), "{text}");
        assert!(text.contains(" e  to choose the two folders."), "{text}");
    }

    #[test]
    fn a_scan_shows_its_steps_and_what_it_reads() {
        let mut app = app_with_episodes(0);
        let _cancel = app.set_running_scan_for_test();
        app.set_hash_activity_for_test(
            Phase::HashingSample,
            &[(1, "/srv/media/TV/Show Name/Season 1/Show Name S01E03.mkv")],
        );
        let text = screen(&app, 120, 36);
        assert!(text.contains("✓ Find files"), "{text}");
        assert!(text.contains("● Hash"), "{text}");
        assert!(text.contains("○ Verify matches"), "{text}");
        assert!(
            text.contains("Library    TV/Show Name/Season 1/Show Name S01E03.mkv"),
            "{text}"
        );
        assert!(!text.contains("No duplicates"), "{text}");
    }
}
