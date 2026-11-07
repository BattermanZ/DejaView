# AGENTS.md

Project conventions for DejaView. Host-specific conventions live in
`AGENTS.local.md` (gitignored); what a reviewer judges lives in
`CODING_STANDARDS.md`; how tickets are read, filed and labelled lives in
`docs/agents/issue-tracker.md`.

DejaView replaces files in someone's downloads folder with links into their
media library. A mistake loses data or stops a torrent seeding, so where a
rule here has to choose, it chooses refusing over guessing.

## What DejaView never does

Each of these is a contract, with the test that holds it where one exists. A
change that breaks one is wrong whatever else it fixes.

- **The media library is only read.** A link replaces the download with a
  link to the library file; nothing under the media root is created, renamed
  or deleted. `ensure_roots_disjoint` in `src/config.rs` and
  `ensure_source_outside_media_root` in `src/engine/linking.rs` guard it, and
  `nested_media_root_is_rejected_and_library_is_untouched` in
  `tests/cli_integration.rs` holds it. Both guards compare paths, so the walk
  in `src/engine/collect.rs` also refuses a directory with another root's
  device and inode (a bind mount), and planning in `linking.rs` skips a
  download that is a library file seen through a second mount (a mergerfs
  branch). `library_bind_mounted_inside_downloads_is_refused` holds the first
  where the test may mount.
- **A download is replaced in one step.** The link is made under a
  `.dejaview_*.tmp.*` name beside the download and renamed over it, so a crash
  leaves the old file or the new link, and at worst a leftover temp file
  beside it. Scans skip `.dejaview_*` files.
- **`--tui-events` stdout is pure NDJSON**: one event per line and nothing
  else, because the TUI parses it (see How the program is shaped). Anything
  meant for a person is left out in that mode.
  `link_tui_events_stdout_stays_machine_parseable` holds it, and `src/engine`
  denies clippy's print lints, so every line the engine prints goes through
  `report::print_out`, which writes nothing to stdout in that mode.
- **The report JSON and `--json` output are a public format.** Cron scripts
  parse them. Renaming, removing or retyping a field breaks those scripts: it
  takes a ticket that asks for it and an `### After upgrading` line in the
  changelog. Adding a field does not. The golden files in `tests/golden/`
  hold the report and the event stream byte for byte; a change made on
  purpose rewrites them with `DEJAVIEW_BLESS=1`, and their diff is what a
  script will see.
- **The cache, `<downloads>/.dejaview_cache.json`, survives an upgrade.** A
  cache that no longer parses is replaced by an empty one, which means
  rehashing the whole library over a network drive. Change its format the way
  you would change the report's. `a_cache_entry_is_one_flat_object_on_disk`
  holds an entry's bytes.

## Who runs it, and where

Weigh every change against the setup DejaView is built for: a torrent client
seeding from the downloads folder, Sonarr or Radarr importing into the
library, both on a mergerfs pool on a network drive, run from the TUI, from
cron and in Docker. On that pool a run's cost is the reads and `stat` calls
it makes per file. Run `just perf` after changing `src/engine/hashing.rs`,
`cache.rs` or `collect.rs`, and put what it measured in the pull request.

## Dependencies

**The registry is the source of truth for every version number.** A version
written from memory is the one that was current at your training cutoff, not
today's. Add a dependency with `cargo add <crate>`, which writes the current
version; a version typed into `Cargo.toml` by hand is a bug.

**Read the crate's current documentation before writing against it**, even
when you believe you know the API: through the Context7 MCP server where the
session has it, otherwise on docs.rs at the version `Cargo.lock` holds. A 0.x
crate such as ratatui may rename things between minor releases.

**Take the current version when a dependency is first added, then hold it.**
Moving to a new major release is a ticket of its own.

**A new dependency is named in its ticket.** Put any other to the maintainer
before adding it: it is code that runs with write access to the downloads
folder, often as root.

## Branching

All work happens on a branch off `development`, never a commit straight onto
`development` or `main`, whatever the size of the change. Check before the
first edit:

```sh
git branch --show-current
git switch -c <prefix>/<short-slug> development
```

Use `feature/`, `fix/`, `docs/` or `refactor/` as the prefix; a session handed
a branch name by its harness uses that name instead. Push the branch and open
a pull request into `development`. `main` receives only merges from
`development`, at a release.

Every merge is a merge commit, `development` into `main` included: no squash,
no rebase-merge, and no force-push to `development` or `main`.

**Whoever built a pull request merges it**, once `just check` and `just test`
pass, the review's findings are settled and the live check has passed on the
code being merged. Merge with a merge commit (`gh pr merge <number> --merge`)
and confirm the merge commit is on `development`.

**A pull request closes its ticket by naming it**: `Closes #N` in the title,
the body or a commit message. GitHub acts on that only for merges into the
default branch, `main`, so `.github/workflows/close-issues-on-development.yml`
does it for merges into `development`. It reads one number after each keyword
(`Closes #1, closes #2` for two) and ignores the words around it: "does not
fix #7" closes #7. A ticket built in code stays open until its pull request
merges.

