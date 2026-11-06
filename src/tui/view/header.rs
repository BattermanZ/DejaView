//! The header: the logo, then the one number that matters now drawn big
//! beside it (space to free, space freed, or how far a run is), the counts
//! behind it, a bar of how much is done, and a line saying what is going on.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::pairs::{LinkKind, Tally};
use super::{cut, format_size, group_digits, line_width, pad_left, rule, take_top, wrap_text};
use crate::protocol::Phase;
use crate::tui::app::{App, LinkCheck};
use crate::tui::bignum;
use crate::tui::theme::{Theme, Tone};
use crate::tui::wordmark;

/// The number drawn big, its unit and what it counts.
struct Hero {
    number: String,
    unit: String,
    caption: String,
    tone: Tone,
}

/// A size split into the number drawn big and its unit: `56.0` and `GiB`.
fn split_size(bytes: u64) -> (String, String) {
    let text = format_size(bytes);
    match text.split_once(' ') {
        Some((number, unit)) => (number.to_string(), unit.to_string()),
        None => (text, String::new()),
    }
}

/// The running job's step in a word or two.
fn phase_word(app: &App) -> &'static str {
    if app.cancelling() {
        return "cancelling";
    }
    match app.phase() {
        Some(Phase::Collecting) => "finding files",
        Some(Phase::HashingSample) => "hashing",
        Some(Phase::HashingFull) => "verifying",
        Some(Phase::Planning) => "planning",
        Some(Phase::Applying) => "linking",
        _ if app.scan_running() => "scanning",
        _ => "linking",
    }
}

fn hero(app: &App) -> Option<Hero> {
    if app.is_running() {
        let (number, unit, caption) = match app.run_progress() {
            Some((done, total)) if total > 0 => (
                (done.min(total) * 100 / total).to_string(),
                "%".to_string(),
                phase_word(app).to_string(),
            ),
            _ => (
                app.files_considered().to_string(),
                String::new(),
                "files found".to_string(),
            ),
        };
        return Some(Hero {
            number,
            unit,
            caption,
            tone: Tone::Working,
        });
    }
    if !app.has_report() {
        return None;
    }
    let (bytes, caption, tone) = if app.showing_link_report() {
        (app.saved_bytes(), "freed", Tone::Freed)
    } else {
        (app.potential_saved_bytes(), "to free", Tone::ToFree)
    };
    let (number, unit) = split_size(bytes);
    Some(Hero {
        number,
        unit,
        caption: caption.to_string(),
        tone: if bytes == 0 { Tone::Quiet } else { tone },
    })
}

/// A count behind the big number, with the link drawing it counts when it
/// counts pairs, so the counts double as the link column's key.
struct Stat {
    kind: Option<LinkKind>,
    value: String,
    label: String,
}

