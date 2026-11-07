use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::config::{Config, load_config, save_config};
use crate::engine::LinkMode;
use crate::fsutil::{expand_home, normalize_runtime_dir};
use crate::protocol::{Mode, Phase, PlannedPair};

use super::actions;
use super::events::{self, Event};
use super::model::{LinkReportEntry, TuiEvent};
use super::run_state::{JobKind, RunState, Step};

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    JsonPreview(String),
    ConfigEditor(ConfigEditor),
    ConfirmLink(ConfirmLinkMode),
    /// Clear the cache and scan again: every file is read again.
    ConfirmRescan,
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

/// What a test hardlink from the library into downloads showed, as
/// `dejaview config validate` tries one: whether link runs will make
/// hardlinks or fall back to symlinks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkCheck {
    /// Not tried this session. It runs when the folders are saved, since it
    /// touches one library file's change time.
    Untested,
    Testing,
    Hardlinks,
    /// The test failed, so link runs will make symlinks; `cause` says why,
    /// in words a person can act on.
    Symlinks {
        cause: &'static str,
    },
    /// The library holds no file to try with yet.
    NoLibraryFile,
    /// The link mode is symlink-only, so there is nothing to test.
    SymlinkOnly,
    /// The folders reach each other through a second mount, so no test file
    /// was made; `reason` says how.
    Refused {
        reason: String,
    },
}

impl LinkCheck {
    /// What link runs will make, in one line, once a test has an answer.
    pub fn summary(&self) -> Option<String> {
        Some(match self {
            Self::Untested => return None,
            Self::Testing => "Testing a hardlink between the folders…".to_string(),
            Self::Hardlinks => {
                "A test hardlink worked: the folders share a filesystem, so links free space"
                    .to_string()
            }
            Self::Symlinks { cause } => format!(
                "A test hardlink failed ({}): links will be symlinks",
                cause.split(':').next().unwrap_or(cause)
            ),
            Self::NoLibraryFile => "No library file to test a hardlink with yet".to_string(),
            Self::SymlinkOnly => {
                "The link mode is symlink-only: links will be symlinks".to_string()
            }
            Self::Refused { reason } => format!("No test hardlink made: {reason}"),
        })
    }
}

/// What the dashboard asks a run to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobRequest {
    Scan {
        strict: bool,
    },
    /// Link exactly these reviewed rows.
    Link {
        strict: bool,
        downloads: PathBuf,
        pairs: Vec<PlannedPair>,
    },
}

/// Starts a run: it streams the run's events into the sender, and stops it
/// when the receiver gets a cancel.
pub(crate) type Spawner = Box<dyn FnMut(JobRequest, Sender<Result<TuiEvent>>, Receiver<()>)>;

/// Run `dejaview scan` or `dejaview link --plan` with `--tui-events` as a
/// child process, read on a thread of its own.
#[cfg_attr(test, allow(dead_code))]
fn spawn_child_run(request: JobRequest, events: Sender<Result<TuiEvent>>, cancel: Receiver<()>) {
    thread::spawn(move || {
        let _ = match request {
            JobRequest::Scan { strict } => actions::run_scan(strict, events, cancel),
            JobRequest::Link {
                strict,
                downloads,
                pairs,
            } => actions::run_link_pairs(strict, &downloads, &pairs, events, cancel),
        };
    });
}

/// The spawner a dashboard starts with: a child process per run. A unit
/// test's dashboard starts nothing instead and records the request, with
/// both ends of its channels, for the test to read and to send events
/// through; see [`test_runs`].
fn default_spawner() -> Spawner {
    #[cfg(not(test))]
    {
        Box::new(spawn_child_run)
    }
    #[cfg(test)]
    {
        Box::new(test_runs::record)
    }
}

/// The runs a unit test's dashboards asked for, on the test's own thread.
#[cfg(test)]
pub(crate) mod test_runs {
    use super::{JobRequest, Receiver, Result, Sender, TuiEvent};
    use std::cell::RefCell;

    /// A run a test's dashboard started: what it asked for, the sender a
    /// test feeds events through, and the receiver its cancel arrives on.
    pub(crate) struct Started {
        pub(crate) request: JobRequest,
        pub(crate) events: Sender<Result<TuiEvent>>,
        pub(crate) cancel: Receiver<()>,
    }

    thread_local! {
        static STARTED: RefCell<Vec<Started>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(
        request: JobRequest,
        events: Sender<Result<TuiEvent>>,
        cancel: Receiver<()>,
    ) {
        STARTED.with(|started| {
            started.borrow_mut().push(Started {
                request,
                events,
                cancel,
            })
        });
    }

    /// The runs started on this thread since the last call, oldest first.
    pub(crate) fn take() -> Vec<Started> {
        STARTED.with(|started| std::mem::take(&mut *started.borrow_mut()))
    }
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

/// What a click on a spot of the screen does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Click {
    /// Select this row.
    Row(usize),
    /// Press this key: a key in the footer or on a dialog's border.
    Key(KeyCode),
}

/// What the last frame drawn had room for, which paging and scrolling go
/// by, and where it drew what can be clicked. The view sets it as it draws.
#[derive(Default)]
pub struct ViewSizes {
    /// Result rows the table shows at once.
    pub result_rows: std::cell::Cell<usize>,
    /// Lines an open Help or JSON overlay shows at once, and has in all.
    pub overlay_rows: std::cell::Cell<usize>,
    pub overlay_lines: std::cell::Cell<usize>,
    /// The spots that can be clicked, in the order drawn. A dialog clears
    /// the screen's spots behind it, so only its own keys answer a click.
    clicks: std::cell::RefCell<Vec<(Rect, Click)>>,
    /// The open Help or JSON dialog, so a click outside it closes it.
    pub dialog: std::cell::Cell<Option<Rect>>,
}

impl ViewSizes {
    /// Forget every spot drawn so far, as a new frame or a dialog starts.
    pub fn clear_clicks(&self) {
        self.clicks.borrow_mut().clear();
        self.dialog.set(None);
    }

    pub fn add_click(&self, area: Rect, click: Click) {
        self.clicks.borrow_mut().push((area, click));
    }