## Commits

Subject `type(scope): summary`, as the history has it:
`fix(engine): fill sample reads on filesystems that return short reads`. The
body is `- ` bullets saying what changed and why, with real line breaks: pass
a multi-line message with a heredoc or `-F <file>`.

## The changelog

`CHANGELOG.md` opens with `## Unreleased`, and a release is cut from it.

**A change that a person running DejaView would notice adds its line under
`## Unreleased`, in the same commit as the change.** That person uses the CLI
or the TUI, or runs a script that reads the report. A refactor, a test or a
change to these instructions adds no line.

- **One line per change**, for that reader: what DejaView does now, and what
  it did before where that is the news.
- **Under Added, Changed or Fixed.** A change someone must act on when
  upgrading (a new default, a renamed report field, a cache that will
  rebuild) goes under `### After upgrading`, which comes first.
- **A fix to something still under `## Unreleased` gets no line of its own.**
  Correct the line already there, and take out the line of a change that is
  reverted before a release.
- **End the line with its issue as `[#N]`**, and add `[#N]: <url>` to the
  definitions at the foot of the section, in number order. A change with no
  issue names its pull request the same way.
- **The changelog is public.** It names no host and no person.

## Building, testing and running

Everything runs through `just` at the repo root, and CI runs `just check` and
`just test`, so what passes here passes there.

```sh
just check          # cargo fmt --check, then clippy on every target with warnings as errors
just test           # every test; under root, the unit tests again as nobody
just format         # apply cargo fmt
just perf           # the perf harness, in a release build
just sandbox        # a fresh scratch tree under .dev/sandbox, Movies and TV as two library folders
just sandbox one    # the same tree with media as the one library folder
just try <args>     # run dejaview against it: just try scan, just try link --json
```

**A fix starts from a test that fails on the bug.** Run it red before the fix
and green after, and say in the pull request that it failed first.

**A test takes its folder from `TempDir`** in `tests/common/mod.rs`, which
removes it on drop, even after a failed assertion; unit tests reach the same
module as `crate::test_common`.

**`just test` reruns the unit tests as `nobody` under root**, because five
engine tests make a file unreadable and root reads it anyway: as root those
tests return early and still report `ok`. Run `just test` rather than `cargo
test` in a root shell, which a cloud container is.

**The live check is `just sandbox`, then `just try`.** Run the change the way
a person would (`just try scan`, `just try link`) and look at the disk. A
change to the walk, the cache, the report or the paths shown runs on both
layouts: `just sandbox` (two library folders) and `just sandbox one` (one).
`ls -li .dev/sandbox/downloads/*` shows a hardlink as an inode shared with the
library file. `just try` keeps the config and log under `.dev/home`, so the
real ones are never touched. A change to the TUI is looked at by a person in a
terminal; say so in the pull request when you could not.

## How the program is shaped

- `src/main.rs` is the CLI and wires every command. `config::check` in
  `src/config.rs` is the one check a configuration passes, for `config
  validate`, every run and the dashboard's path editor: a new requirement on
  the folders goes there.
- `src/engine/` does the work, in this order: `collect.rs` walks downloads
  and every library folder and groups files by size, `hashing.rs` hashes them (a quick hash of the
  first and last MiB, a full hash only when quick hashes collide), `cache.rs`
  keeps hashes between runs, `linking.rs` plans and applies, `report.rs`
  writes the report.
- `src/tui/` never runs a scan or a link itself. It starts `dejaview scan`
  or `dejaview link` with `--tui-events` as a child process, reads the event
  stream (`src/tui/actions.rs`), and stops it by writing `cancel` to the
  child's stdin. `src/protocol.rs` holds the stream's types, and
  `src/tui/run_state.rs` what each event changes, with no thread or process,
  so a test drives it with events alone. Two short calls
  into the engine run in the dashboard's process, both from `actions.rs`:
  clearing the cache, and the path editor's test hardlink, which runs on a
  thread of its own and only after the same second-mount check `config
  validate` makes.
- `docs/HOW_IT_WORKS.md` explains the flow for a reader.
  `docs/TUI_BLUEPRINT.md` and `docs/TUI_REDESIGN_PLAN.md` are historical: they
  say why the TUI looks as it does, and the code says what it does now.

## Tickets and decisions

Work is tracked as GitHub issues on `BattermanZ/DejaView`. `docs/FINDINGS.md`
is a review's backlog not yet filed as issues. Its four decisions are settled,
and its first round is worked straight from that file, as its "Rules for
working in this repo" say; after that, a finding is built once it is an issue
labelled `ready-for-agent`.

A decision that is hard to reverse (a default, a format, a refusal) is
recorded as an ADR under `docs/adr/` once made, and the words DejaView uses
for its own things go in `GLOSSARY.md`, which does not exist yet. Where a
ticket and an ADR disagree, the ADR wins.
