use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::config::{Config, load_config, save_config, validate_runtime_paths};

use super::actions;
use super::events::{self, Event};
use super::model::{LinkReportEntry, RunReport, TuiEvent};

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
    receiver: Receiver<Result<TuiEvent>>,
}

pub struct App {
    pub strict: bool,
    selected_row: usize,
    pub overlay: Overlay,
    report: Option<RunReport>,
    logs: VecDeque<String>,
    downloads: Option<PathBuf>,
    media: Option<PathBuf>,
    min_age: Option<u64>,
    threads: Option<usize>,
    expand_paths: bool,
    current_phase: Option<String>,
    current_path: Option<String>,
    latest_activity_message: Option<String>,
    live_files_seen: Option<usize>,
    live_groups_found: Option<usize>,
    live_planned_actions: Option<usize>,
    live_applied_actions: Option<usize>,
    hash_progress_done: Option<usize>,
    hash_progress_total: Option<usize>,
    active_workers: BTreeMap<usize, String>,
    job_started_at: Option<Instant>,
    last_run_duration: Option<Duration>,
    running_job: Option<RunningJob>,
    should_quit: bool,
}

impl App {
    pub fn new(config: &Config, strict: bool) -> Self {
        let mut logs = VecDeque::new();
        logs.push_back("TUI started. Press ? for help.".to_string());
        Self {
            strict,
            selected_row: 0,
            overlay: Overlay::None,
            report: None,
            logs,
            downloads: config.downloads.clone(),
            media: config.media.clone(),
            min_age: config.min_age,
            threads: config.threads,
            expand_paths: false,
            current_phase: None,
            current_path: None,
            latest_activity_message: None,
            live_files_seen: None,
            live_groups_found: None,
            live_planned_actions: None,
            live_applied_actions: None,
            hash_progress_done: None,
            hash_progress_total: None,
            active_workers: BTreeMap::new(),
            job_started_at: None,
            last_run_duration: None,
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
        self.live_files_seen
            .or_else(|| self.report.as_ref().map(|r| r.files_considered))
            .unwrap_or(0)
    }

    pub fn planned_actions(&self) -> usize {
        self.live_planned_actions
            .or_else(|| self.report.as_ref().map(|r| r.planned_actions))
            .unwrap_or(0)
    }

    pub fn applied_actions(&self) -> usize {
        self.live_applied_actions
            .or_else(|| self.report.as_ref().map(|r| r.applied_actions))
            .unwrap_or(0)
    }

    pub fn total_saved_bytes(&self) -> u64 {
        self.report
            .as_ref()
            .map(|r| r.total_saved_bytes)
            .unwrap_or(0)
    }

    pub fn saved_bytes(&self) -> u64 {
        self.report
            .as_ref()
            .map(|report| {
                if report.mode != "link" {
                    return 0;
                }
                report
                    .entries
                    .iter()
                    .filter(|entry| entry.status.eq_ignore_ascii_case("applied"))
                    .map(|entry| entry.size)
                    .sum()
            })
            .unwrap_or(0)
    }

    pub fn potential_saved_bytes(&self) -> u64 {
        self.report
            .as_ref()
            .map(|report| {
                if report.mode == "link" {
                    report
                        .entries
                        .iter()
                        .filter(|entry| !entry.status.eq_ignore_ascii_case("applied"))
                        .map(|entry| entry.size)
                        .sum()
                } else {
                    report.total_saved_bytes
                }
            })
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
        if let Some(phase) = self.current_phase.as_deref() {
            return display_phase(phase);
        }
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

    pub fn throughput_label(&self) -> String {
        if self.running_job.is_some() {
            if let (Some(done), Some(total)) = (self.hash_progress_done, self.hash_progress_total) {
                return match self.current_phase.as_deref() {
                    Some("hashing_full") => format!("{done} / {total} duplicate files verified"),
                    _ => format!("{done} / {total} files hashed"),
                };
            }
            if matches!(self.current_phase.as_deref(), Some("applying"))
                && let (Some(applied), Some(planned)) =
                    (self.live_applied_actions, self.live_planned_actions)
            {
                return format!("{applied} / {planned} actions applied");
            }
            let mut parts = Vec::new();
            if let Some(files) = self.live_files_seen {
                parts.push(format!("{files} files found"));
            }
            if let Some(groups) = self.live_groups_found {
                parts.push(format!("{groups} groups"));
            }
            if let Some(planned) = self.live_planned_actions {
                parts.push(format!("{planned} planned"));
            }
            if let Some(applied) = self.live_applied_actions {
                parts.push(format!("{applied} applied"));
            }
            if parts.is_empty() {
                return "subprocess active".to_string();
            }
            return parts.join("  |  ");
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

    pub fn activity_message(&self) -> String {
        if self.running_job.is_some()
            && matches!(
                self.current_phase.as_deref(),
                Some("hashing_sample") | Some("hashing_full") | Some("applying")
            )
        {
            return String::new();
        }
        self.latest_activity_message
            .clone()
            .unwrap_or_else(|| self.last_log_line())
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current_path.as_deref()
    }

    pub fn current_path_label(&self) -> &'static str {
        "current"
    }

    pub fn active_worker_paths(&self) -> Vec<(usize, &str)> {
        self.active_workers
            .iter()
            .map(|(worker, path)| (*worker, path.as_str()))
            .collect()
    }

    pub fn elapsed_label(&self) -> String {
        let duration = if let Some(started) = self.job_started_at {
            started.elapsed()
        } else if let Some(duration) = self.last_run_duration {
            duration
        } else {
            Duration::ZERO
        };
        format_duration(duration)
    }

    pub fn expand_paths(&self) -> bool {
        self.expand_paths
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
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('p') => self.toggle_path_mode(),
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
        self.reset_live_run_state();
        self.report = None;
        self.selected_row = 0;
        self.push_log("Running scan...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = actions::run_scan(strict, tx);
        });
        self.job_started_at = Some(Instant::now());
        self.current_phase = Some("collecting".to_string());
        self.latest_activity_message = Some("Collecting candidate files".to_string());
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
        self.reset_live_run_state();
        self.report = None;
        self.selected_row = 0;
        self.push_log("Running link...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = actions::run_link(strict, tx);
        });
        self.job_started_at = Some(Instant::now());
        self.current_phase = Some("planning".to_string());
        self.latest_activity_message = Some("Preparing link run".to_string());
        self.running_job = Some(RunningJob {
            kind: JobKind::Link,
            receiver: rx,
        });
    }

    fn toggle_path_mode(&mut self) {
        self.expand_paths = !self.expand_paths;
        self.push_log(format!(
            "Path display {}.",
            if self.expand_paths {
                "expanded"
            } else {
                "compact"
            }
        ));
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
        let mut disconnected_kind = None;
        let mut events = Vec::new();
        if let Some(job) = self.running_job.as_mut() {
            loop {
                match job.receiver.try_recv() {
                    Ok(event) => events.push((job.kind, event)),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected_kind = Some(job.kind);
                        break;
                    }
                }
            }
        }

        let mut saw_done = false;
        for (kind, result) in events {
            match result {
                Ok(event) => {
                    if self.apply_tui_event(kind, event) {
                        saw_done = true;
                    }
                }
                Err(err) => {
                    self.running_job = None;
                    self.finish_live_run();
                    let label = match kind {
                        JobKind::Scan => "Scan",
                        JobKind::Link => "Link",
                    };
                    self.push_log(format!("{label} failed: {err}"));
                    return;
                }
            }
        }

        if let Some(kind) = disconnected_kind
            && !saw_done
            && self.running_job.is_some()
        {
            self.running_job = None;
            self.finish_live_run();
            let label = match kind {
                JobKind::Scan => "Scan",
                JobKind::Link => "Link",
            };
            self.push_log(format!(
                "{label} failed: worker thread disconnected unexpectedly"
            ));
        }
    }