fn stat(kind: Option<LinkKind>, value: impl ToString, label: &str) -> Stat {
    Stat {
        kind,
        value: value.to_string(),
        label: label.to_string(),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 { one } else { many }.to_string()
}

fn stats(app: &App) -> Vec<Stat> {
    if app.is_running() {
        let mut stats = Vec::new();
        if app.scan_running() {
            if let Some((done, total)) = app.hash_progress() {
                let verb = if app.phase() == Some(Phase::HashingFull) {
                    "verified"
                } else {
                    "hashed"
                };
                stats.push(stat(
                    None,
                    group_digits(done as u64),
                    &format!("of {} {verb}", group_digits(total as u64)),
                ));
            }
            if app.files_considered() > 0 {
                stats.push(stat(
                    None,
                    group_digits(app.files_considered() as u64),
                    "files found",
                ));
            }
        } else if let Some((done, total)) = app.run_progress() {
            // While linking, the run counts the pairs it has linked of all it
            // was given.
            stats.push(stat(
                None,
                group_digits(done),
                &format!("of {} linked", group_digits(total)),
            ));
        }
        stats.push(stat(None, app.elapsed_label(), "elapsed"));
        return stats;
    }
    let tally = Tally::of(app.rows());
    let mut stats = Vec::new();
    let mut count = |kind: LinkKind, one: &str, many: &str, always: bool| {
        let n = tally.count(kind);
        if n > 0 || always {
            stats.push(stat(Some(kind), n, &plural(n, one, many)));
        }
    };
    // Most needed first: four rows fit, and the rest give way.
    if app.showing_link_report() {
        count(LinkKind::Hardlink, "hardlink", "hardlinks", true);
        count(LinkKind::Symlink, "symlink", "symlinks", false);
        count(LinkKind::Failed, "failed", "failed", false);
        count(LinkKind::Planned, "not linked yet", "not linked yet", false);
        count(
            LinkKind::PlannedSymlink,
            "not linked yet",
            "not linked yet",
            false,
        );
        count(LinkKind::Skipped, "left alone", "left alone", false);
        count(LinkKind::Already, "already linked", "already linked", false);
    } else if app.has_report() {
        if tally.count(LinkKind::PlannedSymlink) > 0 {
            count(LinkKind::Planned, "to hardlink", "to hardlink", false);
            count(LinkKind::PlannedSymlink, "to symlink", "to symlink", true);
        } else {
            count(LinkKind::Planned, "to link", "to link", true);
        }
        count(LinkKind::Skipped, "left alone", "left alone", false);
        count(LinkKind::Already, "already linked", "already linked", false);
        stats.push(stat(
            None,
            group_digits(app.files_considered() as u64),
            "files scanned",
        ));
    }
    stats.truncate(4);
    stats
}

/// A count's line: its link drawing when `glyph` and it has one, its value
/// right-aligned in `value_width`, then its label.
fn stat_line(stat: &Stat, glyph: bool, value_width: usize) -> Line<'static> {
    let value_style = match stat.kind {
        Some(LinkKind::Failed) => Theme::failed(),
        Some(LinkKind::Symlink) => Theme::symlinked(),
        _ => Theme::strong(),
    };
    let mut spans = Vec::new();
    if glyph {
        let (drawing, style) = stat
            .kind
            .map_or(("   ", Theme::faint()), |kind| (kind.glyph(), kind.style()));
        spans.push(Span::styled(drawing, style));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        pad_left(&stat.value, value_width),
        value_style.add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(stat.label.clone(), Theme::dim()));
    Line::from(spans)
}

/// The bar's filled parts, as shares of its width.
fn bar_parts(app: &App) -> Vec<(f64, Style)> {
    if app.is_running() {
        return match app.run_progress() {
            Some((done, total)) if total > 0 => {
                vec![(done.min(total) as f64 / total as f64, Theme::progress())]
            }
            _ => Vec::new(),
        };
    }
    if !app.showing_link_report() {
        return Vec::new();
    }
    let tally = Tally::of(app.rows());
    let total: u64 =
        app.rows().iter().map(|row| row.size).sum::<u64>() - tally.bytes(LinkKind::Already);
    if total == 0 {
        return Vec::new();
    }
    [LinkKind::Hardlink, LinkKind::Symlink]
        .into_iter()
        .map(|kind| (tally.bytes(kind) as f64 / total as f64, kind.style()))
        .collect()
}

fn bar(width: usize, parts: &[(f64, Style)]) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used = 0;
    for (share, style) in parts {
        let cells = ((share * width as f64).round() as usize).min(width - used);
        if cells > 0 {
            spans.push(Span::styled("━".repeat(cells), *style));
            used += cells;
        }
    }
    spans.push(Span::styled("━".repeat(width - used), Theme::track()));
    Line::from(spans)
}

/// What a running job is doing, in a sentence.
fn phase_sentence(app: &App) -> String {
    match app.phase() {
        Some(Phase::Collecting) => "Walking both folders and sorting files by size".to_string(),
        Some(Phase::HashingSample) => {
            "Reading the first and last MiB of files that share a size".to_string()
        }
        Some(Phase::HashingFull) => match app.full_pass_progress() {
            Some(bytes) => format!("Reading matches in full: {bytes}"),
            None => "Reading matches in full to be sure they are copies".to_string(),
        },
        Some(Phase::Planning) => "Pairing each download with its library file".to_string(),
        Some(Phase::Applying) => "Replacing downloads with links".to_string(),
        None => app.activity_message(),
    }
}

/// What a test hardlink showed, for the line under the bar.
fn link_check_line(check: &LinkCheck) -> Option<Line<'static>> {
    let summary = check.summary()?;
    Some(match check {
        LinkCheck::Hardlinks => Line::from(vec![
            Span::styled("✓ ", Theme::hardlinked()),
            Span::styled(summary, Theme::dim()),
        ]),
        LinkCheck::Symlinks { .. } | LinkCheck::SymlinkOnly => {
            Line::from(Span::styled(format!("! {summary}"), Theme::symlinked()))
        }
        LinkCheck::Refused { .. } => {
            Line::from(Span::styled(format!("! {summary}"), Theme::failed()))
        }
        LinkCheck::Untested | LinkCheck::Testing | LinkCheck::NoLibraryFile => {
            Line::from(Span::styled(summary, Theme::dim()))
        }
    })
}

