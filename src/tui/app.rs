use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use crate::config::{Config, load_config, save_config, validate_runtime_paths};
use crate::engine::LinkMode;
use crate::fsutil::normalize_runtime_dir;
use crate::protocol::PlannedPair;

use super::actions;
use super::events::{self, Event};
use super::model::{LinkReportEntry, RunReport, TuiEvent};

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    JsonPreview(String),
    ConfigEditor(ConfigEditor),
    ConfirmLink(ConfirmLinkMode),
    BlockingNotice(String),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConfirmLinkMode {
    All,
    Selected,
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum JobKind {
    Scan,
    LinkAll,
    LinkSelected(String),
}

struct RunningJob {
    kind: JobKind,
    receiver: Receiver<Result<TuiEvent>>,
    cancel: Sender<()>,
    cancel_requested: bool,
}

impl RunningJob {
    fn new(kind: JobKind, receiver: Receiver<Result<TuiEvent>>, cancel: Sender<()>) -> Self {
        Self {
            kind,
            receiver,
            cancel,
            cancel_requested: false,
        }
    }
}

pub struct App {
    pub strict: bool,
    selected_row: usize,
    pub overlay: Overlay,
    report: Option<RunReport>,
    /// The reviewed report, put aside while a link runs: restored if the run
    /// ends without a report of its own, so the rows are not lost.
    report_before_link: Option<RunReport>,
    live_entries: Vec<LinkReportEntry>,
    logs: VecDeque<String>,
    downloads: Option<PathBuf>,
    media: Option<PathBuf>,
    min_age: Option<u64>,
    min_size: u64,
    threads: usize,
    full_threads: usize,
    link_mode: LinkMode,
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
            report_before_link: None,
            live_entries: Vec::new(),
            logs,
            downloads: config.downloads.clone(),
            media: config.media.clone(),
            min_age: config.min_age,
            min_size: config.effective_min_size(),
            threads: config.effective_threads(),
            full_threads: config.effective_full_threads(),
            link_mode: config.link_mode.unwrap_or(LinkMode::Best),
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
            if job.cancel_requested {
                return "Cancelling";
            }
            return match job.kind {
                JobKind::Scan => "Scanning",
                JobKind::LinkAll | JobKind::LinkSelected(_) => "Linking",
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
            .or_else(|| {
                self.report.as_ref().map(|r| {
                    if r.mode == "link" {
                        r.entries
                            .iter()
                            .filter(|entry| entry.status.eq_ignore_ascii_case("planned"))
                            .count()
                    } else {
                        r.planned_actions
                    }
                })
            })
            .unwrap_or(0)
    }

    pub fn applied_actions(&self) -> usize {
        self.live_applied_actions
            .or_else(|| {
                self.report.as_ref().map(|r| {
                    if r.mode == "link" {
                        r.entries
                            .iter()
                            .filter(|entry| entry.status.eq_ignore_ascii_case("applied"))
                            .count()
                    } else {
                        r.applied_actions
                    }
                })
            })
            .unwrap_or(0)
    }

    /// What link runs freed: a download's bytes come back only once its
    /// last path is replaced, which the run counts as it links.
    pub fn saved_bytes(&self) -> u64 {
        self.report
            .as_ref()
            .filter(|report| report.mode == "link")
            .map_or(0, |report| report.freed_bytes)
    }

    pub fn potential_saved_bytes(&self) -> u64 {
        if self.report.is_none() && !self.live_entries.is_empty() {
            return self.live_entries.iter().map(|entry| entry.size).sum();
        }
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
        self.min_age.unwrap_or(0)
    }

    /// The planned rows of the last finished run, as shown: what `L` links.
    pub fn reviewed_planned_pairs(&self) -> Vec<PlannedPair> {
        self.report
            .iter()
            .flat_map(|report| &report.entries)
            .filter(|entry| entry.status.eq_ignore_ascii_case("planned"))
            .map(|entry| PlannedPair {
                source: entry.source.clone(),
                target: entry.target.clone(),
            })
            .collect()
    }

    pub fn min_size_bytes(&self) -> u64 {
        self.min_size
    }

    /// Files walked and quick-hashed at once.
    pub fn threads_label(&self) -> String {
        self.threads.to_string()
    }

    /// Files read in full at once.
    pub fn full_threads_label(&self) -> String {
        self.full_threads.to_string()
    }

    pub fn link_mode_label(&self) -> &'static str {
        match self.link_mode {
            LinkMode::Best => "best",
            LinkMode::SymlinkOnly => "symlink-only",
        }
    }

    pub fn rows(&self) -> &[LinkReportEntry] {
        if let Some(report) = &self.report {
            return report.entries.as_slice();
        }
        self.live_entries.as_slice()
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
                JobKind::LinkAll => "LINKING DUPLICATES",
                JobKind::LinkSelected(_) => "LINKING SELECTED DUPLICATE",
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

        match self.overlay.clone() {
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
            Overlay::ConfirmLink(mode) => match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.overlay = Overlay::None;
                    match mode {
                        ConfirmLinkMode::All => self.start_link_job(),
                        ConfirmLinkMode::Selected => {
                            if let Err(err) = self.start_selected_link_job() {
                                self.push_log(format!("Selected link failed: {err}"));
                            }
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None;
                    self.push_log("Link run cancelled.".to_string());
                }
                _ => {}
            },
            Overlay::BlockingNotice(_) => match code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.overlay = Overlay::None,
                KeyCode::Char('c') => {
                    self.overlay = Overlay::None;
                    self.request_cancel();
                }
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
            KeyCode::Char('c') => self.request_cancel(),
            KeyCode::Char('e') => self.open_config_editor(),
            KeyCode::Char('S') | KeyCode::Char('s') => self.start_scan_job(),
            KeyCode::Char('X') | KeyCode::Char('x') => {
                if let Err(err) = self.start_rescan_job() {
                    self.push_log(format!("Rescan failed: {err}"));
                }
            }
            KeyCode::Char('L') => {
                if self.reviewed_planned_pairs().is_empty() {
                    self.push_log(
                        "Run a scan first: L links the planned rows a finished scan shows, and nothing else."
                            .to_string(),
                    );
                } else {
                    self.overlay = Overlay::ConfirmLink(ConfirmLinkMode::All);
                }
            }
            KeyCode::Char('l') => {
                if self
                    .selected_entry()
                    .is_some_and(|entry| entry.status.eq_ignore_ascii_case("planned"))
                {
                    self.overlay = Overlay::ConfirmLink(ConfirmLinkMode::Selected);
                } else {
                    self.push_log(
                        "Select a planned row before running a single-item link.".to_string(),
                    );
                }
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
        self.live_entries.clear();
        self.selected_row = 0;
        self.push_log("Running scan...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = actions::run_scan(strict, tx, cancel_rx);
        });
        self.job_started_at = Some(Instant::now());
        self.current_phase = Some("collecting".to_string());
        self.latest_activity_message = Some("Collecting candidate files".to_string());
        self.running_job = Some(RunningJob::new(JobKind::Scan, rx, cancel_tx));
    }

    fn start_rescan_job(&mut self) -> Result<()> {
        self.start_rescan_job_with(actions::clear_cache)
    }

    fn start_rescan_job_with<F>(&mut self, clear_cache_fn: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return Ok(());
        }

        clear_cache_fn()?;
        self.push_log("Cache cleared. Running fresh scan...".to_string());
        self.start_scan_job();
        Ok(())
    }

    fn start_link_job(&mut self) {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return;
        }
        let pairs = self.reviewed_planned_pairs();
        let Some(downloads) = self.downloads.clone().filter(|_| !pairs.is_empty()) else {
            self.push_log("Run a scan first: nothing reviewed is planned.".to_string());
            return;
        };
        self.reset_live_run_state();
        self.report_before_link = self.report.take();
        self.live_entries.clear();
        self.selected_row = 0;
        self.push_log("Running link...".to_string());
        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = actions::run_link_pairs(strict, &downloads, &pairs, tx, cancel_rx);
        });
        self.job_started_at = Some(Instant::now());
        self.current_phase = Some("planning".to_string());
        self.latest_activity_message = Some("Preparing link run".to_string());
        self.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, cancel_tx));
    }

    fn start_selected_link_job(&mut self) -> Result<()> {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return Ok(());
        }

        let Some(pair) = self
            .selected_entry()
            .filter(|entry| entry.status.eq_ignore_ascii_case("planned"))
            .map(|entry| PlannedPair {
                source: entry.source.clone(),
                target: entry.target.clone(),
            })
        else {
            self.push_log("Select a planned row before running a single-item link.".to_string());
            return Ok(());
        };
        let Some(downloads) = self.downloads.clone() else {
            self.push_log("Set the downloads path before linking.".to_string());
            return Ok(());
        };
        let source = pair.source.clone();

        self.live_entries.clear();
        self.live_planned_actions = None;
        self.live_applied_actions = None;
        self.current_phase = Some("planning".to_string());
        self.latest_activity_message = Some("Preparing selected link".to_string());
        self.current_path = Some(source.clone());
        self.hash_progress_done = None;
        self.hash_progress_total = None;
        self.active_workers.clear();
        self.job_started_at = Some(Instant::now());
        self.last_run_duration = None;
        self.push_log(format!("Running selected link for {source}"));

        let strict = self.strict;
        let (tx, rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = actions::run_link_pairs(strict, &downloads, &[pair], tx, cancel_rx);
        });
        self.running_job = Some(RunningJob::new(
            JobKind::LinkSelected(source),
            rx,
            cancel_tx,
        ));
        Ok(())
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
        self.min_size = config.effective_min_size();
        self.link_mode = config.link_mode.unwrap_or(LinkMode::Best);
        self.overlay = Overlay::None;
        self.push_log("Saved TUI path configuration.".to_string());
        Ok(())
    }

    fn normalized_config_from_editor<L>(&self, load_fn: L, editor: &ConfigEditor) -> Result<Config>
    where
        L: FnOnce() -> Result<Config>,
    {
        let mut config = load_fn()?;
        config.downloads = Some(normalize_runtime_dir(std::path::Path::new(
            editor.downloads.trim(),
        ))?);
        config.media = Some(normalize_runtime_dir(std::path::Path::new(
            editor.media.trim(),
        ))?);
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

    /// Ask the running job to stop. A scan stops within one read chunk and keeps
    /// the hashes it finished in the cache; a link stops between files, so files
    /// already linked stay linked and the rest stay planned.
    fn request_cancel(&mut self) {
        let Some(job) = self.running_job.as_mut() else {
            self.push_log("No run to cancel.".to_string());
            return;
        };
        if job.cancel_requested {
            return;
        }
        job.cancel_requested = true;
        let _ = job.cancel.send(());
        let message = match job.kind {
            JobKind::Scan => "Cancelling scan; finished hashes stay cached.",
            JobKind::LinkAll | JobKind::LinkSelected(_) => {
                "Cancelling link; it stops before the next file."
            }
        };
        self.latest_activity_message = Some(message.to_string());
        self.push_log(message.to_string());
    }

    fn request_quit(&mut self) {
        if self.running_job.is_some() {
            self.overlay = Overlay::BlockingNotice(
                "A run is still active, so quitting is disabled until it stops. Press c to cancel it, or Esc to keep it running.".to_string(),
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
                    Ok(event) => events.push((job.kind.clone(), event)),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected_kind = Some(job.kind.clone());
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
                    self.fail_running_job(kind, err.to_string());
                    return;
                }
            }
        }

        if let Some(kind) = disconnected_kind
            && !saw_done
            && self.running_job.is_some()
        {
            self.fail_running_job(kind, "worker thread disconnected unexpectedly".to_string());
        }
    }

    /// A link run that ended without a report of its own (it failed, could not
    /// start, or was cancelled before changing anything) gives the reviewed
    /// rows back, so they can be linked again without a new scan.
    fn restore_reviewed_rows(&mut self) {
        if let Some(previous) = self.report_before_link.take()
            && self.report.is_none()
        {
            self.report = Some(previous);
            self.push_log(
                "The rows shown are from before the link run; scan again to see what it changed."
                    .to_string(),
            );
        }
    }

    fn fail_running_job(&mut self, kind: JobKind, message: String) {
        self.running_job = None;
        self.live_entries.clear();
        self.live_planned_actions = None;
        self.live_applied_actions = None;
        self.finish_live_run();
        let label = match kind {
            JobKind::Scan => "Scan",
            JobKind::LinkAll | JobKind::LinkSelected(_) => "Link",
        };
        let line = format!("{label} failed: {message}");
        self.latest_activity_message = Some(line.clone());
        self.push_log(line);
        self.restore_reviewed_rows();
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
            TuiEvent::PlannedEntries { entries } => {
                if !entries.is_empty() {
                    self.live_entries.extend(entries);
                    // Groups finish in any order; keep the rows in the order
                    // the finished report lists them, so they do not jump.
                    self.live_entries.sort_by(|a, b| {
                        a.source
                            .cmp(&b.source)
                            .then_with(|| a.target.cmp(&b.target))
                    });
                    self.live_planned_actions = Some(self.live_entries.len());
                    self.latest_activity_message = Some(format!(
                        "Planning actions: {} queued",
                        self.live_entries.len()
                    ));
                }
                false
            }
            TuiEvent::Log { message } => {
                self.latest_activity_message = Some(message.clone());
                self.push_log(message);
                false
            }
            TuiEvent::Cancelled => {
                self.running_job = None;
                self.live_entries.clear();
                self.live_planned_actions = None;
                self.live_applied_actions = None;
                self.finish_live_run();
                let line = match kind {
                    JobKind::Scan => "Scan cancelled. Finished hashes are cached.".to_string(),
                    JobKind::LinkAll | JobKind::LinkSelected(_) => {
                        "Link cancelled before any file was changed.".to_string()
                    }
                };
                self.latest_activity_message = Some(line.clone());
                self.push_log(line);
                self.restore_reviewed_rows();
                true
            }
            TuiEvent::Error { message, report } => {
                self.running_job = None;
                self.finish_live_run();
                // A link --strict stopped partway reports which pairs it
                // linked before the failure: show that, not the plan.
                if let Some(report) = report {
                    self.active_workers.clear();
                    self.hash_progress_done = None;
                    self.hash_progress_total = None;
                    self.merge_report(kind.clone(), *report);
                    self.report_before_link = None;
                    self.live_entries.clear();
                }
                let label = match kind {
                    JobKind::Scan => "Scan",
                    JobKind::LinkAll | JobKind::LinkSelected(_) => "Link",
                };
                let line = format!("{label} failed: {message}");
                self.latest_activity_message = Some(line.clone());
                self.push_log(line);
                self.restore_reviewed_rows();
                true
            }
            // The runner checks it before passing anything on.
            TuiEvent::Hello { .. } => false,
            TuiEvent::Done { report } => {
                let report = *report;
                let planned = report.planned_actions;
                let applied = report.applied_actions;
                let link_failures = report.warnings.link_errors;
                let cancelled = report.outcome == "cancelled";
                let symlinked = report
                    .entries
                    .iter()
                    .filter(|entry| entry.is_applied_symlink())
                    .count();
                self.selected_row = 0;
                self.current_path = None;
                self.active_workers.clear();
                self.hash_progress_done = None;
                self.hash_progress_total = None;
                self.merge_report(kind.clone(), report);
                self.report_before_link = None;
                self.live_entries.clear();
                self.running_job = None;
                self.finish_live_run();
                let line = match kind {
                    JobKind::Scan => format!("Scan finished. Planned actions: {planned}"),
                    _ if cancelled => format!(
                        "Link cancelled after {applied} of {planned} action(s); the rest stay planned."
                    ),
                    JobKind::LinkAll => format!("Link finished. Applied actions: {applied}"),
                    JobKind::LinkSelected(_) => {
                        format!("Selected link finished. Applied actions: {applied}")
                    }
                };
                self.latest_activity_message = Some(line.clone());
                self.push_log(line);
                if matches!(kind, JobKind::LinkAll | JobKind::LinkSelected(_)) && symlinked > 0 {
                    self.push_log(format!(
                        "{symlinked} of them as symlinks: each breaks if its library file is upgraded, renamed or deleted. See fallback_reason."
                    ));
                }
                if matches!(kind, JobKind::LinkAll | JobKind::LinkSelected(_)) && link_failures > 0
                {
                    self.push_log(format!(
                        "Link run had {link_failures} failure(s). Review failed rows for details."
                    ));
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
        self.live_entries.clear();
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

    fn merge_report(&mut self, kind: JobKind, report: RunReport) {
        match kind {
            JobKind::LinkSelected(_) => {
                if let Some(existing) = &mut self.report {
                    // A scan's report freed nothing; a link's adds up.
                    let freed_before = if existing.mode == "link" {
                        existing.freed_bytes
                    } else {
                        0
                    };
                    for entry in &report.entries {
                        if let Some(current) = existing.entries.iter_mut().find(|current| {
                            current.source == entry.source && current.target == entry.target
                        }) {
                            *current = entry.clone();
                        } else {
                            existing.entries.push(entry.clone());
                        }
                    }
                    existing.mode = "link".to_string();
                    existing.dry_run = false;
                    existing.strict = report.strict;
                    existing.files_considered =
                        report.files_considered.max(existing.files_considered);
                    existing.applied_actions = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.status.eq_ignore_ascii_case("applied"))
                        .count();
                    existing.planned_actions = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.status.eq_ignore_ascii_case("planned"))
                        .count();
                    existing.freed_bytes = freed_before + report.freed_bytes;
                    existing.total_saved_bytes = existing.freed_bytes;
                    existing.symlinked = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.is_applied_symlink())
                        .count();
                    existing.warnings.link_errors = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.status.eq_ignore_ascii_case("failed"))
                        .count() as u64;
                    self.live_files_seen = Some(existing.files_considered);
                    self.live_planned_actions = Some(existing.planned_actions);
                    self.live_applied_actions = Some(existing.applied_actions);
                } else {
                    self.live_files_seen = Some(report.files_considered);
                    self.live_planned_actions = Some(report.planned_actions);
                    self.live_applied_actions = Some(report.applied_actions);
                    self.report = Some(report);
                }
            }
            JobKind::Scan | JobKind::LinkAll => {
                let mut report = report;
                // A link of reviewed rows the cache vouches for walks
                // nothing and considers only the rows' files; keep the count
                // the scan showed, as a single-row link does.
                // A link run sets the scan's report aside while it runs.
                if matches!(kind, JobKind::LinkAll)
                    && let Some(previous) =
                        self.report.as_ref().or(self.report_before_link.as_ref())
                {
                    report.files_considered =
                        report.files_considered.max(previous.files_considered);
                }
                self.live_files_seen = Some(report.files_considered);
                self.live_planned_actions = Some(report.planned_actions);
                self.live_applied_actions = Some(report.applied_actions);
                self.report = Some(report);
            }
        }
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

    pub(crate) fn set_running_scan_for_test(&mut self) -> Receiver<()> {
        let (_tx, rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        self.running_job = Some(RunningJob::new(JobKind::Scan, rx, cancel_tx));
        cancel_rx
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
    use anyhow::anyhow;

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
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
            warnings: WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![],
            ..Default::default()
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
            freed_bytes: 10,
            total_saved_bytes: 10,
            skipped_by_min_age: 0,
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
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
            ..Default::default()
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
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));

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
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));
        tx.send(Ok(TuiEvent::Done {
            report: Box::new(sample_report()),
        }))
        .expect("send result");

        app.poll_running_job();
        assert!(app.running_job.is_none());
        assert_eq!(app.files_considered(), 2);
        assert!(app.logs.iter().any(|line| line.contains("Scan finished")));
    }

    #[test]
    fn link_done_with_failures_logs_summary() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));
        let mut report = sample_report();
        report.mode = "link".to_string();
        report.warnings.link_errors = 2;
        tx.send(Ok(TuiEvent::Done {
            report: Box::new(report),
        }))
        .expect("send result");

        app.poll_running_job();

        assert!(
            app.logs
                .iter()
                .any(|line| line == "Link finished. Applied actions: 0")
        );
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Link run had 2 failure(s). Review failed rows for details.")
        );
    }

    #[test]
    fn poll_running_job_updates_live_progress_state() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));
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
    fn planned_entries_populate_live_rows_and_to_save() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));

        tx.send(Ok(TuiEvent::PlannedEntries {
            entries: vec![LinkReportEntry {
                source: "/downloads/a.mkv".to_string(),
                target: "/media/a.mkv".to_string(),
                size: 10,
                status: "planned".to_string(),
                method: Some("try-hardlink".to_string()),
                fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
                error: None,
            }],
        }))
        .expect("send planned entry");

        app.poll_running_job();

        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.planned_actions(), 1);
        assert_eq!(app.potential_saved_bytes(), 10);
        assert_eq!(
            app.selected_entry()
                .and_then(|entry| entry.method.as_deref()),
            Some("try-hardlink")
        );
    }

    #[test]
    fn live_rows_stay_in_download_path_order() {
        let mut app = App::new(&Config::default(), false);
        let entry = |source: &str| LinkReportEntry {
            source: source.to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: "planned".to_string(),
            method: None,
            fallback_reason: None,
            error: None,
        };
        for source in ["/dl/c.mkv", "/dl/a.mkv", "/dl/b.mkv"] {
            app.apply_tui_event(
                JobKind::Scan,
                TuiEvent::PlannedEntries {
                    entries: vec![entry(source)],
                },
            );
        }
        let sources: Vec<_> = app.rows().iter().map(|row| row.source.as_str()).collect();
        assert_eq!(sources, ["/dl/a.mkv", "/dl/b.mkv", "/dl/c.mkv"]);
    }

    #[test]
    fn a_link_stopped_by_strict_shows_the_pairs_it_linked() {
        let mut app = App::new(&Config::default(), false);
        let row = |source: &str, status: &str| LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status: status.to_string(),
            method: None,
            fallback_reason: None,
            error: None,
        };
        let mut scanned = sample_report();
        scanned.entries = vec![row("/dl/a.mkv", "planned"), row("/dl/b.mkv", "planned")];
        app.report_before_link = Some(scanned);
        let mut partial = sample_report();
        partial.mode = "link".to_string();
        partial.outcome = "failed".to_string();
        partial.entries = vec![row("/dl/a.mkv", "applied"), row("/dl/b.mkv", "failed")];

        assert!(app.apply_tui_event(
            JobKind::LinkAll,
            TuiEvent::Error {
                message: "--strict stopped the run at its first failed link".to_string(),
                report: Some(Box::new(partial)),
            },
        ));

        let statuses: Vec<_> = app.rows().iter().map(|row| row.status.as_str()).collect();
        assert_eq!(statuses, ["applied", "failed"]);
        assert!(app.report_before_link.is_none());
        assert!(
            app.activity_message()
                .starts_with("Link failed: --strict stopped")
        );
    }

    #[test]
    fn a_link_that_fails_to_run_keeps_the_reviewed_rows() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let mut report = sample_report();
        report.entries = vec![LinkReportEntry {
            source: "/downloads/a.mkv".to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: None,
            error: None,
        }];
        // What start_link_job leaves behind: the reviewed report put aside
        // while the child runs.
        app.report_before_link = Some(report);
        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));
        tx.send(Err(anyhow!(
            "link failed: Error: another DejaView run holds the lock"
        )))
        .expect("send error");

        app.poll_running_job();

        assert!(app.running_job.is_none());
        assert_eq!(app.reviewed_planned_pairs().len(), 1, "the rows come back");
        app.handle_main_key(KeyCode::Char('L'))
            .expect("key handling should not fail");
        assert!(matches!(
            app.overlay,
            Overlay::ConfirmLink(ConfirmLinkMode::All)
        ));
    }

    fn rendered_text(app: &App, cols: u16, rows: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(cols, rows);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::tui::view::render(frame, app))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_run_log_shows_its_newest_lines() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        for n in 1..=30 {
            app.push_log(format!("log line number {n}"));
        }

        let text = rendered_text(&app, 120, 36);

        assert!(text.contains("log line number 30"), "{text}");
        assert!(
            !text.contains("TUI started"),
            "the oldest line has scrolled off"
        );
    }

    #[test]
    fn a_failed_link_says_why_on_the_activity_line() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));
        app.latest_activity_message = Some("Preparing link run".to_string());
        tx.send(Err(anyhow!(
            "another DejaView run holds the lock /downloads/.dejaview_lock"
        )))
        .expect("send error");

        app.poll_running_job();

        assert_eq!(
            app.activity_message(),
            "Link failed: another DejaView run holds the lock /downloads/.dejaview_lock"
        );
    }

    #[test]
    fn link_job_error_clears_stale_live_rows() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));

        tx.send(Ok(TuiEvent::PlannedEntries {
            entries: vec![LinkReportEntry {
                source: "/downloads/a.mkv".to_string(),
                target: "/media/a.mkv".to_string(),
                size: 10,
                status: "planned".to_string(),
                method: Some("try-hardlink".to_string()),
                fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
                error: None,
            }],
        }))
        .expect("send planned entry");
        tx.send(Err(anyhow!("parse streamed event from link")))
            .expect("send error");

        app.poll_running_job();

        assert!(app.running_job.is_none());
        assert!(app.rows().is_empty());
        assert_eq!(app.planned_actions(), 0);
        assert_eq!(app.applied_actions(), 0);
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Link failed: parse streamed event from link")
        );
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
    fn rescan_clears_cache_then_starts_scan() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.start_rescan_job_with(|| Ok(()))
            .expect("rescan should start");

        assert!(app.running_job.is_some());
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Cache cleared. Running fresh scan...")
        );
    }

    #[test]
    fn rescan_failure_does_not_start_job() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let err = app.start_rescan_job_with(|| Err(anyhow::anyhow!("boom")));
        assert!(err.is_err());
        assert!(app.running_job.is_none());
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
    fn c_sends_cancel_once_and_marks_the_job_cancelling() {
        let mut app = App::new(&Config::default(), false);
        let cancel_rx = app.set_running_scan_for_test();

        app.handle_main_key(KeyCode::Char('c')).expect("key");
        app.handle_main_key(KeyCode::Char('c')).expect("key");

        assert_eq!(cancel_rx.try_iter().count(), 1);
        assert_eq!(app.mode_label(), "Cancelling");
        assert!(
            app.logs
                .iter()
                .any(|line| line.starts_with("Cancelling scan"))
        );
    }

    #[test]
    fn c_without_a_run_only_logs() {
        let mut app = App::new(&Config::default(), false);
        app.handle_main_key(KeyCode::Char('c')).expect("key");
        assert!(app.logs.iter().any(|line| line == "No run to cancel."));
        assert!(app.running_job.is_none());
    }

    #[test]
    fn cancelled_event_ends_the_job_without_rows() {
        let mut app = App::new(&Config::default(), false);
        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));
        app.live_entries.push(sample_entry_for_cancel());
        tx.send(Ok(TuiEvent::Cancelled)).expect("send cancelled");

        app.poll_running_job();

        assert!(app.running_job.is_none());
        assert!(app.rows().is_empty());
        assert_eq!(app.mode_label(), "Idle");
        assert!(
            app.logs
                .iter()
                .any(|line| line.starts_with("Scan cancelled"))
        );
    }

    #[test]
    fn c_in_the_quit_notice_cancels_the_run() {
        let mut app = App::new(&Config::default(), false);
        let cancel_rx = app.set_running_scan_for_test();

        app.handle_key(KeyCode::Char('q'), KeyModifiers::NONE)
            .expect("key");
        assert!(matches!(app.overlay, Overlay::BlockingNotice(_)));
        app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE)
            .expect("key");

        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(cancel_rx.try_iter().count(), 1);
    }

    fn sample_entry_for_cancel() -> LinkReportEntry {
        LinkReportEntry {
            source: "/dl/a".to_string(),
            target: "/media/a".to_string(),
            size: 1,
            status: "planned".to_string(),
            method: None,
            fallback_reason: None,
            error: None,
        }
    }

    #[test]
    fn quit_is_blocked_while_job_is_running() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        let (_tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));

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
    fn lower_l_requires_a_planned_row() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('l'))
            .expect("key handling should not fail");

        assert!(app.running_job.is_none());
        assert!(app.logs.iter().any(|line| line.contains("planned row")));
    }

    #[test]
    fn uppercase_l_needs_a_finished_scan_with_planned_rows() {
        let config = Config::default();
        let mut app = App::new(&config, false);

        app.handle_main_key(KeyCode::Char('L'))
            .expect("key handling should not fail");

        assert!(matches!(app.overlay, Overlay::None), "nothing reviewed yet");
        assert!(app.running_job.is_none());
        assert!(app.logs.iter().any(|line| line.contains("scan")));

        // A finished scan with nothing planned is no better.
        app.report = Some(sample_report());
        app.handle_main_key(KeyCode::Char('L'))
            .expect("key handling should not fail");
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn link_all_keeps_the_file_count_the_scan_showed() {
        let mut app = App::new(&Config::default(), false);
        let mut scanned = sample_report();
        scanned.files_considered = 7;
        // As starting a link run leaves it: the scan's report set aside.
        app.report_before_link = Some(scanned);
        let mut linked = sample_report();
        linked.files_considered = 3;

        assert!(app.apply_tui_event(
            JobKind::LinkAll,
            TuiEvent::Done {
                report: Box::new(linked)
            }
        ));

        assert_eq!(app.live_files_seen, Some(7));
        assert_eq!(app.report.as_ref().map(|r| r.files_considered), Some(7));
        assert!(app.report_before_link.is_none());
    }

    #[test]
    fn link_all_takes_the_planned_rows_the_scan_showed() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let mut report = sample_report();
        let row = |source: &str, status: &str| LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status: status.to_string(),
            method: None,
            fallback_reason: None,
            error: None,
        };
        report.entries = vec![
            row("/downloads/a.mkv", "planned"),
            row("/downloads/b.mkv", "applied"),
            row("/downloads/c.mkv", "planned"),
        ];
        app.report = Some(report);

        let pairs = app.reviewed_planned_pairs();
        assert_eq!(
            pairs
                .iter()
                .map(|pair| (pair.source.as_str(), pair.target.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("/downloads/a.mkv", "/media/downloads/a.mkv"),
                ("/downloads/c.mkv", "/media/downloads/c.mkv"),
            ]
        );
    }

    #[test]
    fn uppercase_l_opens_confirmation_overlay() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let mut report = sample_report();
        report.entries = vec![LinkReportEntry {
            source: "/downloads/a.mkv".to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
        }];
        app.report = Some(report);

        app.handle_main_key(KeyCode::Char('L'))
            .expect("key handling should not fail");

        assert!(matches!(
            app.overlay,
            Overlay::ConfirmLink(ConfirmLinkMode::All)
        ));
        assert!(app.running_job.is_none());
    }

    #[test]
    fn lowercase_l_opens_selected_confirmation_overlay() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let mut report = sample_report();
        report.entries = vec![LinkReportEntry {
            source: "/downloads/a.mkv".to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: "planned".to_string(),
            method: Some("try-hardlink".to_string()),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
        }];
        report.planned_actions = 1;
        app.report = Some(report);

        app.handle_main_key(KeyCode::Char('l'))
            .expect("key handling should not fail");

        assert!(matches!(
            app.overlay,
            Overlay::ConfirmLink(ConfirmLinkMode::Selected)
        ));
        assert!(app.running_job.is_none());
    }

    #[test]
    fn selected_link_done_keeps_rows_and_updates_status() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.report = Some(RunReport {
            mode: "scan".to_string(),
            dry_run: true,
            strict: false,
            files_considered: 2,
            planned_actions: 2,
            applied_actions: 0,
            total_saved_bytes: 30,
            skipped_by_min_age: 0,
            skipped_by_min_size: 0,
            skipped_by_exclude: 0,
            skipped_as_library_file: 0,
            dangling_symlinks: Vec::new(),
            warnings: WarningSummary {
                walk_errors: 0,
                metadata_errors: 0,
                sample_hash_errors: 0,
                full_hash_errors: 0,
                link_errors: 0,
            },
            entries: vec![
                LinkReportEntry {
                    source: "/downloads/a.mkv".to_string(),
                    target: "/media/a.mkv".to_string(),
                    size: 10,
                    status: "planned".to_string(),
                    method: Some("try-hardlink".to_string()),
                    fallback_reason: Some(
                        "tries hardlink first; may fall back to symlink".to_string(),
                    ),
                    error: None,
                },
                LinkReportEntry {
                    source: "/downloads/b.mkv".to_string(),
                    target: "/media/b.mkv".to_string(),
                    size: 20,
                    status: "planned".to_string(),
                    method: Some("try-hardlink".to_string()),
                    fallback_reason: Some(
                        "tries hardlink first; may fall back to symlink".to_string(),
                    ),
                    error: None,
                },
            ],
            ..Default::default()
        });

        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(
            JobKind::LinkSelected("/downloads/a.mkv".to_string()),
            rx,
            mpsc::channel().0,
        ));
        tx.send(Ok(TuiEvent::Done {
            report: Box::new(RunReport {
                mode: "link".to_string(),
                dry_run: false,
                strict: false,
                files_considered: 2,
                planned_actions: 1,
                applied_actions: 1,
                total_saved_bytes: 10,
                skipped_by_min_age: 0,
                skipped_by_min_size: 0,
                skipped_by_exclude: 0,
                skipped_as_library_file: 0,
                dangling_symlinks: Vec::new(),
                warnings: WarningSummary {
                    walk_errors: 0,
                    metadata_errors: 0,
                    sample_hash_errors: 0,
                    full_hash_errors: 0,
                    link_errors: 0,
                },
                entries: vec![LinkReportEntry {
                    source: "/downloads/a.mkv".to_string(),
                    target: "/media/a.mkv".to_string(),
                    size: 10,
                    status: "applied".to_string(),
                    method: Some("hardlink".to_string()),
                    fallback_reason: None,
                    error: None,
                }],
                ..Default::default()
            }),
        }))
        .expect("send result");

        app.poll_running_job();

        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.applied_actions(), 1);
        assert_eq!(app.planned_actions(), 1);
        assert_eq!(app.rows()[0].status, "applied");
        assert_eq!(app.rows()[1].status, "planned");
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Selected link finished. Applied actions: 1")
        );
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
                    min_size: None,
                    threads: Some(8),
                    full_threads: None,
                    exclude: None,
                    link_mode: Some(LinkMode::Best),
                })
            },
            |config| {
                *saved_clone.lock().expect("lock saved config") = Some(Config {
                    downloads: config.downloads.clone(),
                    media: config.media.clone(),
                    min_age: config.min_age,
                    min_size: config.min_size,
                    threads: config.threads,
                    full_threads: config.full_threads,
                    exclude: config.exclude.clone(),
                    link_mode: config.link_mode,
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
