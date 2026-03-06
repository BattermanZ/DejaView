use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::config::{load_config, save_config, validate_runtime_paths, Config};

use super::actions;
use super::events::{self, Event};
use super::model::{LinkReportEntry, RunReport};

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
    ConfigEditor(ConfigEditor),
    ConfirmLink,
    BlockingNotice(String),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConfigField {
    Downloads,
    Media,
}

#[derive(Clone, Debug)]
pub struct ConfigEditor {
    pub downloads: String,
    pub media: String,
    pub active_field: ConfigField,
    pub error: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum JobKind {
    Scan,
    Link,
}

struct RunningJob {
    kind: JobKind,
    receiver: Receiver<Result<RunReport>>,
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
    running_job: Option<RunningJob>,
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
            running_job: None,
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
            Event::Tick => self.poll_running_job(),
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    self.handle_key(key.code, key.modifiers)?;
                }
            }
        }
        Ok(())
    }

    pub fn mode_label(&self) -> &str {
        if let Some(job) = &self.running_job {
            return match job.kind {
                JobKind::Scan => "Scanning",
                JobKind::Link => "Linking",
            };
        }
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

    pub fn selected_entry(&self) -> Option<&LinkReportEntry> {
        self.rows().get(self.selected_row)
    }

    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    pub fn config_editor(&self) -> Option<&ConfigEditor> {
        match &self.overlay {
            Overlay::ConfigEditor(editor) => Some(editor),
            _ => None,
        }
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

    pub fn log_lines(&self) -> Vec<String> {
        self.logs.iter().cloned().collect()
    }

    pub fn activity_phase(&self) -> &'static str {
        if let Some(job) = &self.running_job {
            return match job.kind {
                JobKind::Scan => "SCANNING LIBRARY",
                JobKind::Link => "LINKING DUPLICATES",
            };
        }
        match &self.report {
            Some(report) if report.mode == "link" => "LAST RUN COMPLETE",
            Some(_) => "READY TO REVIEW",
            None => "READY TO SCAN",
        }
    }

    pub fn progress_ratio(&self) -> f64 {
        match &self.running_job {
            Some(job) => match job.kind {
                JobKind::Scan => 0.35,
                JobKind::Link => 0.75,
            },
            None => match &self.report {
                Some(_) => 1.0,
                None => 0.0,
            },
        }
    }

    pub fn progress_label(&self) -> String {
        match &self.running_job {
            Some(job) => match job.kind {
                JobKind::Scan => "SCAN".to_string(),
                JobKind::Link => "LINK".to_string(),
            },
            None => {
                if self.report.is_some() {
                    "DONE".to_string()
                } else {
                    "IDLE".to_string()
                }
            }
        }
    }

    pub fn throughput_label(&self) -> String {
        if self.running_job.is_some() {
            return format!(
                "subprocess active  |  waiting for final report  |  {} rows currently visible",
                self.rows().len()
            );
        }
        format!(
            "{} files  |  {} planned  |  {} applied",
            self.files_considered(),
            self.planned_actions(),
            self.applied_actions()
        )
    }

    pub fn last_log_line(&self) -> String {
        self.logs
            .back()
            .cloned()
            .unwrap_or_else(|| "No activity yet.".to_string())
    }

    fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Result<()> {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.request_quit();
            return Ok(());
        }

        match &self.overlay {
            Overlay::Help => match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None
                }
                _ => {}
            },
            Overlay::JsonPreview(_) => match code {
                KeyCode::Esc | KeyCode::Char('j') | KeyCode::Char('J') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None
                }
                _ => {}
            },
            Overlay::ConfigEditor(_) => self.handle_config_editor_key(code)?,
            Overlay::ConfirmLink => match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.overlay = Overlay::None;
                    self.start_link_job();
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None;
                    self.push_log("Link run cancelled.".to_string());
                }
                _ => {}
            },
            Overlay::BlockingNotice(_) => match code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.overlay = Overlay::None,
                _ => {}
            },
            Overlay::None => self.handle_main_key(code)?,
        }
        Ok(())
    }

    fn handle_main_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Tab => self.focus = next_focus(self.focus),
            KeyCode::BackTab => self.focus = prev_focus(self.focus),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('e') => self.open_config_editor(),
            KeyCode::Char('S') | KeyCode::Char('s') => self.start_scan_job(),
            KeyCode::Char('L') => self.overlay = Overlay::ConfirmLink,
            KeyCode::Char('l') => {
                self.push_log("Per-group link intent is not implemented yet. Use uppercase L to confirm a full link run.".to_string());
            }
            KeyCode::Char('R') | KeyCode::Char('r') => {
                if let Err(err) = self.save_report() {
                    self.push_log(format!("Save report failed: {err}"));
                }
            }
            KeyCode::Char('J') => {
                if let Err(err) = self.open_json_overlay() {
                    self.push_log(format!("Open JSON preview failed: {err}"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn open_config_editor(&mut self) {
        self.overlay = Overlay::ConfigEditor(ConfigEditor {
            downloads: self.downloads_label_or_empty(),
            media: self.media_label_or_empty(),
            active_field: ConfigField::Downloads,
            error: None,
        });
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

    fn start_scan_job(&mut self) {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return;
        }
        self.push_log("Running scan...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(actions::run_scan(strict));
        });
        self.running_job = Some(RunningJob {
            kind: JobKind::Scan,
            receiver: rx,
        });
    }

    fn start_link_job(&mut self) {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return;
        }
        self.push_log("Running link...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(actions::run_link(strict));
        });
        self.running_job = Some(RunningJob {
            kind: JobKind::Link,
            receiver: rx,
        });
    }

    fn handle_config_editor_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                self.push_log("Config editor cancelled.".to_string());
            }
            KeyCode::Tab | KeyCode::Down => self.cycle_config_field(true),
            KeyCode::BackTab | KeyCode::Up => self.cycle_config_field(false),
            KeyCode::Enter => {
                if matches!(
                    self.config_editor().map(|e| e.active_field),
                    Some(ConfigField::Downloads)
                ) {
                    self.cycle_config_field(true);
                } else {
                    self.save_config_from_editor()?;
                }
            }
            KeyCode::Backspace => self.edit_config_field(|field| {
                field.pop();
            }),
            KeyCode::Char(c) => self.edit_config_field(|field| field.push(c)),
            _ => {}
        }
        Ok(())
    }

    fn cycle_config_field(&mut self, forward: bool) {
        if let Overlay::ConfigEditor(editor) = &mut self.overlay {
            editor.active_field = match (editor.active_field, forward) {
                (ConfigField::Downloads, true) => ConfigField::Media,
                (ConfigField::Media, true) => ConfigField::Downloads,
                (ConfigField::Downloads, false) => ConfigField::Media,
                (ConfigField::Media, false) => ConfigField::Downloads,
            };
            editor.error = None;
        }
    }

    fn edit_config_field<F>(&mut self, mut update: F)
    where
        F: FnMut(&mut String),
    {
        if let Overlay::ConfigEditor(editor) = &mut self.overlay {
            let field = match editor.active_field {
                ConfigField::Downloads => &mut editor.downloads,
                ConfigField::Media => &mut editor.media,
            };
            update(field);
            editor.error = None;
        }
    }

    fn save_config_from_editor(&mut self) -> Result<()> {
        self.save_config_from_editor_with(load_config, save_config)
    }

    fn save_config_from_editor_with<L, S>(&mut self, load_fn: L, save_fn: S) -> Result<()>
    where
        L: FnOnce() -> Result<Config>,
        S: FnOnce(&Config) -> Result<()>,
    {
        let Some(editor) = self.config_editor().cloned() else {
            return Ok(());
        };

        let config = match self.normalized_config_from_editor(load_fn, &editor) {
            Ok(config) => config,
            Err(err) => {
                self.set_config_editor_error(err.to_string());
                return Ok(());
            }
        };

        if let Err(err) = save_fn(&config) {
            self.set_config_editor_error(format!("Failed to save config: {err}"));
            return Ok(());
        }

        self.downloads = config.downloads.clone();
        self.media = config.media.clone();
        self.overlay = Overlay::None;
        self.push_log("Saved TUI path configuration.".to_string());
        Ok(())
    }

    fn normalized_config_from_editor<L>(&self, load_fn: L, editor: &ConfigEditor) -> Result<Config>
    where
        L: FnOnce() -> Result<Config>,
    {
        let mut config = load_fn()?;
        config.downloads = Some(std::fs::canonicalize(editor.downloads.trim())?);
        config.media = Some(std::fs::canonicalize(editor.media.trim())?);
        validate_runtime_paths(&config)?;
        Ok(config)
    }

    fn set_config_editor_error(&mut self, error: String) {
        if let Overlay::ConfigEditor(current) = &mut self.overlay {
            current.error = Some(error);
        }
    }

    fn downloads_label_or_empty(&self) -> String {
        self.downloads
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn media_label_or_empty(&self) -> String {
        self.media
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    fn request_quit(&mut self) {
        if self.running_job.is_some() {
            self.overlay = Overlay::BlockingNotice(
                "A run is still active. Quitting is disabled until it finishes so the TUI cannot detach from a live filesystem operation.".to_string(),
            );
            self.push_log("Quit blocked while a run is active.".to_string());
        } else if !matches!(self.overlay, Overlay::None) {
            self.overlay = Overlay::None;
        } else {
            self.should_quit = true;
        }
    }

    fn poll_running_job(&mut self) {
        let completion = if let Some(job) = self.running_job.as_mut() {
            match job.receiver.try_recv() {
                Ok(result) => Some((job.kind, result)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some((
                    job.kind,
                    Err(anyhow::anyhow!("worker thread disconnected unexpectedly")),
                )),
            }
        } else {
            None
        };

        if let Some((kind, result)) = completion {
            self.running_job = None;
            match result {
                Ok(report) => {
                    let planned = report.planned_actions;
                    let applied = report.applied_actions;
                    self.selected_row = 0;
                    self.report = Some(report);
                    match kind {
                        JobKind::Scan => {
                            self.push_log(format!("Scan finished. Planned actions: {planned}"))
                        }
                        JobKind::Link => {
                            self.push_log(format!("Link finished. Applied actions: {applied}"))
                        }
                    }
                }
                Err(err) => {
                    let label = match kind {
                        JobKind::Scan => "Scan",
                        JobKind::Link => "Link",
                    };
                    self.push_log(format!("{label} failed: {err}"));
                }
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
    use super::super::model::{RunReport, WarningSummary};
    use super::*;
    use crossterm::event::KeyCode;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dejaview_tui_{prefix}_{}_{}",
            std::process::id(),
            ts
        ))
    }

    fn sample_report() -> RunReport {
        RunReport {
            mode: "scan".to_string(),
            dry_run: true,
            strict: false,
            files_considered: 2,
            planned_actions: 1,
            applied_actions: 0,
            total_saved_bytes: 1024,
            skipped_by_min_age: 0,
            warnings: WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![],
        }
    }

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

    #[test]
    fn save_report_error_does_not_abort_ui() {
        let temp_file = unique_temp_path("report_parent_file");
        fs::write(&temp_file, b"not a directory").expect("create temp file");

        let config = Config {
            downloads: Some(temp_file.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config, false);
        app.report = Some(sample_report());

        app.handle_main_key(KeyCode::Char('r'))
            .expect("key handling should not fail");
        assert!(!app.should_quit());
        assert!(app
            .logs
            .iter()
            .any(|line| line.starts_with("Save report failed:")));

        fs::remove_file(temp_file).expect("cleanup temp file");
    }

    #[test]
    fn prevents_parallel_runs_when_job_already_active() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (_tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob {
            kind: JobKind::Scan,
            receiver: rx,
        });

        app.start_link_job();
        assert!(app
            .logs
            .iter()
            .any(|line| line == "A run is already in progress."));
    }

    #[test]
    fn poll_running_job_applies_result() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob {
            kind: JobKind::Scan,
            receiver: rx,
        });
        tx.send(Ok(sample_report())).expect("send result");

        app.poll_running_job();
        assert!(app.running_job.is_none());
        assert_eq!(app.files_considered(), 2);
        assert!(app.logs.iter().any(|line| line.contains("Scan finished")));
    }

    #[test]
    fn quit_is_blocked_while_job_is_running() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (_tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob {
            kind: JobKind::Link,
            receiver: rx,
        });

        app.handle_main_key(KeyCode::Char('q'))
            .expect("key handling should not fail");

        assert!(!app.should_quit());
        assert!(matches!(app.overlay, Overlay::BlockingNotice(_)));
        assert!(app
            .logs
            .iter()
            .any(|line| line == "Quit blocked while a run is active."));
    }

    #[test]
    fn lower_l_does_not_start_link_run() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('l'))
            .expect("key handling should not fail");

        assert!(app.running_job.is_none());
        assert!(app.logs.iter().any(|line| line.contains("uppercase L")));
    }

    #[test]
    fn uppercase_l_opens_confirmation_overlay() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('L'))
            .expect("key handling should not fail");

        assert!(matches!(app.overlay, Overlay::ConfirmLink));
        assert!(app.running_job.is_none());
    }

    #[test]
    fn q_closes_help_overlay_before_quitting() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.overlay = Overlay::Help;

        app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE)
            .expect("key handling should not fail");

        assert!(matches!(app.overlay, Overlay::None));
        assert!(!app.should_quit());
    }

    #[test]
    fn edit_key_opens_config_editor() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('e'))
            .expect("key handling should not fail");

        assert!(matches!(app.overlay, Overlay::ConfigEditor(_)));
    }

    #[test]
    fn config_editor_updates_active_field_text() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.open_config_editor();

        app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE)
            .expect("key handling should not fail");
        app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE)
            .expect("key handling should not fail");

        let editor = app.config_editor().expect("editor should be open");
        assert_eq!(editor.downloads, "/t");
        assert_eq!(editor.active_field, ConfigField::Downloads);
    }

    #[test]
    fn config_editor_validation_error_stays_in_overlay() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.overlay = Overlay::ConfigEditor(ConfigEditor {
            downloads: "/does/not/exist".to_string(),
            media: "/also/missing".to_string(),
            active_field: ConfigField::Media,
            error: None,
        });

        app.save_config_from_editor_with(|| Ok(Config::default()), |_| Ok(()))
            .expect("save flow should not hard fail");

        let editor = app.config_editor().expect("editor should stay open");
        assert!(editor.error.is_some());
    }

    #[test]
    fn config_editor_persists_valid_paths() {
        let root = unique_temp_path("config_editor_save");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads");
        fs::create_dir_all(&media).expect("create media");

        let config = Config::default();
        let mut app = App::new(&config, false);
        app.overlay = Overlay::ConfigEditor(ConfigEditor {
            downloads: downloads.display().to_string(),
            media: media.display().to_string(),
            active_field: ConfigField::Media,
            error: None,
        });

        let saved = Arc::new(Mutex::new(None::<Config>));
        let saved_clone = Arc::clone(&saved);
        app.save_config_from_editor_with(
            || {
                Ok(Config {
                    downloads: Some(PathBuf::from("/stale/downloads")),
                    media: Some(PathBuf::from("/stale/media")),
                    min_age: Some(900),
                    threads: Some(8),
                })
            },
            |config| {
                *saved_clone.lock().expect("lock saved config") = Some(Config {
                    downloads: config.downloads.clone(),
                    media: config.media.clone(),
                    min_age: config.min_age,
                    threads: config.threads,
                });
                Ok(())
            },
        )
        .expect("save should succeed");

        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.downloads, Some(downloads.clone()));
        assert_eq!(app.media, Some(media.clone()));
        assert_eq!(
            saved
                .lock()
                .expect("lock saved config")
                .as_ref()
                .expect("config was saved")
                .downloads,
            Some(downloads)
        );
        assert_eq!(
            saved
                .lock()
                .expect("lock saved config")
                .as_ref()
                .expect("config was saved")
                .min_age,
            Some(900)
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn config_editor_save_failure_stays_in_overlay() {
        let root = unique_temp_path("config_editor_save_failure");
        let downloads = root.join("downloads");
        let media = root.join("media");
        fs::create_dir_all(&downloads).expect("create downloads");
        fs::create_dir_all(&media).expect("create media");

        let config = Config::default();
        let mut app = App::new(&config, false);
        app.overlay = Overlay::ConfigEditor(ConfigEditor {
            downloads: downloads.display().to_string(),
            media: media.display().to_string(),
            active_field: ConfigField::Media,
            error: None,
        });

        app.save_config_from_editor_with(
            || Ok(Config::default()),
            |_| Err(anyhow::anyhow!("disk full")),
        )
        .expect("save flow should not hard fail");

        let editor = app.config_editor().expect("editor should stay open");
        assert!(editor
            .error
            .as_ref()
            .expect("save error")
            .contains("Failed to save config"));

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }
}
