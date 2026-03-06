use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct DashboardLayout {
    pub top: Rect,
    pub center_left: Rect,
    pub center_main: Rect,
    pub center_right: Rect,
    pub bottom_log: Rect,
    pub footer: Rect,
}

pub fn dashboard(area: Rect) -> DashboardLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(7),
            Constraint::Length(2),
        ])
        .split(area);

    let center = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(23),
            Constraint::Percentage(52),
            Constraint::Percentage(25),
        ])
        .split(vertical[1]);

    DashboardLayout {
        top: vertical[0],
        center_left: center[0],
        center_main: center[1],
        center_right: center[2],
        bottom_log: vertical[2],
        footer: vertical[3],
    }
}