/// The line under the bar, left and right.
fn caption(app: &App) -> (Line<'static>, Line<'static>) {
    let finished = |what: &str| {
        Line::from(Span::styled(
            app.last_run_label()
                .map(|time| format!("{what} finished in {time}"))
                .unwrap_or_default(),
            Theme::faint(),
        ))
    };
    if app.is_running() {
        return (
            Line::from(Span::styled(phase_sentence(app), Theme::dim())),
            Line::default(),
        );
    }
    if app.showing_link_report() {
        let tally = Tally::of(app.rows());
        let mut left = Vec::new();
        for kind in [LinkKind::Hardlink, LinkKind::Symlink] {
            let bytes = tally.bytes(kind);
            if bytes > 0 {
                if !left.is_empty() {
                    left.push(Span::raw("   "));
                }
                let verb = if kind == LinkKind::Hardlink {
                    "hardlinked"
                } else {
                    "symlinked"
                };
                left.push(Span::styled(
                    format!("{} {verb}", format_size(bytes)),
                    kind.style(),
                ));
            }
        }
        // Already-linked pairs never took space twice; every other pair the
        // run did not link still does.
        let left_over = [
            LinkKind::Failed,
            LinkKind::Planned,
            LinkKind::PlannedSymlink,
            LinkKind::Skipped,
        ]
        .into_iter()
        .map(|kind| tally.bytes(kind))
        .sum::<u64>();
        let right = if left_over > 0 {
            Line::from(Span::styled(
                format!("{} not freed", format_size(left_over)),
                Theme::dim(),
            ))
        } else {
            finished("Link run")
        };
        return (Line::from(left), right);
    }
    let left = link_check_line(app.link_check()).unwrap_or_else(|| {
        Line::from(Span::styled(
            if app.planned_actions() > 0 {
                "Nothing freed yet"
            } else {
                ""
            },
            Theme::dim(),
        ))
    });
    (left, finished("Scan"))
}

/// The header of a terminal with room for it: the logo on the left, the big
/// number, its counts, the bar and the caption on the right.
pub(super) fn render_full(frame: &mut Frame, area: Rect, app: &App) {
    let logo = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: wordmark::WIDTH.min(area.width.saturating_sub(2)),
        height: wordmark::HEIGHT.min(area.height.saturating_sub(1)),
    };
    frame.render_widget(Paragraph::new(wordmark::lines()), logo);

    let x = logo.right() + 4;
    let zone = Rect {
        x,
        y: area.y + 1,
        width: area.right().saturating_sub(x + 2),
        height: area.height.saturating_sub(1),
    };
    let Some(hero) = hero(app) else {
        render_intro(frame, zone, app);
        return;
    };

    // The number in pixels with its unit beside it, then the counts at the
    // right: they drop their link drawings before the number gives up its
    // pixels for a line of text.
    let stats = stats(app);
    let value_width = stats
        .iter()
        .map(|stat| stat.value.chars().count())
        .max()
        .unwrap_or(0);
    let label_width = stats
        .iter()
        .map(|stat| stat.label.chars().count())
        .max()
        .unwrap_or(0);
    let stats_width = |glyph: bool| (if glyph { 5 } else { 0 }) + value_width + 2 + label_width;
    let words = hero.unit.chars().count().max(hero.caption.chars().count());
    let pixels = bignum::width(&hero.number).map(|width| usize::from(width) + 2 + words);
    let zone_width = usize::from(zone.width);
    let fits = |number: usize, glyph: bool| number + 3 + stats_width(glyph) <= zone_width;
    let (drawn, glyph) = match pixels {
        Some(number) if fits(number, true) => (true, true),
        Some(number) if fits(number, false) => (true, false),
        _ => (false, true),
    };
    let stats_width = u16::try_from(stats_width(glyph))
        .unwrap_or(u16::MAX)
        .min(zone.width);
    let lines: Vec<Line> = stats
        .iter()
        .map(|stat| stat_line(stat, glyph, value_width))
        .collect();
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: zone.right().saturating_sub(stats_width),
            width: stats_width,
            height: 4,
            ..zone
        },
    );

    let (face, depth) = Theme::number(hero.tone);
    match bignum::lines(&hero.number, face, depth).filter(|_| drawn) {
        Some(lines) => {
            let width = bignum::width(&hero.number).unwrap_or(0);
            frame.render_widget(
                Paragraph::new(lines),
                Rect {
                    width,
                    height: bignum::HEIGHT,
                    ..zone
                },
            );
            let text = vec![
                Line::from(Span::styled(hero.unit, Theme::number_unit(hero.tone))),
                Line::from(Span::styled(hero.caption, Theme::muted())),
            ];
            frame.render_widget(
                Paragraph::new(text),
                Rect {
                    x: zone.x + width + 2,
                    y: zone.y + 1,
                    width: u16::try_from(words).unwrap_or(0),
                    height: 2,
                },
            );
        }
        None => {
            let unit = if hero.unit.is_empty() || hero.unit == "%" {
                hero.unit.clone()
            } else {
                format!(" {}", hero.unit)
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{}{unit}", hero.number),
                    Theme::number_unit(hero.tone),
                ),
                Span::styled(format!(" {}", hero.caption), Theme::muted()),
            ]);
            frame.render_widget(
                Paragraph::new(line),
                Rect {
                    y: zone.y + 1,
                    height: 1,
                    width: zone.width.saturating_sub(stats_width + 2),
                    ..zone
                },
            );
        }
    }

    let bar_row = Rect {
        y: zone.y + 4,
        height: 1,
        ..zone
    };
    frame.render_widget(
        Paragraph::new(bar(usize::from(zone.width), &bar_parts(app))),
        bar_row,
    );
    let (left, right) = caption(app);
    let caption_row = Rect {
        y: zone.y + 5,
        height: 1,
        ..zone
    };
    // The right side gives way when the two do not fit apart.
    if line_width(&left) + 2 + line_width(&right) <= usize::from(caption_row.width) {
        frame.render_widget(Paragraph::new(right).right_aligned(), caption_row);
    }
    frame.render_widget(Paragraph::new(left), caption_row);
}

