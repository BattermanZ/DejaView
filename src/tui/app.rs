use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::style::Style;

use crate::config::Config;

use super::actions;
use super::events::{self, Event};
use super::model::{LinkReportEntry, RunReport};
use super::theme::Theme;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FocusPane {
    Left,
    Center,
    Right,
    Bottom,
}

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    JsonPreview(String),
}

pub struct App {
    pub strict: bool,
    pub focus: FocusPane,
    selected_row: usize,
    pub overlay: Overlay,
    report: Option<RunReport>,
    logs: VecDeque<String>,
    downloads: Option<PathBuf>,
    media: Option<PathBuf>,
    min_age: Option<u64>,
    threads: Option<usize>,
    should_quit: bool,
}

impl App {
    pub fn new(config: &Config, strict: bool) -> Self {
        let mut logs = VecDeque::new();
        logs.push_back("TUI started. Press ? for help.".to_string());
        Self {
            strict,
            focus: FocusPane::Center,
            selected_row: 0,
            overlay: Overlay::None,
            report: None,
            logs,
            downloads: config.downloads.clone(),
            media: config.media.clone(),
            min_age: config.min_age,
            threads: config.threads,
            should_quit: false,
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn tick_rate(&self) -> Duration {
        Duration::from_millis(150)
    }

    pub fn step(&mut self) -> Result<()> {
        let event = events::next_event(self.tick_rate())?;
        match event {
            Event::Tick => {}
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    self.handle_key(key.code, key.modifiers)?;
                }
            }
        }
        Ok(())
    }

    pub fn mode_label(&self) -> &str {
        match &self.report {
            Some(report) if report.mode == "link" => "Link",
            Some(_) => "Scan",
            None => "Idle",
        }
    }

    pub fn files_considered(&self) -> usize {
        self.report
            .as_ref()
            .map(|r| r.files_considered)
            .unwrap_or(0)
    }

    pub fn planned_actions(&self) -> usize {
        self.report.as_ref().map(|r| r.planned_actions).unwrap_or(0)
    }

    pub fn applied_actions(&self) -> usize {
        self.report.as_ref().map(|r| r.applied_actions).unwrap_or(0)
    }

    pub fn total_saved_bytes(&self) -> u64 {
        self.report
            .as_ref()
            .map(|r| r.total_saved_bytes)
            .unwrap_or(0)
    }

    pub fn selected_row(&self) -> usize {
        self.selected_row
    }

    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    pub fn downloads_label(&self) -> String {
        self.downloads
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not set>".to_string())
    }

    pub fn media_label(&self) -> String {
        self.media
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<not set>".to_string())
    }

    pub fn min_age_secs(&self) -> u64 {
        self.min_age.unwrap_or(300)
    }

    pub fn threads_label(&self) -> String {
        self.threads
            .map(|t| t.to_string())
            .unwrap_or_else(|| "auto".to_string())
    }