    fn apply_tui_event(&mut self, kind: JobKind, event: TuiEvent) -> bool {
        match event {
            TuiEvent::Phase { phase } => {
                self.current_phase = Some(phase.clone());
                self.current_path = None;
                if phase.starts_with("hashing_") {
                    self.active_workers.clear();
                } else {
                    self.active_workers.clear();
                    self.hash_progress_done = None;
                    self.hash_progress_total = None;
                }
                let message = format!("Phase: {}", display_phase(&phase));
                self.latest_activity_message = Some(message.clone());
                self.push_log(message);
                false
            }
            TuiEvent::WorkerActivity {
                phase,
                worker,
                path,
            } => {
                self.current_phase = Some(phase);
                self.current_path = None;
                match path {
                    Some(path) => {
                        self.active_workers.insert(worker, path);
                    }
                    None => {
                        self.active_workers.remove(&worker);
                    }
                }
                false
            }
            TuiEvent::File { phase, path } => {
                if let Some(phase) = phase {
                    self.current_phase = Some(phase);
                }
                self.current_path = Some(path.clone());
                self.latest_activity_message = Some(format!("Working on {path}"));
                false
            }
            TuiEvent::Count {
                files_seen,
                groups_found,
                planned_actions,
                applied_actions,
                hash_progress_done,
                hash_progress_total,
            } => {
                if let Some(files_seen) = files_seen {
                    self.live_files_seen = Some(files_seen);
                    if matches!(self.current_phase.as_deref(), Some("collecting"))
                        && (files_seen == 1 || files_seen % 25 == 0)
                    {
                        self.push_log(format!("Collected {files_seen} file(s)"));
                    }
                }
                if let Some(groups_found) = groups_found {
                    self.live_groups_found = Some(groups_found);
                }
                if let Some(planned_actions) = planned_actions {
                    self.live_planned_actions = Some(planned_actions);
                }
                if let Some(applied_actions) = applied_actions {
                    self.live_applied_actions = Some(applied_actions);
                    if matches!(self.current_phase.as_deref(), Some("applying")) {
                        let planned = self.live_planned_actions.unwrap_or(applied_actions);
                        self.push_log(format!("Applied {applied_actions} / {planned} action(s)"));
                    }
                }
                if let Some(done) = hash_progress_done {
                    self.hash_progress_done = Some(done);
                }
                if let Some(total) = hash_progress_total {
                    self.hash_progress_total = Some(total);
                }
                if matches!(
                    self.current_phase.as_deref(),
                    Some("hashing_sample") | Some("hashing_full")
                ) && let (Some(done), Some(total)) =
                    (self.hash_progress_done, self.hash_progress_total)
                {
                    self.latest_activity_message = Some(match self.current_phase.as_deref() {
                        Some("hashing_full") => {
                            format!("Verifying duplicate files: {done} / {total}")
                        }
                        _ => format!("Hashing files: {done} / {total}"),
                    });
                    if done == 1 || done == total || done % 10 == 0 {
                        self.push_log(self.latest_activity_message.clone().unwrap_or_default());
                    }
                }
                false
            }
            TuiEvent::Log { message } => {
                self.latest_activity_message = Some(message.clone());
                self.push_log(message);
                false
            }
            TuiEvent::Error { message } => {
                self.running_job = None;
                self.finish_live_run();
                let label = match kind {
                    JobKind::Scan => "Scan",
                    JobKind::Link => "Link",
                };
                self.push_log(format!("{label} failed: {message}"));
                true
            }
            TuiEvent::Done { report } => {
                let planned = report.planned_actions;
                let applied = report.applied_actions;
                self.selected_row = 0;
                self.live_files_seen = Some(report.files_considered);
                self.live_planned_actions = Some(report.planned_actions);
                self.live_applied_actions = Some(report.applied_actions);
                self.current_path = None;
                self.active_workers.clear();
                self.hash_progress_done = None;
                self.hash_progress_total = None;
                self.report = Some(report);
                self.running_job = None;
                self.finish_live_run();
                match kind {
                    JobKind::Scan => {
                        self.push_log(format!("Scan finished. Planned actions: {planned}"))
                    }
                    JobKind::Link => {
                        self.push_log(format!("Link finished. Applied actions: {applied}"))
                    }
                }
                true
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

    fn reset_live_run_state(&mut self) {
        self.current_phase = None;
        self.current_path = None;
        self.latest_activity_message = None;
        self.live_files_seen = None;
        self.live_groups_found = None;
        self.live_planned_actions = None;
        self.live_applied_actions = None;
        self.hash_progress_done = None;
        self.hash_progress_total = None;
        self.active_workers.clear();
        self.last_run_duration = None;
        self.job_started_at = None;
    }

    fn finish_live_run(&mut self) {
        if let Some(started) = self.job_started_at.take() {
            self.last_run_duration = Some(started.elapsed());
        }
        self.current_phase = None;
        self.current_path = None;
        self.active_workers.clear();
    }
}

#[cfg(test)]
impl App {
    pub(crate) fn set_hash_activity_for_test(&mut self, phase: &str, workers: &[(usize, &str)]) {
        self.current_phase = Some(phase.to_string());
        self.active_workers = workers
            .iter()
            .map(|(worker, path)| (*worker, (*path).to_string()))
            .collect();
    }

    pub(crate) fn set_hash_progress_for_test(&mut self, done: usize, total: usize) {
        self.hash_progress_done = Some(done);
        self.hash_progress_total = Some(total);
    }

    pub(crate) fn set_running_scan_for_test(&mut self) {
        let (_tx, rx) = mpsc::channel();
        self.running_job = Some(RunningJob {
            kind: JobKind::Scan,
            receiver: rx,
        });
    }

    pub(crate) fn set_report_for_test(&mut self, report: RunReport) {
        self.report = Some(report);
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn display_phase(phase: &str) -> &'static str {
    match phase {
        "collecting" => "COLLECTING FILES",
        "hashing_sample" => "HASHING FILES",
        "hashing_full" => "VERIFYING DUPLICATES",
        "planning" => "PLANNING ACTIONS",
        "applying" => "APPLYING LINKS",
        _ => "PROCESSING",
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::WarningSummary;

    use super::super::model::{RunReport, TuiEvent};
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
    fn savings_split_is_truthful_for_scan_and_link_reports() {
        let mut app = App::new(&Config::default(), false);
        app.report = Some(sample_report());
        assert_eq!(app.saved_bytes(), 0);
        assert_eq!(app.potential_saved_bytes(), 1024);

        app.report = Some(RunReport {
            mode: "link".to_string(),
            dry_run: false,
            strict: false,
            files_considered: 2,
            planned_actions: 2,
            applied_actions: 1,
            total_saved_bytes: 30,
            skipped_by_min_age: 0,
            warnings: WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![
                LinkReportEntry {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    size: 10,
                    status: "applied".to_string(),
                    method: Some("hardlink".to_string()),
                    fallback_reason: None,
                    error: None,
                },
                LinkReportEntry {
                    source: "c".to_string(),
                    target: "d".to_string(),
                    size: 20,
                    status: "failed".to_string(),
                    method: None,
                    fallback_reason: None,
                    error: Some("boom".to_string()),
                },
            ],
        });

        assert_eq!(app.saved_bytes(), 10);
        assert_eq!(app.potential_saved_bytes(), 20);
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
        assert!(
            app.logs
                .iter()
                .any(|line| line.starts_with("Save report failed:"))
        );

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
        assert!(
            app.logs
                .iter()
                .any(|line| line == "A run is already in progress.")
        );
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
        tx.send(Ok(TuiEvent::Done {
            report: sample_report(),
        }))
        .expect("send result");

        app.poll_running_job();
        assert!(app.running_job.is_none());
        assert_eq!(app.files_considered(), 2);
        assert!(app.logs.iter().any(|line| line.contains("Scan finished")));
    }

    #[test]
    fn poll_running_job_updates_live_progress_state() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob {
            kind: JobKind::Scan,
            receiver: rx,
        });
        app.job_started_at = Some(Instant::now());

        tx.send(Ok(TuiEvent::Phase {
            phase: "hashing_sample".to_string(),
        }))
        .expect("send phase");
        tx.send(Ok(TuiEvent::WorkerActivity {
            phase: "hashing_sample".to_string(),
            worker: 2,
            path: Some("/downloads/movie.mkv".to_string()),
        }))
        .expect("send worker");
        tx.send(Ok(TuiEvent::Count {
            files_seen: Some(42),
            groups_found: Some(3),
            planned_actions: Some(2),
            applied_actions: Some(0),
            hash_progress_done: Some(23),
            hash_progress_total: Some(42),
        }))
        .expect("send count");

        app.poll_running_job();

        assert_eq!(app.files_considered(), 42);
        assert_eq!(app.planned_actions(), 2);
        assert_eq!(app.activity_phase(), "HASHING FILES");
        assert_eq!(app.active_worker_paths(), vec![(2, "/downloads/movie.mkv")]);
        assert_eq!(app.throughput_label(), "23 / 42 files hashed");
    }

    #[test]
    fn starting_run_clears_stale_report_rows() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.report = Some(sample_report());
        app.selected_row = 1;

        app.start_scan_job();

        assert!(app.report.is_none());
        assert_eq!(app.selected_row, 0);
        assert!(app.running_job.is_some());
    }

    #[test]
    fn path_toggle_switches_to_expanded_mode() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('p'))
            .expect("key handling should not fail");

        assert!(app.expand_paths());
        assert!(app.logs.iter().any(|line| line == "Path display expanded."));
    }

    #[test]
    fn tab_no_longer_cycles_fake_pane_focus() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Tab)
            .expect("key handling should not fail");

        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.selected_row(), 0);
    }

    #[test]
    fn current_path_label_is_current() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.current_phase = Some("hashing_sample".to_string());

        assert_eq!(app.current_path_label(), "current");
    }

    #[test]
    fn phase_change_clears_stale_current_path() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.current_path = Some("/downloads/old.mkv".to_string());

        let cleared = app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::Phase {
                phase: "planning".to_string(),
            },
        );

        assert!(!cleared);
        assert_eq!(app.current_path(), None);
        assert_eq!(app.activity_phase(), "PLANNING ACTIONS");
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
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Quit blocked while a run is active.")
        );
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
        assert!(
            editor
                .error
                .as_ref()
                .expect("save error")
                .contains("Failed to save config")
        );

        fs::remove_dir_all(root).expect("cleanup temp dir");
    }
}
