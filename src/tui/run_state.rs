//! What the dashboard knows of the run it watches and of the report it
//! shows, and how each event of the run's stream changes it. Nothing here
//! starts a thread or a process or draws anything, so a test drives it with
//! events alone.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::protocol::{Counts, Mode, Outcome, Phase};

use super::model::{LinkReportEntry, RunReport, TuiEvent};

/// Which run the dashboard started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobKind {
    Scan,
    LinkAll,
    /// A single-row link, of the download at this path.
    LinkSelected(String),
}

/// The run being watched, live, and the last report.
#[derive(Default)]
pub(crate) struct RunState {
    pub(super) report: Option<RunReport>,
    /// The reviewed report, put aside while a link runs: restored if the run
    /// ends without a report of its own, so the rows are not lost.
    pub(super) report_before_link: Option<RunReport>,
    pub(super) live_entries: Vec<LinkReportEntry>,
    pub(super) current_phase: Option<Phase>,
    pub(super) current_path: Option<String>,
    pub(super) latest_activity_message: Option<String>,
    pub(super) live_files_seen: Option<usize>,
    pub(super) live_groups_found: Option<usize>,
    pub(super) live_planned_actions: Option<usize>,
    pub(super) live_applied_actions: Option<usize>,
    pub(super) hash_progress_done: Option<usize>,
    pub(super) hash_progress_total: Option<usize>,
    /// Bytes the full pass has read, of all it will read, and when it began:
    /// speed and time left come from them.
    pub(super) hash_bytes: Option<(u64, u64)>,
    pub(super) full_pass_started_at: Option<Instant>,
    pub(super) active_workers: BTreeMap<usize, String>,
    pub(super) job_started_at: Option<Instant>,
    pub(super) last_run_duration: Option<Duration>,
}

/// What an event changed beyond the run's own state, for the dashboard to
/// take: the log, whether the run ended, and the selection.
#[derive(Debug, Default)]
pub(crate) struct Step {
    /// Lines for the Run Log, in order.
    pub(crate) logs: Vec<String>,
    /// The run ended: it is done, failed or was cancelled.
    pub(crate) finished: bool,
    /// The row to select now, when the event moved the selection.
    pub(crate) select: Option<usize>,
}

impl RunState {
    /// A run of `kind` is starting.
    pub(crate) fn start(&mut self, kind: &JobKind) {
        match kind {
            JobKind::Scan => {
                self.reset_live_run_state();
                self.report = None;
                self.current_phase = Some(Phase::Collecting);
                self.latest_activity_message = Some("Collecting candidate files".to_string());
            }
            JobKind::LinkAll => {
                self.reset_live_run_state();
                self.report_before_link = self.report.take();
                self.current_phase = Some(Phase::Planning);
                self.latest_activity_message = Some("Preparing link run".to_string());
            }
            // The rows stay on screen while one of them is linked.
            JobKind::LinkSelected(source) => {
                self.live_entries.clear();
                self.live_planned_actions = None;
                self.live_applied_actions = None;
                self.current_phase = Some(Phase::Planning);
                self.latest_activity_message = Some("Preparing selected link".to_string());
                self.current_path = Some(source.clone());
                self.hash_progress_done = None;
                self.hash_progress_total = None;
                self.active_workers.clear();
                self.last_run_duration = None;
            }
        }
        self.job_started_at = Some(Instant::now());
    }

    /// The rows shown: the report's, or while a scan runs, those planned so
    /// far.
    pub(crate) fn rows(&self) -> &[LinkReportEntry] {
        match &self.report {
            Some(report) => report.entries.as_slice(),
            None => self.live_entries.as_slice(),
        }
    }

