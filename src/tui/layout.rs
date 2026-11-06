use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Below this many rows, or [`SIDEBAR_COLUMNS`] columns, the header drops
/// the wordmark and the metric cards for one line of counts, so the results
/// keep their room.
pub const FULL_HEADER_ROWS: u16 = 30;
/// Below this many columns the Workspace sidebar is hidden: its paths and
/// keys are in the footer and in Help, and the results need the width.
pub const SIDEBAR_COLUMNS: u16 = 100;
/// Below this many columns the Inspector is hidden too.
pub const INSPECTOR_COLUMNS: u16 = 70;
/// Below this many rows the Run Log keeps one line and the footer its border.
const TINY_ROWS: u16 = 22;

pub struct DashboardLayout {
    pub top: Rect,
    /// The header is one block of counts and activity, without the wordmark.
    pub compact_header: bool,
    pub center_left: Option<Rect>,
    pub center_main: Rect,
    pub center_right: Option<Rect>,
    pub bottom_log: Rect,
    pub footer: Rect,
    /// The footer has room for the line above its keys.
    pub footer_border: bool,
}

pub fn dashboard(area: Rect) -> DashboardLayout {
    let compact_header = area.height < FULL_HEADER_ROWS || area.width < SIDEBAR_COLUMNS;
    let tiny = area.height < TINY_ROWS;
    let (top, log, footer) = match (compact_header, tiny) {
        (false, _) => (9, 6, 2),
        (true, false) => (6, 4, 2),
        (true, true) => (6, 3, 1),
    };
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top),
            Constraint::Min(5),
            Constraint::Length(log),
            Constraint::Length(footer),
        ])
        .split(area);

    let (center_left, center_main, center_right) = if area.width >= SIDEBAR_COLUMNS {
        let center = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(57),
                Constraint::Percentage(23),
            ])
            .split(vertical[1]);
        (Some(center[0]), center[1], Some(center[2]))
    } else if area.width >= INSPECTOR_COLUMNS {
        let center = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(vertical[1]);
        (None, center[0], Some(center[1]))
    } else {
        (None, vertical[1], None)
    };

    DashboardLayout {
        top: vertical[0],
        compact_header,
        center_left,
        center_main,
        center_right,
        bottom_log: vertical[2],
        footer: vertical[3],
        footer_border: !tiny,
    }
}
