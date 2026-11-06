use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Below this many rows, or [`FULL_HEADER_COLUMNS`] columns, the header
/// drops the logo and the big number for three lines of counts and
/// activity, so the pairs keep their room.
pub const FULL_HEADER_ROWS: u16 = 30;
pub const FULL_HEADER_COLUMNS: u16 = 100;
/// From this many columns the pairs show the library file beside the
/// download; below it, the details panel names it.
pub const LIBRARY_COLUMNS: u16 = 96;
/// Below this many rows the details panel gives way and the footer loses the
/// rule above its keys.
const DETAILS_ROWS: u16 = 22;
/// From this many rows the log shows its two newest lines.
const TWO_LOG_LINES_ROWS: u16 = 34;

pub struct DashboardLayout {
    pub header: Rect,
    /// The header is three lines of counts and activity, without the logo.
    pub compact_header: bool,
    /// The pairs, or what stands in for them.
    pub body: Rect,
    /// The selected pair, told in full.
    pub details: Option<Rect>,
    pub log: Rect,
    pub footer: Rect,
    /// The footer has room for the rule above its keys.
    pub footer_rule: bool,
}

pub fn dashboard(area: Rect) -> DashboardLayout {
    let compact_header = area.height < FULL_HEADER_ROWS || area.width < FULL_HEADER_COLUMNS;
    let roomy = area.height >= DETAILS_ROWS;
    let header = if compact_header { 4 } else { 8 };
    let details = match area.height {
        h if h >= TWO_LOG_LINES_ROWS => 6,
        h if h >= DETAILS_ROWS => 5,
        _ => 0,
    };
    let log = if area.height >= TWO_LOG_LINES_ROWS {
        2
    } else {
        1
    };
    let footer = if roomy { 2 } else { 1 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header),
            Constraint::Min(4),
            Constraint::Length(details),
            Constraint::Length(log),
            Constraint::Length(footer),
        ])
        .split(area);
    DashboardLayout {
        header: rows[0],
        compact_header,
        body: rows[1],
        details: roomy.then_some(rows[2]),
        log: rows[3],
        footer: rows[4],
        footer_rule: roomy,
    }
}