    /// Take one event of the run's stream. `selected_row` is the row the
    /// dashboard has selected, which a single-row link moves on from.
    pub(crate) fn apply(&mut self, kind: JobKind, event: TuiEvent, selected_row: usize) -> Step {
        let mut step = Step::default();
        match event {
            TuiEvent::Phase { phase } => {
                self.hash_bytes = None;
                self.full_pass_started_at = (phase == Phase::HashingFull).then(Instant::now);
                self.current_phase = Some(phase);
                self.current_path = None;
                self.active_workers.clear();
                if !phase.is_hashing() {
                    self.hash_progress_done = None;
                    self.hash_progress_total = None;
                }
                let message = format!("Phase: {}", display_phase(phase));
                self.latest_activity_message = Some(message.clone());
                step.logs.push(message);
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
            }
            TuiEvent::File { phase, path } => {
                if let Some(phase) = phase {
                    self.current_phase = Some(phase);
                }
                // The current-file line shows it; the activity line keeps
                // what the phase is doing, so it never names a file the run
                // has long moved past.
                self.current_path = Some(path);
            }
            TuiEvent::Count(Counts {
                files_seen,
                groups_found,
                planned_actions,
                applied_actions,
                hash_progress_done,
                hash_progress_total,
            }) => {
                // Progress goes to the activity line and the counters, never
                // the Run Log: a line per step would push out what a person
                // needs to read there, such as why a run failed.
                if let Some(files_seen) = files_seen {
                    self.live_files_seen = Some(files_seen);
                }
                if let Some(groups_found) = groups_found {
                    self.live_groups_found = Some(groups_found);
                }
                if let Some(planned_actions) = planned_actions {
                    self.live_planned_actions = Some(planned_actions);
                }
                if let Some(applied_actions) = applied_actions {
                    self.live_applied_actions = Some(applied_actions);
                }
                if let Some(done) = hash_progress_done {
                    self.hash_progress_done = Some(done);
                }
                if let Some(total) = hash_progress_total {
                    self.hash_progress_total = Some(total);
                }
                if self.current_phase.is_some_and(Phase::is_hashing)
                    && let (Some(done), Some(total)) =
                        (self.hash_progress_done, self.hash_progress_total)
                {
                    self.latest_activity_message = Some(match self.current_phase {
                        Some(Phase::HashingFull) => {
                            format!("Verifying duplicate files: {done} / {total}")
                        }
                        _ => format!("Hashing files: {done} / {total}"),
                    });
                }
            }
            TuiEvent::PlannedEntries { entries } => {
                if !entries.is_empty() {
                    self.live_entries.extend(entries);
                    // Groups finish in any order; keep the rows in the order
                    // the finished report lists them, so they do not jump.
                    // That order compares paths folder by folder, which a
                    // string compare does not: `Show.S01/` sorts after
                    // `Show/` as a path and before it as a string.
                    self.live_entries
                        .sort_by_cached_key(LinkReportEntry::row_order);
                    self.live_planned_actions = Some(self.live_entries.len());
                    self.latest_activity_message = Some(format!(
                        "Planning actions: {} queued",
                        self.live_entries.len()
                    ));
                }
            }
            TuiEvent::Log { message } => {
                self.latest_activity_message = Some(message.clone());
                step.logs.push(message);
            }
            TuiEvent::Cancelled => {
                step.finished = true;
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
                step.logs.push(line);
                self.restore_reviewed_rows(&mut step);
            }
            TuiEvent::Error { message, report } => {
                step.finished = true;
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
                step.logs.push(line);
                self.restore_reviewed_rows(&mut step);
            }
            TuiEvent::HashBytes { done, total } => {
                self.hash_bytes = Some((done, total));
            }
            // The runner checks it before passing anything on.
            TuiEvent::Hello { .. } => {}
            TuiEvent::Done { report } => {
                let report = *report;
                let planned = report.planned_actions;
                let applied = report.applied_actions;
                let link_failures = report.warnings.link_errors;
                let unreadable = report.warnings.total() - link_failures;
                let cancelled = report.outcome == Outcome::Cancelled;
                let symlinked = report
                    .entries
                    .iter()
                    .filter(|entry| entry.is_applied_symlink())
                    .count();
                self.current_path = None;
                self.active_workers.clear();
                self.hash_progress_done = None;
                self.hash_progress_total = None;
                self.merge_report(kind.clone(), report);
                step.select = Some(match &kind {
                    JobKind::LinkSelected(source) => self.next_planned_after(source, selected_row),
                    _ => 0,
                });
                self.report_before_link = None;
                self.live_entries.clear();
                step.finished = true;
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
                step.logs.push(line);
                if matches!(kind, JobKind::LinkAll | JobKind::LinkSelected(_)) && symlinked > 0 {
                    step.logs.push(format!(
                        "{symlinked} of them as symlinks: each breaks if its library file is upgraded, renamed or deleted. See fallback_reason."
                    ));
                }
                if unreadable > 0 {
                    step.logs.push(format!(
                        "{unreadable} file(s) or folder(s) could not be read; the log names each. Make them readable, or leave a folder out with `dejaview config set-exclude`."
                    ));
                }
                if matches!(kind, JobKind::LinkAll | JobKind::LinkSelected(_)) && link_failures > 0
                {
                    step.logs.push(format!(
                        "Link run had {link_failures} failure(s). Review failed rows for details."
                    ));
                }
            }
        }
        step
    }

