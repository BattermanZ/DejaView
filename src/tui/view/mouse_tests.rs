//! The mouse: what a click or the wheel does where the screen drew something.
//! Each test draws the screen, finds a piece of text on it, and clicks there,
//! as a person would.

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use super::render;
use super::test_support::{app_with_episodes, app_with_link_report};
use crate::tui::app::{App, ConfirmLinkMode, Overlay, test_runs};

const COLS: u16 = 120;
const ROWS: u16 = 40;

/// Draw `app` and return the screen's lines, a cell per char.
fn draw(app: &App) -> Vec<Vec<String>> {
    let backend = ratatui::backend::TestBackend::new(COLS, ROWS);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..ROWS)
        .map(|y| {
            (0..COLS)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

/// Where `text` starts on the screen `app` draws: the lowest line holding
/// it, since keys sit at the bottom of the screen and of a dialog.
fn find(app: &App, text: &str) -> (u16, u16) {
    let screen = draw(app);
    let wanted: Vec<char> = text.chars().collect();
    for (y, line) in screen.iter().enumerate().rev() {
        let chars: Vec<char> = line
            .iter()
            .map(|cell| cell.chars().next().unwrap_or(' '))
            .collect();
        if let Some(x) = chars
            .windows(wanted.len())
            .position(|window| window == wanted)
        {
            return (x as u16, y as u16);
        }
    }
    let text_screen: Vec<String> = screen.iter().map(|line| line.concat()).collect();
    panic!("{text:?} is not on the screen:\n{}", text_screen.join("\n"));
}

fn click(app: &mut App, (x, y): (u16, u16)) {
    draw(app);
    app.mouse_for_test(MouseEventKind::Down(MouseButton::Left), x, y);
}

fn wheel(app: &mut App, kind: MouseEventKind) {
    draw(app);
    app.mouse_for_test(kind, COLS / 2, ROWS / 2);
}

#[test]
fn a_click_on_a_footer_key_presses_it() {
    let mut app = app_with_episodes(3);
    let at = find(&app, "? help");
    click(&mut app, at);
    assert!(matches!(app.overlay(), Overlay::Help));

    let mut app = app_with_episodes(3);
    // The label is part of the button, not only the key.
    let (x, y) = find(&app, "link all 3");
    click(&mut app, (x + 3, y));
    assert!(matches!(
        app.overlay(),
        Overlay::ConfirmLink(ConfirmLinkMode::All)
    ));
}

#[test]
fn a_click_between_footer_keys_does_nothing() {
    let mut app = app_with_episodes(3);
    let (x, y) = find(&app, "? help");
    click(&mut app, (x - 2, y));
    assert!(matches!(app.overlay(), Overlay::None));
    assert!(!app.should_quit());
}

#[test]
fn a_click_on_a_row_selects_it() {
    let mut app = app_with_episodes(5);
    assert_eq!(app.selected_row(), 0);
    let at = find(&app, "S01E04");
    click(&mut app, at);
    assert_eq!(app.selected_row(), 3);
}

#[test]
fn a_click_on_a_release_heading_selects_its_first_episode() {
    let mut app = app_with_episodes(5);
    app.handle_key_for_test(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(app.selected_row(), 4);
    let at = find(&app, "▾ ");
    click(&mut app, at);
    assert_eq!(app.selected_row(), 0);
}

#[test]
fn the_wheel_moves_the_selection() {
    let mut app = app_with_episodes(5);
    wheel(&mut app, MouseEventKind::ScrollDown);
    wheel(&mut app, MouseEventKind::ScrollDown);
    assert_eq!(app.selected_row(), 2);
    wheel(&mut app, MouseEventKind::ScrollUp);
    assert_eq!(app.selected_row(), 1);
}

#[test]
fn a_click_on_a_dialog_button_answers_it() {
    let mut app = app_with_episodes(3);
    app.handle_key_for_test(KeyCode::Char('L'), KeyModifiers::NONE);
    let at = find(&app, " n  cancel");
    click(&mut app, at);
    assert!(matches!(app.overlay(), Overlay::None));
    assert!(test_runs::take().is_empty(), "cancel starts nothing");

    app.handle_key_for_test(KeyCode::Char('L'), KeyModifiers::NONE);
    let at = find(&app, " y  link all 3");
    click(&mut app, at);
    assert!(matches!(app.overlay(), Overlay::None));
    assert_eq!(test_runs::take().len(), 1, "y starts the link run");
}

#[test]
fn a_row_behind_a_dialog_cannot_be_clicked() {
    let mut app = app_with_episodes(5);
    let row = find(&app, "S01E04");
    app.handle_key_for_test(KeyCode::Char('L'), KeyModifiers::NONE);
    click(&mut app, row);
    assert_eq!(app.selected_row(), 0);
    let footer = find(&app, "q quit");
    click(&mut app, footer);
    assert!(
        !app.should_quit(),
        "the footer behind the dialog is dimmed, not live"
    );
    assert!(matches!(app.overlay(), Overlay::ConfirmLink(_)));
}

#[test]
fn a_click_outside_help_closes_it_and_inside_keeps_it() {
    let mut app = app_with_link_report();
    app.handle_key_for_test(KeyCode::Char('?'), KeyModifiers::NONE);
    let inside = find(&app, "Keys");
    click(&mut app, inside);
    assert!(matches!(app.overlay(), Overlay::Help));
    click(&mut app, (0, 0));
    assert!(matches!(app.overlay(), Overlay::None));
}

#[test]
fn the_wheel_scrolls_help_and_leaves_the_rows_alone() {
    let mut app = app_with_episodes(5);
    app.handle_key_for_test(KeyCode::Char('?'), KeyModifiers::NONE);
    // A short terminal, so Help is longer than its dialog.
    let backend = ratatui::backend::TestBackend::new(COLS, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render(frame, &app)).expect("draw");
    app.mouse_for_test(MouseEventKind::ScrollDown, COLS / 2, 10);
    assert!(app.overlay_scroll() > 0);
    assert_eq!(app.selected_row(), 0);
    app.mouse_for_test(MouseEventKind::ScrollUp, COLS / 2, 10);
    assert_eq!(app.overlay_scroll(), 0);
}

#[test]
fn a_click_on_esc_closes_the_json_preview() {
    let mut app = app_with_link_report();
    app.handle_key_for_test(KeyCode::Char('J'), KeyModifiers::NONE);
    assert!(matches!(app.overlay(), Overlay::JsonPreview(_)));
    let at = find(&app, " Esc  close");
    click(&mut app, at);
    assert!(matches!(app.overlay(), Overlay::None));
}

/// Where `text` sits on the lowest line holding it, at `cols` by `rows`.
fn find_at(app: &App, text: &str, cols: u16, rows: u16) -> (u16, u16) {
    let backend = ratatui::backend::TestBackend::new(cols, rows);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer();
    let wanted: Vec<char> = text.chars().collect();
    for y in (0..rows).rev() {
        let line: Vec<char> = (0..cols)
            .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        if let Some(x) = line
            .windows(wanted.len())
            .position(|window| window == wanted)
        {
            return (x as u16, y);
        }
    }
    panic!("{text:?} is not on the {cols}x{rows} screen");
}

#[test]
fn on_a_narrow_screen_a_click_on_cancel_still_cancels() {
    for cols in [24u16, 28, 32, 40] {
        let mut app = app_with_episodes(3);
        app.handle_key_for_test(KeyCode::Char('L'), KeyModifiers::NONE);
        let (x, y) = find_at(&app, " n ", cols, ROWS);
        // Draw at this size, then click the n the person sees.
        find_at(&app, " n ", cols, ROWS);
        app.mouse_for_test(MouseEventKind::Down(MouseButton::Left), x + 1, y);
        assert!(
            test_runs::take().is_empty(),
            "{cols} columns: cancel started a link"
        );
        assert!(
            matches!(app.overlay(), Overlay::None),
            "{cols} columns: still open"
        );
    }
}
