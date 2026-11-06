# A run answers a script with its exit code and one report

DejaView is run by cron, Docker and healthchecks as often as by a person, and those read only two things: the exit code and the report. Until now a run that failed on some files exited 0 like a clean one, and every link run overwrote the report, so a run that did nothing erased the record of the one before it. Settled on 2026-11-06 (`docs/FINDINGS.md`, C2, C3, U6).

## What was decided

Every command that runs the engine (`scan`, `link`, `cache cleanup-temp`) exits with one of four codes:

| Code | Meaning |
|---|---|
| 0 | the run finished and nothing failed, including a run with nothing to do |
| 1 | the run could not start or stopped on an error, or `--strict` turned a failure into an error |
| 2 | the run finished, but something failed that `--strict` would have stopped on |
| 3 | another run holds the lock on the downloads folder, so this one did nothing |

The report stays one file at one path, `<downloads>/.dejaview_link_report.json`, so a script that reads it keeps working:

- **A run that planned nothing leaves the report as it was.** A path given with `--report` is always written.
- **The report says when it was written**, in new `started_at` and `finished_at` fields beside the ones it already has.

## Consequences

- A cron line that alerts on any non-zero code now also alerts on a run where some files failed. That is the purpose, and it goes under `### After upgrading` in the changelog. The README's cron example names the codes and treats 3 as nothing to report.
- History across runs lives in the log, which records every file linked or failed, not in the reports.

## Considered options

- **A dated report per run in a `.dejaview_reports/` folder, pruned after some days.** Rejected: it fills a folder that the torrent client and Sonarr and Radarr scan, adds pruning code, and repeats what the log already keeps.