    /// The run failed outside its event stream: the child could not start,
    /// its stream broke, or it ended without a final event.
    pub(crate) fn fail(&mut self, kind: JobKind, message: String) -> Step {
        let mut step = Step {
            finished: true,
            ..Step::default()
        };
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
        step.logs.push(line);
        self.restore_reviewed_rows(&mut step);
        step
    }

    /// A link run that ended without a report of its own (it failed, could not
    /// start, or was cancelled before changing anything) gives the reviewed
    /// rows back, so they can be linked again without a new scan.
    fn restore_reviewed_rows(&mut self, step: &mut Step) {
        if let Some(previous) = self.report_before_link.take()
            && self.report.is_none()
        {
            self.report = Some(previous);
            step.logs.push(
                "The rows shown are from before the link run; scan again to see what it changed."
                    .to_string(),
            );
        }
    }

    /// The row to select after a single-row link of `source`: the next
    /// planned row after it, else the first planned row, else where it was,
    /// so linking a list row by row needs no moving back.
    fn next_planned_after(&self, source: &str, selected_row: usize) -> usize {
        let rows = self.rows();
        let linked = rows
            .iter()
            .position(|row| row.source == source)
            .unwrap_or(selected_row);
        let planned = |row: &&LinkReportEntry| row.is_planned();
        rows.iter()
            .enumerate()
            .skip(linked + 1)
            .find(|(_, row)| planned(row))
            .or_else(|| rows.iter().enumerate().find(|(_, row)| planned(row)))
            .map_or(linked.min(rows.len().saturating_sub(1)), |(index, _)| index)
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
        self.hash_bytes = None;
        self.full_pass_started_at = None;
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
                    let freed_before = if existing.mode == Mode::Link {
                        existing.freed_bytes
                    } else {
                        0
                    };
                    for entry in &report.entries {
                        if let Some(current) = existing
                            .entries
                            .iter_mut()
                            .find(|current| current.same_pair(entry))
                        {
                            *current = entry.clone();
                        } else {
                            existing.entries.push(entry.clone());
                        }
                    }
                    existing.mode = Mode::Link;
                    existing.dry_run = false;
                    existing.strict = report.strict;
                    existing.files_considered =
                        report.files_considered.max(existing.files_considered);
                    existing.applied_actions = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.is_applied())
                        .count();
                    existing.planned_actions = existing
                        .entries
                        .iter()
                        .filter(|entry| entry.is_planned())
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
                        .filter(|entry| entry.is_failed())
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

fn display_phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Collecting => "COLLECTING FILES",
        Phase::HashingSample => "HASHING FILES",
        Phase::HashingFull => "VERIFYING DUPLICATES",
        Phase::Planning => "PLANNING ACTIONS",
        Phase::Applying => "APPLYING LINKS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{EntryStatus, LinkMethod, WarningSummary};