    /// What a click at `column`, `row` does: the spot drawn last there wins.
    fn click_at(&self, column: u16, row: u16) -> Option<Click> {
        let at = Position::new(column, row);
        self.clicks
            .borrow()
            .iter()
            .rev()
            .find(|(area, _)| area.contains(at))
            .map(|(_, click)| *click)
    }
}

/// Lines a turn of the wheel scrolls Help or the JSON preview by.
const WHEEL_LINES: usize = 3;

pub struct App {
    pub strict: bool,
    selected_row: usize,
    /// How far an open Help or JSON overlay is scrolled, in lines.
    overlay_scroll: usize,
    view: ViewSizes,
    pub overlay: Overlay,
    logs: VecDeque<String>,
    downloads: Option<PathBuf>,
    media: Option<PathBuf>,
    min_age: Option<u64>,
    min_size: u64,
    threads: usize,
    full_threads: usize,
    link_mode: LinkMode,
    /// The names every walk leaves out, for the check before a test hardlink.
    exclude: Vec<String>,
    expand_paths: bool,
    /// The run being watched, and the report shown.
    run: RunState,
    running_job: Option<RunningJob>,
    /// Starts each run; see [`default_spawner`].
    spawner: Spawner,
    link_check: LinkCheck,
    link_check_rx: Option<Receiver<(LinkCheck, Vec<String>)>>,
    should_quit: bool,
}

impl App {
    pub fn new(config: &Config, strict: bool) -> Self {
        let mut logs = VecDeque::new();
        logs.push_back("TUI started. Press ? for help.".to_string());
        Self {
            strict,
            selected_row: 0,
            overlay_scroll: 0,
            view: ViewSizes::default(),
            overlay: Overlay::None,
            logs,
            downloads: config.downloads.clone(),
            media: config.media.clone(),
            min_age: config.min_age,
            min_size: config.effective_min_size(),
            threads: config.effective_threads(),
            full_threads: config.effective_full_threads(),
            link_mode: config.link_mode.unwrap_or(LinkMode::Best),
            exclude: config.effective_exclude(),
            expand_paths: false,
            run: RunState::default(),
            running_job: None,
            link_check: LinkCheck::Untested,
            link_check_rx: None,
            should_quit: false,
            spawner: default_spawner(),
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
            Event::Tick => {
                self.poll_running_job();
                self.poll_link_check();
            }
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    self.handle_key(key.code, key.modifiers)?;
                }
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.column, mouse.row)?,
        }
        Ok(())
    }

    /// A click presses the key or selects the row drawn under it; the wheel
    /// moves the selection, or scrolls Help and the JSON preview. A click
    /// outside Help or the JSON preview closes it; other dialogs wait for an
    /// answer.
    fn handle_mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) -> Result<()> {
        let scrolling = matches!(self.overlay, Overlay::Help | Overlay::JsonPreview(_));
        match kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = kind == MouseEventKind::ScrollDown;
                if scrolling {
                    let code = if down { KeyCode::Down } else { KeyCode::Up };
                    for _ in 0..WHEEL_LINES {
                        self.scroll_overlay(code);
                    }
                } else if matches!(self.overlay, Overlay::None) {
                    self.move_selection(if down { 1 } else { -1 });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => match self.view.click_at(column, row) {
                Some(Click::Key(code)) => self.handle_key(code, KeyModifiers::NONE)?,
                Some(Click::Row(index)) => {
                    if matches!(self.overlay, Overlay::None) && index < self.rows().len() {
                        self.selected_row = index;
                    }
                }
                None => {
                    let inside = self
                        .view
                        .dialog
                        .get()
                        .is_some_and(|area| area.contains(Position::new(column, row)));
                    if scrolling && !inside {
                        self.overlay = Overlay::None;
                    }
                }
            },
            _ => {}
        }
        Ok(())
    }

    pub fn files_considered(&self) -> usize {
        self.run
            .live_files_seen
            .or_else(|| self.run.report.as_ref().map(|r| r.files_considered))
            .unwrap_or(0)
    }

    pub fn planned_actions(&self) -> usize {
        self.run
            .live_planned_actions
            .or_else(|| {
                self.run.report.as_ref().map(|r| {
                    if r.mode == Mode::Link {
                        r.entries.iter().filter(|entry| entry.is_planned()).count()
                    } else {
                        r.planned_actions
                    }
                })
            })
            .unwrap_or(0)
    }

    /// What link runs freed: a download's bytes come back only once its
    /// last path is replaced, which the run counts as it links.
    pub fn saved_bytes(&self) -> u64 {
        self.run
            .report
            .as_ref()
            .filter(|report| report.mode == Mode::Link)
            .map_or(0, |report| report.freed_bytes)
    }

    pub fn potential_saved_bytes(&self) -> u64 {
        if self.run.report.is_none() && !self.run.live_entries.is_empty() {
            return self.run.live_entries.iter().map(|entry| entry.size).sum();
        }
        self.run
            .report
            .as_ref()
            .map(|report| {
                if report.mode == Mode::Link {
                    report
                        .entries
                        .iter()
                        .filter(|entry| !entry.is_applied())
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

    pub fn overlay_scroll(&self) -> usize {
        self.overlay_scroll
    }

    pub fn view_sizes(&self) -> &ViewSizes {
        &self.view
    }

    pub fn config_editor(&self) -> Option<&ConfigEditor> {
        match &self.overlay {
            Overlay::ConfigEditor(editor) => Some(editor),
            _ => None,
        }
    }

    /// The downloads folder, which the table shows each download below.
    pub fn downloads_root(&self) -> Option<&std::path::Path> {
        self.downloads.as_deref()
    }

    /// The media folder, which the table shows each library file below.
    pub fn media_root(&self) -> Option<&std::path::Path> {
        self.media.as_deref()
    }

    pub fn min_age_secs(&self) -> u64 {
        self.min_age.unwrap_or(0)
    }

    /// The planned rows of the last finished run, as shown: what `L` links.
    pub fn reviewed_planned_pairs(&self) -> Vec<PlannedPair> {
        self.run
            .report
            .iter()
            .flat_map(|report| &report.entries)
            .filter(|entry| entry.is_planned())
            .map(PlannedPair::of)
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
        self.run.rows()
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.logs.iter().cloned().collect()
    }

    /// How far the full pass has read, how fast and how long it has left:
    /// `12.3 GiB of 40.0 GiB  |  150.0 MiB/s  |  ETA 03:05`. Speed and time
    /// left wait for a second of reading.
    pub fn full_pass_progress(&self) -> Option<String> {
        let (done, total) = self.run.hash_bytes?;
        use crate::human::format_size;
        let mut label = format!("{} of {}", format_size(done), format_size(total));
        let elapsed = self.run.full_pass_started_at?.elapsed();
        if elapsed >= Duration::from_secs(1) && done > 0 {
            let rate = done as f64 / elapsed.as_secs_f64();
            let left = Duration::from_secs_f64(total.saturating_sub(done) as f64 / rate);
            label.push_str(&format!(
                "  |  {}/s  |  ETA {}",
                format_size(rate as u64),
                format_duration(left)
            ));
        }
        Some(label)
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
                self.run.current_phase,
                Some(Phase::HashingSample | Phase::HashingFull | Phase::Applying)
            )
        {
            return String::new();
        }
        self.run
            .latest_activity_message
            .clone()
            .unwrap_or_else(|| self.last_log_line())
    }

    pub fn current_path(&self) -> Option<&str> {
        self.run.current_path.as_deref()
    }

    pub fn active_worker_paths(&self) -> Vec<(usize, &str)> {
        self.run
            .active_workers
            .iter()
            .map(|(worker, path)| (*worker, path.as_str()))
            .collect()
    }

    pub fn elapsed_label(&self) -> String {
        let duration = if let Some(started) = self.run.job_started_at {
            started.elapsed()
        } else if let Some(duration) = self.run.last_run_duration {
            duration
        } else {
            Duration::ZERO
        };
        format_duration(duration)
    }

    pub fn expand_paths(&self) -> bool {
        self.expand_paths
    }

    /// Whether both folders are set.
    pub fn has_paths(&self) -> bool {
        self.downloads.is_some() && self.media.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.running_job.is_some()
    }

    /// A scan is running, as opposed to a link run.
    pub fn scan_running(&self) -> bool {
        self.running_job
            .as_ref()
            .is_some_and(|job| job.kind == JobKind::Scan)
    }

    /// The running job's phase, as the event stream last named it.
    pub fn phase(&self) -> Option<Phase> {
        self.run.current_phase
    }

    /// How far the running job is, as done of total: bytes in the full
    /// pass, files while hashing, links while applying.
    pub fn run_progress(&self) -> Option<(u64, u64)> {
        self.running_job.as_ref()?;
        match self.run.current_phase {
            Some(Phase::HashingFull) if self.run.hash_bytes.is_some() => self.run.hash_bytes,
            Some(Phase::HashingSample | Phase::HashingFull) => {
                let done = self.run.hash_progress_done?;
                let total = self.run.hash_progress_total?;
                Some((done as u64, total as u64))
            }
            Some(Phase::Applying) => {
                let applied = self.run.live_applied_actions?;
                let planned = self.run.live_planned_actions?;
                Some((applied as u64, planned as u64))
            }
            _ => None,
        }
    }

    /// Files hashed, or matches verified, of those the phase will read.
    pub fn hash_progress(&self) -> Option<(usize, usize)> {
        Some((self.run.hash_progress_done?, self.run.hash_progress_total?))
    }

    /// Whether a report is loaded, and from a link run.
    pub fn showing_link_report(&self) -> bool {
        self.run
            .report
            .as_ref()
            .is_some_and(|report| report.mode == Mode::Link)
    }

    pub fn has_report(&self) -> bool {
        self.run.report.is_some()
    }

    /// The running job was asked to stop and has not yet.
    pub fn cancelling(&self) -> bool {
        self.running_job
            .as_ref()
            .is_some_and(|job| job.cancel_requested)
    }

    pub fn link_check(&self) -> &LinkCheck {
        &self.link_check
    }

    /// How long the last finished run took, if one finished this session.
    pub fn last_run_label(&self) -> Option<String> {
        self.run.last_run_duration.map(format_duration)
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
                _ => self.scroll_overlay(code),
            },
            // j scrolls, as in the table; J, which opened it, closes it.
            Overlay::JsonPreview(_) => match code {
                KeyCode::Esc | KeyCode::Char('J') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None
                }
                _ => self.scroll_overlay(code),
            },
            Overlay::ConfigEditor(_) => self.handle_config_editor_key(code)?,
            Overlay::ConfirmLink(mode) => match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.overlay = Overlay::None;
                    match mode {
                        ConfirmLinkMode::All => self.start_link_job(),
                        ConfirmLinkMode::Selected => {
                            if let Err(err) = self.start_selected_link_job() {
                                self.push_log(format!("Selected link failed: {err:#}"));
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
            Overlay::ConfirmRescan => match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.overlay = Overlay::None;
                    if let Err(err) = self.start_rescan_job() {
                        self.push_log(format!("Rescan failed: {err:#}"));
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => {
                    self.overlay = Overlay::None;
                    self.push_log("Rescan cancelled; the cache is kept.".to_string());
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
            KeyCode::Char('?') => {
                self.overlay_scroll = 0;
                self.overlay = Overlay::Help;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(self.page_rows()),
            KeyCode::PageUp => self.move_selection(-self.page_rows()),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Char('p') => self.toggle_path_mode(),
            KeyCode::Char('c') => self.request_cancel(),
            KeyCode::Char('e') => self.open_config_editor(),
            KeyCode::Char('S') | KeyCode::Char('s') => self.start_scan_job(),
            // Clearing the cache costs a read of every file, hours on a
            // network drive, and x sits next to c: ask first.
            KeyCode::Char('X') | KeyCode::Char('x') => {
                if self.running_job.is_some() {
                    self.push_log("A run is already in progress.".to_string());
                } else if self.paths_are_set_or_ask() {
                    self.overlay = Overlay::ConfirmRescan;
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
                    .is_some_and(|entry| entry.is_planned())
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
                    self.push_log(format!("Save report failed: {err:#}"));
                }
            }
            KeyCode::Char('J') => {
                if let Err(err) = self.open_json_overlay() {
                    self.push_log(format!("Open JSON preview failed: {err:#}"));
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

    /// How far PgUp and PgDn move: the rows the table showed last.
    fn page_rows(&self) -> isize {
        isize::try_from(self.view.result_rows.get().max(1)).unwrap_or(1)
    }

    /// Scroll an open Help or JSON overlay, within what the view drew.
    fn scroll_overlay(&mut self, code: KeyCode) {
        let page = self.view.overlay_rows.get().max(1);
        let last = self
            .view
            .overlay_lines
            .get()
            .saturating_sub(self.view.overlay_rows.get());
        let scroll = self.overlay_scroll.min(last);
        self.overlay_scroll = match code {
            KeyCode::Down | KeyCode::Char('j') => scroll + 1,
            KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => scroll + page,
            KeyCode::PageUp => scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            _ => scroll,
        }
        .min(last);
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows().len();
        if rows == 0 {
            self.selected_row = 0;
            return;
        }

        let current = self.selected_row as isize;
        let max = rows.saturating_sub(1) as isize;
        let next = current.saturating_add(delta).clamp(0, max);
        self.selected_row = next as usize;
    }

    /// Open the path editor when a scan has nowhere to look, saying so in
    /// the editor, and return false; true when both paths are set.
    fn paths_are_set_or_ask(&mut self) -> bool {
        if self.downloads.is_some() && self.media.is_some() {
            return true;
        }
        self.open_config_editor();
        self.set_config_editor_error(
            "Set the downloads and media folders first: type each path, Tab to switch, Enter to save."
                .to_string(),
        );
        self.push_log("Set the downloads and media folders before scanning.".to_string());
        false
    }

    fn start_scan_job(&mut self) {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return;
        }
        if !self.paths_are_set_or_ask() {
            return;
        }
        self.run.start(&JobKind::Scan);
        self.selected_row = 0;
        self.push_log("Running scan...".to_string());
        self.start_job(
            JobKind::Scan,
            JobRequest::Scan {
                strict: self.strict,
            },
        );
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
        if !self.paths_are_set_or_ask() {
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
        self.run.start(&JobKind::LinkAll);
        self.selected_row = 0;
        self.push_log("Running link...".to_string());
        self.start_job(
            JobKind::LinkAll,
            JobRequest::Link {
                strict: self.strict,
                downloads,
                pairs,
            },
        );
    }

    fn start_selected_link_job(&mut self) -> Result<()> {
        if self.running_job.is_some() {
            self.push_log("A run is already in progress.".to_string());
            return Ok(());
        }

        let Some(pair) = self
            .selected_entry()
            .filter(|entry| entry.is_planned())
            .map(PlannedPair::of)
        else {
            self.push_log("Select a planned row before running a single-item link.".to_string());
            return Ok(());
        };
        let Some(downloads) = self.downloads.clone() else {
            self.push_log("Set the downloads path before linking.".to_string());
            return Ok(());
        };
        let source = pair.source.clone();
        let kind = JobKind::LinkSelected(source.clone());
        self.run.start(&kind);
        self.push_log(format!("Running selected link for {source}"));

        self.start_job(
            kind,
            JobRequest::Link {
                strict: self.strict,
                downloads,
                pairs: vec![pair],
            },
        );
        Ok(())
    }

    /// Start the run and watch it.
    fn start_job(&mut self, kind: JobKind, request: JobRequest) {
        let (events, receiver) = mpsc::channel();
        let (cancel, cancel_rx) = mpsc::channel();
        (self.spawner)(request, events, cancel_rx);
        self.running_job = Some(RunningJob::new(kind, receiver, cancel));
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
        self.save_config_from_editor_with(load_config, save_config)?;
        if matches!(self.overlay, Overlay::None) {
            self.start_link_check();
        }
        Ok(())
    }

    /// Try one hardlink between the saved folders, off the UI thread.
    /// Try one hardlink between the saved folders, off the UI thread, as
    /// `config validate` does: only once a walk has found that neither
    /// folder reaches the other through a second mount, so the test file is
    /// never made inside the library.
    fn start_link_check(&mut self) {
        let (Some(downloads), Some(media)) = (self.downloads.clone(), self.media.clone()) else {
            return;
        };
        if self.link_mode == LinkMode::SymlinkOnly {
            self.link_check = LinkCheck::SymlinkOnly;
            self.link_check_rx = None;
            return;
        }
        self.link_check = LinkCheck::Testing;
        self.link_check_rx = Some(actions::spawn_link_check(
            downloads,
            media,
            self.exclude.clone(),
        ));
    }

    fn poll_link_check(&mut self) {
        let Some(receiver) = &self.link_check_rx else {
            return;
        };
        let (check, lines) = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => (
                LinkCheck::Untested,
                vec!["The test hardlink stopped before it could say how it went.".to_string()],
            ),
        };
        self.link_check_rx = None;
        for line in lines {
            self.push_log(line);
        }
        self.link_check = check;
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
                self.set_config_editor_error(format!("{err:#}"));
                return Ok(());
            }
        };

        if let Err(err) = save_fn(&config) {
            self.set_config_editor_error(format!("Failed to save config: {err:#}"));
            return Ok(());
        }

        self.downloads = config.downloads.clone();
        self.media = config.media.clone();
        self.min_size = config.effective_min_size();
        self.link_mode = config.link_mode.unwrap_or(LinkMode::Best);
        self.exclude = config.effective_exclude();
        self.overlay = Overlay::None;
        self.push_log("Saved TUI path configuration.".to_string());
        Ok(())
    }

    fn normalized_config_from_editor<L>(&self, load_fn: L, editor: &ConfigEditor) -> Result<Config>
    where
        L: FnOnce() -> Result<Config>,
    {
        let mut config = load_fn()?;
        let home = dirs::home_dir();
        // Each error says which field it is about.
        let folder = |field: &str, typed: &str| -> Result<PathBuf> {
            let typed = typed.trim();
            if typed.is_empty() {
                return Err(anyhow::anyhow!("{field}: type the folder's path"));
            }
            normalize_runtime_dir(&expand_home(typed, home.as_deref()))
                .map_err(|err| anyhow::anyhow!("{field}: {err:#}"))
        };
        config.downloads = Some(folder("Downloads", &editor.downloads)?);
        config.media = Some(folder("Media", &editor.media)?);
        crate::config::check(&config)?;
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
        let message = if job.kind.is_link() {
            "Cancelling link; it stops before the next file."
        } else {
            "Cancelling scan; finished hashes stay cached."
        };
        self.run.latest_activity_message = Some(message.to_string());
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
                    self.fail_running_job(kind, format!("{err:#}"));
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

    /// Take what an event, or a failure, changed beyond the run's state.
    /// Returns whether the run ended.
    fn take_step(&mut self, step: Step) -> bool {
        for line in step.logs {
            self.push_log(line);
        }
        if let Some(row) = step.select {
            self.selected_row = row;
        }
        if step.finished {
            self.running_job = None;
        }
        step.finished
    }

    fn fail_running_job(&mut self, kind: JobKind, message: String) {
        let step = self.run.fail(kind, message);
        self.take_step(step);
    }

    fn apply_tui_event(&mut self, kind: JobKind, event: TuiEvent) -> bool {
        let step = self.run.apply(kind, event, self.selected_row);
        self.take_step(step)
    }

    fn save_report(&mut self) -> Result<()> {
        let Some(report) = &self.run.report else {
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
        let Some(report) = &self.run.report else {
            self.push_log("No report available to preview.".to_string());
            return Ok(());
        };

        let json = serde_json::to_string_pretty(report)?;
        self.overlay_scroll = 0;
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

#[cfg(test)]
impl App {
    pub(crate) fn set_hash_activity_for_test(&mut self, phase: Phase, workers: &[(usize, &str)]) {
        self.run.current_phase = Some(phase);
        self.run.active_workers = workers
            .iter()
            .map(|(worker, path)| (*worker, (*path).to_string()))
            .collect();
    }

    pub(crate) fn set_hash_progress_for_test(&mut self, done: usize, total: usize) {
        self.run.hash_progress_done = Some(done);
        self.run.hash_progress_total = Some(total);
    }

    pub(crate) fn set_full_pass_for_test(&mut self, done: u64, total: u64, elapsed: Duration) {
        self.run.hash_bytes = Some((done, total));
        self.run.full_pass_started_at = std::time::Instant::now().checked_sub(elapsed);
    }

    pub(crate) fn set_running_scan_for_test(&mut self) -> Receiver<()> {
        let (_tx, rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        self.running_job = Some(RunningJob::new(JobKind::Scan, rx, cancel_tx));
        cancel_rx
    }

    /// A link run partway through applying: `applied` of `planned` linked.
    pub(crate) fn set_running_link_for_test(&mut self, applied: usize, planned: usize) {
        let (_tx, rx) = mpsc::channel();
        self.running_job = Some(RunningJob::new(JobKind::LinkAll, rx, mpsc::channel().0));
        self.run.current_phase = Some(Phase::Applying);
        self.run.live_applied_actions = Some(applied);
        self.run.live_planned_actions = Some(planned);
    }

    pub(crate) fn set_last_run_for_test(&mut self, duration: Duration) {
        self.run.last_run_duration = Some(duration);
    }

    pub(crate) fn set_report_for_test(&mut self, report: super::model::RunReport) {
        self.run.report = Some(report);
    }

    pub(crate) fn set_entry_error_for_test(&mut self, index: usize, error: &str) {
        if let Some(report) = self.run.report.as_mut() {
            report.entries[index].error = Some(error.to_string());
        }
    }

    pub(crate) fn toggle_path_mode_for_test(&mut self) {
        self.toggle_path_mode();
    }

    pub(crate) fn set_link_check_for_test(&mut self, check: LinkCheck) {
        self.link_check = check;
    }

    pub(crate) fn push_log_for_test(&mut self, line: &str) {
        self.push_log(line.to_string());
    }

    pub(crate) fn handle_key_for_test(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.handle_key(code, mods).expect("key handling");
    }

    pub(crate) fn mouse_for_test(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        self.handle_mouse(kind, column, row)
            .expect("mouse handling");
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use crate::protocol::{Counts, EntryStatus, LinkMethod, WarningSummary};
    use crate::test_common::TempDir;
    use anyhow::anyhow;
    use std::time::Instant;

    use super::super::model::{RunReport, TuiEvent};
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    fn sample_report() -> RunReport {
        RunReport {
            mode: Mode::Scan,
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
            warnings: WarningSummary::default(),
            entries: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn savings_split_is_truthful_for_scan_and_link_reports() {
        let mut app = App::new(&Config::default(), false);
        app.run.report = Some(sample_report());
        assert_eq!(app.saved_bytes(), 0);
        assert_eq!(app.potential_saved_bytes(), 1024);

        app.run.report = Some(RunReport {
            mode: Mode::Link,
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
            warnings: WarningSummary::default(),
            entries: vec![
                LinkReportEntry {
                    source: "a".to_string(),
                    target: "b".to_string(),
                    size: 10,
                    status: EntryStatus::Applied,
                    method: Some(LinkMethod::Hardlink),
                    fallback_reason: None,
                    error: None,
                    ..Default::default()
                },
                LinkReportEntry {
                    source: "c".to_string(),
                    target: "d".to_string(),
                    size: 20,
                    status: EntryStatus::Failed,
                    method: None,
                    fallback_reason: None,
                    error: Some("boom".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        assert_eq!(app.saved_bytes(), 10);
        assert_eq!(app.potential_saved_bytes(), 20);
    }

    #[test]
    fn save_report_error_does_not_abort_ui() {
        let dir = TempDir::new("report_parent_file");
        let temp_file = dir.join("not-a-folder");
        fs::write(&temp_file, b"not a directory").expect("create temp file");

        let config = Config {
            downloads: Some(temp_file.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config, false);
        app.run.report = Some(sample_report());

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
        report.mode = Mode::Link;
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
        app.run.job_started_at = Some(Instant::now());

        tx.send(Ok(TuiEvent::Phase {
            phase: Phase::HashingSample,
        }))
        .expect("send phase");
        tx.send(Ok(TuiEvent::WorkerActivity {
            phase: Phase::HashingSample,
            worker: 2,
            path: Some("/downloads/movie.mkv".to_string()),
        }))
        .expect("send worker");
        tx.send(Ok(TuiEvent::Count(Counts {
            files_seen: Some(42),
            groups_found: Some(3),
            planned_actions: Some(2),
            applied_actions: Some(0),
            hash_progress_done: Some(23),
            hash_progress_total: Some(42),
        })))
        .expect("send count");

        app.poll_running_job();

        assert_eq!(app.files_considered(), 42);
        assert_eq!(app.planned_actions(), 2);
        assert_eq!(app.phase(), Some(Phase::HashingSample));
        assert_eq!(app.active_worker_paths(), vec![(2, "/downloads/movie.mkv")]);
        assert_eq!(app.hash_progress(), Some((23, 42)));
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
                status: EntryStatus::Planned,
                method: Some(LinkMethod::TryHardlink),
                fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
                error: None,
                ..Default::default()
            }],
        }))
        .expect("send planned entry");

        app.poll_running_job();

        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.planned_actions(), 1);
        assert_eq!(app.potential_saved_bytes(), 10);
        assert_eq!(
            app.selected_entry().and_then(|entry| entry.method),
            Some(LinkMethod::TryHardlink)
        );
    }

    #[test]
    fn live_rows_stay_in_download_path_order() {
        let mut app = App::new(&Config::default(), false);
        let entry = |source: &str| LinkReportEntry {
            source: source.to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: EntryStatus::Planned,
            method: None,
            fallback_reason: None,
            error: None,
            ..Default::default()
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
    fn live_rows_keep_the_order_the_engine_gives_the_plan() {
        let mut app = App::new(&Config::default(), false);
        let entry = |source: &str| LinkReportEntry {
            source: source.to_string(),
            target: "/media/a.mkv".to_string(),
            size: 10,
            status: EntryStatus::Planned,
            ..Default::default()
        };
        // As paths, Show/ sorts before Show.S01/; as strings, after.
        for source in [
            "/dl/Show.S01/e01.mkv",
            "/dl/Show/e01.mkv",
            "/dl/a/b.mkv",
            "/dl/a.mkv",
        ] {
            app.apply_tui_event(
                JobKind::Scan,
                TuiEvent::PlannedEntries {
                    entries: vec![entry(source)],
                },
            );
        }
        let mut paths: Vec<_> = app
            .rows()
            .iter()
            .map(|row| PathBuf::from(&row.source))
            .collect();
        let shown = paths.clone();
        paths.sort();
        assert_eq!(shown, paths, "the engine's order, path by path");
        assert_eq!(shown[0], PathBuf::from("/dl/Show/e01.mkv"));
        assert_eq!(shown[2], PathBuf::from("/dl/a/b.mkv"));
    }

    #[test]
    fn a_single_link_selects_the_next_planned_row() {
        let mut app = App::new(&Config::default(), false);
        let row = |source: &str, status: EntryStatus| LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status,
            method: None,
            fallback_reason: None,
            error: None,
            ..Default::default()
        };
        let mut scanned = sample_report();
        scanned.entries = vec![
            row("/dl/a.mkv", EntryStatus::Planned),
            row("/dl/b.mkv", EntryStatus::Applied),
            row("/dl/c.mkv", EntryStatus::Planned),
            row("/dl/d.mkv", EntryStatus::Planned),
        ];
        app.run.report = Some(scanned);
        app.selected_row = 2;
        let mut linked = sample_report();
        linked.mode = Mode::Link;
        linked.entries = vec![row("/dl/c.mkv", EntryStatus::Applied)];

        app.apply_tui_event(
            JobKind::LinkSelected("/dl/c.mkv".to_string()),
            TuiEvent::Done {
                report: Box::new(linked.clone()),
            },
        );
        assert_eq!(app.selected_row(), 3, "the next planned row, d");

        // From the last row, back to the first still planned.
        linked.entries = vec![row("/dl/d.mkv", EntryStatus::Applied)];
        app.apply_tui_event(
            JobKind::LinkSelected("/dl/d.mkv".to_string()),
            TuiEvent::Done {
                report: Box::new(linked),
            },
        );
        assert_eq!(app.selected_row(), 0, "a");
    }

    #[test]
    fn a_link_stopped_by_strict_shows_the_pairs_it_linked() {
        let mut app = App::new(&Config::default(), false);
        let row = |source: &str, status: EntryStatus| LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status,
            method: None,
            fallback_reason: None,
            error: None,
            ..Default::default()
        };
        let mut scanned = sample_report();
        scanned.entries = vec![
            row("/dl/a.mkv", EntryStatus::Planned),
            row("/dl/b.mkv", EntryStatus::Planned),
        ];
        app.run.report_before_link = Some(scanned);
        let mut partial = sample_report();
        partial.mode = Mode::Link;
        partial.outcome = crate::protocol::Outcome::Failed;
        partial.entries = vec![
            row("/dl/a.mkv", EntryStatus::Applied),
            row("/dl/b.mkv", EntryStatus::Failed),
        ];

        assert!(app.apply_tui_event(
            JobKind::LinkAll,
            TuiEvent::Error {
                message: "--strict stopped the run at its first failed link".to_string(),
                report: Some(Box::new(partial)),
            },
        ));

        let statuses: Vec<_> = app.rows().iter().map(|row| row.status).collect();
        assert_eq!(statuses, [EntryStatus::Applied, EntryStatus::Failed]);
        assert!(app.run.report_before_link.is_none());
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
            status: EntryStatus::Planned,
            method: Some(LinkMethod::TryHardlink),
            fallback_reason: None,
            error: None,
            ..Default::default()
        }];
        // What start_link_job leaves behind: the reviewed report put aside
        // while the child runs.
        app.run.report_before_link = Some(report);
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
        app.run.latest_activity_message = Some("Preparing link run".to_string());
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
                status: EntryStatus::Planned,
                method: Some(LinkMethod::TryHardlink),
                fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
                error: None,
                ..Default::default()
            }],
        }))
        .expect("send planned entry");
        tx.send(Err(anyhow!("parse streamed event from link")))
            .expect("send error");

        app.poll_running_job();

        assert!(app.running_job.is_none());
        assert!(app.rows().is_empty());
        assert_eq!(app.planned_actions(), 0);
        assert_eq!(app.run.live_applied_actions, None);
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Link failed: parse streamed event from link")
        );
    }

    /// A config whose paths are set, so a scan may start.
    fn config_with_paths() -> Config {
        Config {
            downloads: Some(PathBuf::from("/nonexistent/downloads")),
            media: Some(PathBuf::from("/nonexistent/media")),
            ..Config::default()
        }
    }

    #[test]
    fn x_asks_before_clearing_the_cache() {
        let mut app = App::new(&config_with_paths(), false);

        app.handle_main_key(KeyCode::Char('x')).expect("key");
        assert!(matches!(app.overlay, Overlay::ConfirmRescan));
        assert!(app.running_job.is_none(), "nothing cleared or started yet");

        app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE)
            .expect("key");
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.running_job.is_none());
        assert!(
            app.logs
                .iter()
                .any(|line| line.contains("the cache is kept"))
        );

        app.handle_main_key(KeyCode::Char('X')).expect("key");
        assert!(matches!(app.overlay, Overlay::ConfirmRescan));
    }

    #[test]
    fn the_full_pass_shows_bytes_speed_and_time_left() {
        let mut app = App::new(&Config::default(), false);
        app.set_running_scan_for_test();
        app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::Phase {
                phase: Phase::HashingFull,
            },
        );
        app.set_hash_progress_for_test(0, 2);
        app.set_full_pass_for_test(500 << 20, 2000 << 20, Duration::from_secs(10));

        let label = app.full_pass_progress().expect("the full pass's progress");

        assert_eq!(app.hash_progress(), Some((0, 2)));
        assert!(label.contains("500.0 MiB of 2.0 GiB"), "{label}");
        assert!(label.contains("50.0 MiB/s"), "{label}");
        assert!(label.contains("ETA 00:30"), "{label}");
    }

    #[test]
    fn hash_bytes_events_feed_the_full_pass_progress() {
        let mut app = App::new(&Config::default(), false);
        app.set_running_scan_for_test();
        app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::Phase {
                phase: Phase::HashingFull,
            },
        );
        app.set_hash_progress_for_test(0, 1);
        app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::HashBytes {
                done: 1 << 30,
                total: 4 << 30,
            },
        );
        assert!(
            app.full_pass_progress()
                .is_some_and(|label| label.contains("1.0 GiB of 4.0 GiB"))
        );
    }

    #[test]
    fn scanning_without_paths_opens_the_path_editor() {
        let mut app = App::new(&Config::default(), false);

        app.handle_main_key(KeyCode::Char('S')).expect("key");

        assert!(app.running_job.is_none(), "nowhere to scan");
        let editor = app.config_editor().expect("the path editor opens");
        assert!(
            editor
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Set the downloads and media folders")),
            "{editor:?}"
        );
    }

    #[test]
    fn progress_stays_out_of_the_run_log() {
        let mut app = App::new(&Config::default(), false);
        let (tx, rx) = mpsc::channel();
        app.running_job = Some(RunningJob::new(JobKind::Scan, rx, mpsc::channel().0));
        let count = |files_seen, done, applied| {
            TuiEvent::Count(Counts {
                files_seen,
                applied_actions: applied,
                hash_progress_done: done,
                hash_progress_total: done.map(|_| 10),
                ..Counts::default()
            })
        };
        tx.send(Ok(TuiEvent::Phase {
            phase: Phase::Collecting,
        }))
        .expect("send");
        for n in [1, 25, 50] {
            tx.send(Ok(count(Some(n), None, None))).expect("send");
        }
        tx.send(Ok(TuiEvent::Phase {
            phase: Phase::HashingSample,
        }))
        .expect("send");
        for n in [1, 10] {
            tx.send(Ok(count(None, Some(n), None))).expect("send");
        }
        tx.send(Ok(TuiEvent::Phase {
            phase: Phase::Applying,
        }))
        .expect("send");
        tx.send(Ok(count(None, None, Some(1)))).expect("send");
        let before = app.logs.len();

        app.poll_running_job();

        let added: Vec<_> = app.logs.iter().skip(before).collect();
        assert!(
            added.iter().all(|line| line.starts_with("Phase:")),
            "only the phases: {added:?}"
        );
    }

    #[test]
    fn a_file_event_leaves_the_activity_line_to_the_phase() {
        let mut app = App::new(&Config::default(), false);
        app.set_running_scan_for_test();
        app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::Phase {
                phase: Phase::Collecting,
            },
        );
        app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::File {
                phase: Some(Phase::Collecting),
                path: "/downloads/a.mkv".to_string(),
            },
        );

        assert_eq!(app.activity_message(), "Phase: COLLECTING FILES");
        assert_eq!(app.current_path(), Some("/downloads/a.mkv"));
    }

    #[test]
    fn starting_run_clears_stale_report_rows() {
        let config = config_with_paths();
        let mut app = App::new(&config, false);
        app.run.report = Some(sample_report());
        app.selected_row = 1;

        app.start_scan_job();

        assert!(app.run.report.is_none());
        assert_eq!(app.selected_row, 0);
        assert!(app.running_job.is_some());
        let started = test_runs::take();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].request, JobRequest::Scan { strict: false });
    }

    #[test]
    fn rescan_clears_cache_then_starts_scan() {
        let config = config_with_paths();
        let mut app = App::new(&config, false);

        app.start_rescan_job_with(|| Ok(()))
            .expect("rescan should start");

        assert!(app.running_job.is_some());
        assert_eq!(test_runs::take().len(), 1, "one scan");
        assert!(
            app.logs
                .iter()
                .any(|line| line == "Cache cleared. Running fresh scan...")
        );
    }

    #[test]
    fn rescan_failure_does_not_start_job() {
        let config = config_with_paths();
        let mut app = App::new(&config, false);

        let err = app.start_rescan_job_with(|| Err(anyhow::anyhow!("boom")));
        assert!(err.is_err());
        assert!(app.running_job.is_none());
        assert!(test_runs::take().is_empty());
    }

    /// L and y start one link of the reviewed planned rows and nothing else;
    /// the run's events then come back through the channel it was given, and
    /// c reaches the run as a cancel.
    #[test]
    fn link_all_runs_the_reviewed_rows_and_takes_their_events() {
        let mut app = App::new(&config_with_paths(), true);
        let row = |source: &str, status: EntryStatus| LinkReportEntry {
            source: source.to_string(),
            target: format!("/nonexistent/media{source}"),
            size: 10,
            status,
            method: Some(LinkMethod::TryHardlink),
            ..Default::default()
        };
        app.run.report = Some(RunReport {
            entries: vec![
                row("/a.mkv", EntryStatus::Planned),
                row("/b.mkv", EntryStatus::Applied),
                row("/c.mkv", EntryStatus::Planned),
            ],
            ..sample_report()
        });

        app.handle_main_key(KeyCode::Char('L')).expect("key");
        app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE)
            .expect("key");
        let mut started = test_runs::take();
        assert_eq!(started.len(), 1);
        let JobRequest::Link {
            strict,
            downloads,
            pairs,
        } = &started[0].request
        else {
            panic!("a link run: {:?}", started[0].request);
        };
        assert!(*strict);
        assert_eq!(downloads, Path::new("/nonexistent/downloads"));
        let sources: Vec<_> = pairs.iter().map(|pair| pair.source.as_str()).collect();
        assert_eq!(sources, ["/a.mkv", "/c.mkv"], "only the planned rows");

        app.handle_main_key(KeyCode::Char('c')).expect("key");
        assert!(
            started[0].cancel.try_recv().is_ok(),
            "the cancel reaches the run"
        );

        let run = started.remove(0);
        let mut linked = RunReport {
            mode: Mode::Link,
            entries: vec![
                row("/a.mkv", EntryStatus::Applied),
                row("/c.mkv", EntryStatus::Applied),
            ],
            applied_actions: 2,
            ..sample_report()
        };
        linked.planned_actions = 2;
        run.events
            .send(Ok(TuiEvent::Done {
                report: Box::new(linked),
            }))
            .expect("send done");
        app.poll_running_job();
        assert!(app.running_job.is_none());
        assert!(app.rows().iter().all(LinkReportEntry::is_applied));
        assert_eq!(app.last_log_line(), "Link finished. Applied actions: 2");
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
    fn phase_change_clears_stale_current_path() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        app.run.current_path = Some("/downloads/old.mkv".to_string());

        let cleared = app.apply_tui_event(
            JobKind::Scan,
            TuiEvent::Phase {
                phase: Phase::Planning,
            },
        );

        assert!(!cleared);
        assert_eq!(app.current_path(), None);
        assert_eq!(app.phase(), Some(Phase::Planning));
    }

    #[test]
    fn c_sends_cancel_once_and_marks_the_job_cancelling() {
        let mut app = App::new(&Config::default(), false);
        let cancel_rx = app.set_running_scan_for_test();

        app.handle_main_key(KeyCode::Char('c')).expect("key");
        app.handle_main_key(KeyCode::Char('c')).expect("key");

        assert_eq!(cancel_rx.try_iter().count(), 1);
        assert!(app.cancelling());
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
        app.run.live_entries.push(sample_entry_for_cancel());
        tx.send(Ok(TuiEvent::Cancelled)).expect("send cancelled");

        app.poll_running_job();

        assert!(app.running_job.is_none());
        assert!(app.rows().is_empty());
        assert!(!app.is_running() && !app.has_report());
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
            status: EntryStatus::Planned,
            method: None,
            fallback_reason: None,
            error: None,
            ..Default::default()
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
        app.run.report = Some(sample_report());
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
        app.run.report_before_link = Some(scanned);
        let mut linked = sample_report();
        linked.files_considered = 3;

        assert!(app.apply_tui_event(
            JobKind::LinkAll,
            TuiEvent::Done {
                report: Box::new(linked)
            }
        ));

        assert_eq!(app.run.live_files_seen, Some(7));
        assert_eq!(app.run.report.as_ref().map(|r| r.files_considered), Some(7));
        assert!(app.run.report_before_link.is_none());
    }

    #[test]
    fn link_all_takes_the_planned_rows_the_scan_showed() {
        let config = Config::default();
        let mut app = App::new(&config, false);
        let mut report = sample_report();
        let row = |source: &str, status: EntryStatus| LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status,
            method: None,
            fallback_reason: None,
            error: None,
            ..Default::default()
        };
        report.entries = vec![
            row("/downloads/a.mkv", EntryStatus::Planned),
            row("/downloads/b.mkv", EntryStatus::Applied),
            row("/downloads/c.mkv", EntryStatus::Planned),
        ];
        app.run.report = Some(report);

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
            status: EntryStatus::Planned,
            method: Some(LinkMethod::TryHardlink),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
            ..Default::default()
        }];
        app.run.report = Some(report);

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
            status: EntryStatus::Planned,
            method: Some(LinkMethod::TryHardlink),
            fallback_reason: Some("tries hardlink first; may fall back to symlink".to_string()),
            error: None,
            ..Default::default()
        }];
        report.planned_actions = 1;
        app.run.report = Some(report);

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
        app.run.report = Some(RunReport {
            mode: Mode::Scan,
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
            warnings: WarningSummary::default(),
            entries: vec![
                LinkReportEntry {
                    source: "/downloads/a.mkv".to_string(),
                    target: "/media/a.mkv".to_string(),
                    size: 10,
                    status: EntryStatus::Planned,
                    method: Some(LinkMethod::TryHardlink),
                    fallback_reason: Some(
                        "tries hardlink first; may fall back to symlink".to_string(),
                    ),
                    error: None,
                    ..Default::default()
                },
                LinkReportEntry {
                    source: "/downloads/b.mkv".to_string(),
                    target: "/media/b.mkv".to_string(),
                    size: 20,
                    status: EntryStatus::Planned,
                    method: Some(LinkMethod::TryHardlink),
                    fallback_reason: Some(
                        "tries hardlink first; may fall back to symlink".to_string(),
                    ),
                    error: None,
                    ..Default::default()
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
                mode: Mode::Link,
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
                warnings: WarningSummary::default(),
                entries: vec![LinkReportEntry {
                    source: "/downloads/a.mkv".to_string(),
                    target: "/media/a.mkv".to_string(),
                    size: 10,
                    status: EntryStatus::Applied,
                    method: Some(LinkMethod::Hardlink),
                    fallback_reason: None,
                    error: None,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        }))
        .expect("send result");

        app.poll_running_job();

        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.rows().iter().filter(|row| row.is_applied()).count(), 1);
        assert_eq!(app.planned_actions(), 1);
        assert_eq!(app.rows()[0].status, EntryStatus::Applied);
        assert_eq!(app.rows()[1].status, EntryStatus::Planned);
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
        let error = editor.error.as_deref().expect("an error");
        assert!(
            error.starts_with("Downloads: cannot use /does/not/exist"),
            "names the field and the path: {error}"
        );
    }

    #[test]
    fn the_path_editor_reads_a_leading_tilde_as_home() {
        let Some(home) = dirs::home_dir().filter(|home| home.is_dir()) else {
            return;
        };
        let mut app = App::new(&Config::default(), false);
        app.overlay = Overlay::ConfigEditor(ConfigEditor {
            downloads: "~".to_string(),
            media: "/nonexistent/media".to_string(),
            active_field: ConfigField::Media,
            error: None,
        });

        app.save_config_from_editor_with(|| Ok(Config::default()), |_| Ok(()))
            .expect("save flow should not hard fail");

        // The home folder resolved; the media path is what fails.
        let error = app
            .config_editor()
            .and_then(|editor| editor.error.clone())
            .expect("an error");
        assert!(
            error.starts_with("Media:"),
            "{error} (home {})",
            home.display()
        );
    }

    #[test]
    fn config_editor_persists_valid_paths() {
        let root = TempDir::new("config_editor_save");
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
    }

    #[test]
    fn config_editor_save_failure_stays_in_overlay() {
        let root = TempDir::new("config_editor_save_failure");
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
    }
}