/// Before any scan: what DejaView does, in two sentences.
fn render_intro(frame: &mut Frame, area: Rect, app: &App) {
    let width = usize::from(area.width);
    let mut lines: Vec<Line> = wrap_text(
        "Find downloads that are copies of library files, and turn each one into a link so it takes no extra space.",
        width,
    )
    .into_iter()
    .map(|line| Line::from(Span::styled(line, Theme::strong())))
    .collect();
    lines.push(Line::raw(""));
    lines.extend(
        wrap_text(
            "Your torrents keep seeding. The library is only ever read.",
            width,
        )
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Theme::muted()))),
    );
    if let Some(check) = link_check_line(app.link_check()) {
        lines.push(Line::raw(""));
        lines.push(check);
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// What the dashboard is doing, in a few words, for the compact header.
fn status(app: &App) -> (String, Style) {
    if app.is_running() {
        let mut word = phase_word(app).to_string();
        word[..1].make_ascii_uppercase();
        return (word, Theme::progress());
    }
    if !app.has_paths() {
        return ("Choose your folders to begin".to_string(), Theme::planned());
    }
    if !app.has_report() {
        return ("Ready to scan".to_string(), Theme::planned());
    }
    if app.showing_link_report() {
        return ("Link run finished".to_string(), Theme::hardlinked());
    }
    if app.planned_actions() > 0 {
        ("Ready to review".to_string(), Theme::planned())
    } else {
        ("No duplicates found".to_string(), Theme::dim())
    }
}

/// The header of a short or narrow terminal: the number and its counts on
/// one line, what is going on on the next, what the run reads on the third.
/// Too narrow for every count, the line drops the file count first, so the
/// space counts stay whole.
pub(super) fn render_compact(frame: &mut Frame, area: Rect, app: &App) {
    let body = super::inset(area, 2);
    let width = usize::from(body.width);
    let mut first = Vec::new();
    if let Some(hero) = hero(app) {
        let unit = if hero.unit.is_empty() || hero.unit == "%" {
            hero.unit
        } else {
            format!(" {}", hero.unit)
        };
        first.push(Span::styled(
            format!("{}{unit}", hero.number),
            Theme::number_unit(hero.tone),
        ));
        first.push(Span::styled(format!(" {}", hero.caption), Theme::muted()));
    }
    let stats = stats(app);
    let cost = |stat: &Stat| stat.value.chars().count() + 1 + stat.label.chars().count() + 3;
    let mut used = line_width(&Line::from(first.clone()));
    for stat in &stats {
        let files = stat.label.contains("files") && stats.len() > 1;
        if used + cost(stat) > width || (files && used + cost(stat) + 8 > width) {
            continue;
        }
        used += cost(stat);
        first.push(Span::raw("   "));
        first.push(Span::styled(stat.value.clone(), Theme::strong()));
        first.push(Span::styled(format!(" {}", stat.label), Theme::dim()));
    }

    let (word, style) = status(app);
    let mut second = vec![
        Span::styled("● ", style),
        Span::styled(word, Theme::strong()),
    ];
    if let Some(time) = app.last_run_label().filter(|_| !app.is_running()) {
        second.push(Span::styled(
            format!("   finished in {time}"),
            Theme::faint(),
        ));
    }

    let third = if app.is_running() {
        let path = app
            .active_worker_paths()
            .first()
            .map(|(_, path)| path.to_string())
            .or_else(|| app.current_path().map(str::to_string));
        match path {
            Some(path) => Line::from(Span::styled(
                super::trim_keep_name(&path, width),
                Theme::dim(),
            )),
            None => Line::from(Span::styled(cut(&phase_sentence(app), width), Theme::dim())),
        }
    } else {
        caption(app).0
    };

    let lines = vec![Line::from(first), Line::from(second), third];
    let (top, bottom) = take_top(body, 3);
    frame.render_widget(Paragraph::new(lines), top);
    if bottom.height > 0 {
        rule(
            frame,
            Rect {
                height: 1,
                ..take_top(area, 3).1
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{app_with_episodes, app_with_link_report, screen};
    use super::*;
    use crate::protocol::{EntryStatus, LinkMethod, Mode};

    #[test]
    fn a_scan_report_shows_the_space_to_free_big() {
        let app = app_with_episodes(12);
        let text = screen(&app, 120, 36);
        assert!(text.contains("GiB") && text.contains("to free"), "{text}");
        assert!(text.contains("╌○╌  12  to link"), "{text}");
        assert!(text.contains('▀'), "the number in pixels:\n{text}");
    }

    #[test]
    fn a_link_report_counts_hardlinks_and_symlinks_apart() {
        let app = app_with_link_report();
        let text = screen(&app, 120, 36);
        assert!(text.contains("freed"), "{text}");
        assert!(text.contains("━●━  1  hardlink"), "{text}");
        assert!(text.contains("──▶  1  symlink"), "{text}");
        assert!(text.contains("─✗   1  failed"), "{text}");
        assert!(
            text.contains("1.9 GiB hardlinked   1.9 GiB symlinked"),
            "{text}"
        );
    }

    #[test]
    fn a_link_run_with_every_kind_keeps_its_unlinked_pairs_in_view() {
        let config = crate::config::Config {
            downloads: Some("/dl".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let entry = |name: &str, status: EntryStatus, method: LinkMethod| {
            crate::tui::model::LinkReportEntry {
                source: format!("/dl/{name}/{name}.mkv"),
                target: format!("/media/{name}.mkv"),
                size: 1_073_741_824,
                status,
                method: Some(method),
                ..Default::default()
            }
        };
        app.set_report_for_test(crate::protocol::RunReport {
            mode: Mode::Link,
            entries: vec![
                entry("A", EntryStatus::Applied, LinkMethod::Hardlink),
                entry("B", EntryStatus::Applied, LinkMethod::Symlink),
                entry("C", EntryStatus::Failed, LinkMethod::Hardlink),
                entry("D", EntryStatus::AlreadyLinked, LinkMethod::Hardlink),
                entry("E", EntryStatus::Planned, LinkMethod::TryHardlink),
                entry("F", EntryStatus::Skipped, LinkMethod::Hardlink),
            ],
            ..Default::default()
        });
        let text = screen(&app, 140, 36);
        assert!(text.contains("not linked yet"), "{text}");
        // Failed, unlinked and skipped pairs still take their space twice;
        // the already-linked one never did.
        assert!(text.contains("3.0 GiB not freed"), "{text}");
    }

    #[test]
    fn a_scan_with_planned_symlinks_counts_them_apart() {
        let config = crate::config::Config {
            downloads: Some("/dl".into()),
            media: Some("/media".into()),
            ..crate::config::Config::default()
        };
        let mut app = App::new(&config, false);
        let planned = |name: &str, method: LinkMethod| crate::tui::model::LinkReportEntry {
            source: format!("/dl/{name}/{name}.mkv"),
            target: format!("/media/{name}.mkv"),
            size: 1,
            status: EntryStatus::Planned,
            method: Some(method),
            ..Default::default()
        };
        app.set_report_for_test(crate::protocol::RunReport {
            mode: Mode::Scan,
            planned_actions: 3,
            total_saved_bytes: 3,
            entries: vec![
                planned("A", LinkMethod::TryHardlink),
                planned("B", LinkMethod::Symlink),
                planned("C", LinkMethod::Symlink),
            ],
            ..Default::default()
        });
        let text = screen(&app, 140, 36);
        assert!(text.contains("╌○╌  1  to hardlink"), "{text}");
        assert!(text.contains("╌╌▷  2  to symlink"), "{text}");
    }

    #[test]
    fn a_link_run_counts_what_it_linked_of_what_it_was_given() {
        let mut app = app_with_episodes(10);
        app.set_running_link_for_test(5, 10);
        let text = screen(&app, 120, 36);
        assert!(text.contains("5  of 10 linked"), "{text}");
        assert!(!text.contains("of 15"), "{text}");
    }

    #[test]
    fn a_long_caption_never_runs_into_the_time() {
        let mut app = app_with_episodes(1);
        app.set_link_check_for_test(LinkCheck::SymlinkOnly);
        app.set_last_run_for_test(std::time::Duration::from_secs(61));
        let text = screen(&app, 120, 36);
        let line = text
            .lines()
            .find(|line| line.contains("symlink-only"))
            .unwrap_or_else(|| panic!("the caption:\n{text}"));
        assert!(!line.contains("finished in"), "{line}");
        let wide = screen(&app, 200, 36);
        assert!(wide.contains("Scan finished in 01:01"), "{wide}");
    }

    #[test]
    fn a_scan_in_progress_shows_how_far_it_is() {
        let mut app = app_with_episodes(0);
        let _cancel = app.set_running_scan_for_test();
        app.set_hash_activity_for_test(Phase::HashingSample, &[]);
        app.set_hash_progress_for_test(412, 1290);
        let text = screen(&app, 120, 36);
        assert!(text.contains("%") && text.contains("hashing"), "{text}");
        assert!(text.contains("412  of 1,290 hashed"), "{text}");
        assert!(text.contains("Reading the first and last MiB"), "{text}");
    }

    #[test]
    fn a_first_run_says_what_dejaview_does() {
        let app = App::new(&crate::config::Config::default(), false);
        let text = screen(&app, 120, 36);
        assert!(
            text.contains("Find downloads that are copies of library files"),
            "{text}"
        );
        assert!(text.contains("The library is only ever read."), "{text}");
    }

    #[test]
    fn the_logo_shows_from_the_narrowest_full_header() {
        let app = app_with_episodes(1);
        for cols in [crate::tui::layout::FULL_HEADER_COLUMNS, 160] {
            let text = screen(&app, cols, crate::tui::layout::FULL_HEADER_ROWS);
            assert!(text.contains('▀'), "{cols} columns:\n{text}");
        }
    }

    #[test]
    fn a_short_terminal_keeps_counts_log_and_help_and_drops_the_logo() {
        let mut app = app_with_episodes(12);
        app.push_log_for_test("Found 12 duplicates.");
        for (cols, rows) in [(60, 20), (80, 24)] {
            let text = screen(&app, cols, rows);
            assert!(!text.contains('▀'), "{cols}x{rows}: no logo\n{text}");
            assert!(text.contains("12 to link"), "{cols}x{rows}: counts\n{text}");
            assert!(
                text.contains("Found 12 duplicates."),
                "{cols}x{rows}: log\n{text}"
            );
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
    fn a_narrow_header_keeps_the_space_counts_whole() {
        let mut app = app_with_episodes(3);
        app.set_report_for_test(crate::protocol::RunReport {
            mode: Mode::Scan,
            files_considered: 123_456,
            planned_actions: 3,
            total_saved_bytes: 4_500_000_000,
            entries: app.rows().to_vec(),
            ..Default::default()
        });
        let counts = |text: &str| {
            text.lines()
                .find(|line| line.contains("to free"))
                .unwrap_or_default()
                .to_string()
        };
        let text = screen(&app, 60, 20);
        let line = counts(&text);
        assert!(line.contains("4.2 GiB to free"), "{text}");
        assert!(line.contains("3 to link"), "{text}");
        assert!(
            !line.contains("files"),
            "the file count gives way first: {line}"
        );
        let wide = screen(&app, 80, 24);
        assert!(counts(&wide).contains("123,456 files scanned"), "{wide}");
    }
}