    fn row(source: &str, status: EntryStatus) -> LinkReportEntry {
        LinkReportEntry {
            source: source.to_string(),
            target: format!("/media{source}"),
            size: 10,
            status,
            method: Some(LinkMethod::TryHardlink),
            ..Default::default()
        }
    }

    fn report(mode: Mode, entries: Vec<LinkReportEntry>) -> Box<RunReport> {
        Box::new(RunReport {
            mode,
            planned_actions: entries.len(),
            entries,
            ..Default::default()
        })
    }

    #[test]
    fn a_scan_streams_into_rows_and_ends_with_its_report() {
        let mut run = RunState::default();
        run.start(&JobKind::Scan);
        assert_eq!(run.current_phase, Some(Phase::Collecting));
        let step = run.apply(
            JobKind::Scan,
            TuiEvent::PlannedEntries {
                entries: vec![row("/dl/b.mkv", EntryStatus::Planned)],
            },
            0,
        );
        assert!(!step.finished);
        run.apply(
            JobKind::Scan,
            TuiEvent::PlannedEntries {
                entries: vec![row("/dl/a.mkv", EntryStatus::Planned)],
            },
            0,
        );
        let sources: Vec<_> = run.rows().iter().map(|row| row.source.as_str()).collect();
        assert_eq!(sources, ["/dl/a.mkv", "/dl/b.mkv"], "rows keep path order");

        let done = report(
            Mode::Scan,
            vec![
                row("/dl/a.mkv", EntryStatus::Planned),
                row("/dl/b.mkv", EntryStatus::Planned),
            ],
        );
        let step = run.apply(JobKind::Scan, TuiEvent::Done { report: done }, 1);
        assert!(step.finished);
        assert_eq!(step.select, Some(0));
        assert_eq!(step.logs, ["Scan finished. Planned actions: 2"]);
        assert!(run.live_entries.is_empty());
        assert_eq!(run.rows().len(), 2);
        assert_eq!(run.current_phase, None);
        assert!(run.last_run_duration.is_some());
    }

    #[test]
    fn a_link_strict_stopped_shows_what_it_linked_and_says_why() {
        let mut run = RunState {
            report: Some(*report(
                Mode::Scan,
                vec![
                    row("/dl/a.mkv", EntryStatus::Planned),
                    row("/dl/b.mkv", EntryStatus::Planned),
                ],
            )),
            ..RunState::default()
        };
        run.start(&JobKind::LinkAll);
        assert!(run.report.is_none() && run.report_before_link.is_some());

        let mut partial = report(
            Mode::Link,
            vec![
                row("/dl/a.mkv", EntryStatus::Applied),
                row("/dl/b.mkv", EntryStatus::Failed),
            ],
        );
        partial.warnings = WarningSummary {
            link_errors: 1,
            ..Default::default()
        };
        let step = run.apply(
            JobKind::LinkAll,
            TuiEvent::Error {
                message: "--strict stopped the run".to_string(),
                report: Some(partial),
            },
            0,
        );
        assert!(step.finished);
        assert_eq!(step.logs, ["Link failed: --strict stopped the run"]);
        let statuses: Vec<_> = run.rows().iter().map(|row| row.status).collect();
        assert_eq!(statuses, [EntryStatus::Applied, EntryStatus::Failed]);
        assert!(run.report_before_link.is_none(), "the plan is not restored");
    }

    #[test]
    fn a_link_that_fails_before_a_report_gives_the_reviewed_rows_back() {
        let mut run = RunState {
            report: Some(*report(
                Mode::Scan,
                vec![row("/dl/a.mkv", EntryStatus::Planned)],
            )),
            ..RunState::default()
        };
        run.start(&JobKind::LinkAll);
        let step = run.fail(JobKind::LinkAll, "another run holds the lock".to_string());
        assert!(step.finished);
        assert_eq!(step.logs.len(), 2, "{:?}", step.logs);
        assert_eq!(step.logs[0], "Link failed: another run holds the lock");
        assert_eq!(run.rows().len(), 1);
        assert!(run.rows()[0].is_planned());
    }
}