    pub fn rows(&self) -> &[LinkReportEntry] {
        self.report
            .as_ref()
            .map(|r| r.entries.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_details(&self) -> String {
        let Some(report) = &self.report else {
            return "No results yet. Press S to scan.".to_string();
        };

        let Some(entry) = report.entries.get(self.selected_row) else {
            return format!(
                "Run summary\n- mode: {}\n- files: {}\n- planned: {}\n- applied: {}\n- skipped by min-age: {}\n- warnings: {}",
                report.mode,
                report.files_considered,
                report.planned_actions,
                report.applied_actions,
                report.skipped_by_min_age,
                report.warning_total(),
            );
        };

        format!(
            "Selected action\n\nStatus: {}\nMethod: {}\nSource: {}\nTarget: {}\nFallback: {}\nError: {}",
            entry.status,
            entry.method.clone().unwrap_or_else(|| "-".to_string()),
            entry.source,
            entry.target,
            entry
                .fallback_reason
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            entry.error.clone().unwrap_or_else(|| "-".to_string())
        )
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.logs.iter().cloned().collect()
    }

    pub fn focus_style(&self, pane: FocusPane) -> Style {
        if self.focus == pane {
            Theme::accent()
        } else {
            Theme::base()
        }
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Result<()> {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.should_quit = true;
            return Ok(());
        }

        match &mut self.overlay {
            Overlay::Help => match code {
                KeyCode::Esc | KeyCode::Char('?') => self.overlay = Overlay::None,
                _ => {}
            },
            Overlay::JsonPreview(_) => match code {
                KeyCode::Esc | KeyCode::Char('j') | KeyCode::Char('J') => {
                    self.overlay = Overlay::None
                }
                _ => {}
            },
            Overlay::None => self.handle_main_key(code)?,
        }
        Ok(())
    }

    fn handle_main_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Tab => self.focus = next_focus(self.focus),
            KeyCode::BackTab => self.focus = prev_focus(self.focus),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('S') | KeyCode::Char('s') => self.run_scan()?,
            KeyCode::Char('L') | KeyCode::Char('l') => self.run_link()?,
            KeyCode::Char('R') | KeyCode::Char('r') => self.save_report()?,
            KeyCode::Char('J') => self.open_json_overlay()?,
            _ => {}
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows().len();
        if rows == 0 {
            self.selected_row = 0;
            return;
        }

        let current = self.selected_row as isize;
        let max = rows.saturating_sub(1) as isize;
        let next = (current + delta).clamp(0, max);
        self.selected_row = next as usize;
    }

    fn run_scan(&mut self) -> Result<()> {
        self.push_log("Running scan...".to_string());
        match actions::run_scan(self.strict) {
            Ok(report) => {
                let planned = report.planned_actions;
                self.selected_row = 0;
                self.report = Some(report);
                self.push_log(format!("Scan finished. Planned actions: {planned}"));
                Ok(())
            }
            Err(err) => {
                self.push_log(format!("Scan failed: {err}"));
                Ok(())
            }
        }
    }

    fn run_link(&mut self) -> Result<()> {
        self.push_log("Running link...".to_string());
        match actions::run_link(self.strict) {
            Ok(report) => {
                let applied = report.applied_actions;
                self.selected_row = 0;
                self.report = Some(report);
                self.push_log(format!("Link finished. Applied actions: {applied}"));
                Ok(())
            }
            Err(err) => {
                self.push_log(format!("Link failed: {err}"));
                Ok(())
            }
        }
    }

    fn save_report(&mut self) -> Result<()> {
        let Some(report) = &self.report else {
            self.push_log("No report to save yet.".to_string());
            return Ok(());
        };

        let path = self
            .downloads
            .as_ref()
            .map(|d| d.join(".dejaview_tui_report.json"))
            .unwrap_or_else(|| PathBuf::from("dejaview_tui_report.json"));

        actions::save_report(report, &path)?;
        self.push_log(format!("Saved report to {}", path.display()));
        Ok(())
    }

    fn open_json_overlay(&mut self) -> Result<()> {
        let Some(report) = &self.report else {
            self.push_log("No report available to preview.".to_string());
            return Ok(());
        };

        let json = serde_json::to_string_pretty(report)?;
        self.overlay = Overlay::JsonPreview(json);
        Ok(())
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() >= 64 {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }
}

fn next_focus(pane: FocusPane) -> FocusPane {
    match pane {
        FocusPane::Left => FocusPane::Center,
        FocusPane::Center => FocusPane::Right,
        FocusPane::Right => FocusPane::Bottom,
        FocusPane::Bottom => FocusPane::Left,
    }
}

fn prev_focus(pane: FocusPane) -> FocusPane {
    match pane {
        FocusPane::Left => FocusPane::Bottom,
        FocusPane::Center => FocusPane::Left,
        FocusPane::Right => FocusPane::Center,
        FocusPane::Bottom => FocusPane::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycles_forward() {
        assert_eq!(next_focus(FocusPane::Left), FocusPane::Center);
        assert_eq!(next_focus(FocusPane::Center), FocusPane::Right);
        assert_eq!(next_focus(FocusPane::Right), FocusPane::Bottom);
        assert_eq!(next_focus(FocusPane::Bottom), FocusPane::Left);
    }

    #[test]
    fn focus_cycles_backward() {
        assert_eq!(prev_focus(FocusPane::Left), FocusPane::Bottom);
        assert_eq!(prev_focus(FocusPane::Bottom), FocusPane::Right);
    }
}
