# DejaView: findings and handover

Written for the next session that works on this repo. It holds every open finding from a four-part review (data safety, NAS performance, TUI/CLI usability, maintainability/ops), what was already fixed, and the conventions to follow.

## Where things stand

- Repo: `BattermanZ/DejaView`. Work branch: `claude/happy-fermat-r9d08c`, pushed. The last code commit is `a3959f9`; this file was added in the commit after it. No PR is open. `main` is still at `c82ccfe`.
- All line numbers below refer to `a3959f9`. Re-check them before editing.
- State at `a3959f9`: 134 unit tests and 14 CLI tests pass as root and as non-root; `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean; CI (`.github/workflows/ci.yml`) passed on GitHub.
- The reviewers' scratch files lived in the old container and are gone. Each finding below says how to reproduce it.
- This file was fact-checked against `a3959f9` by a separate reviewer: line references and behaviour were checked in the code, and S1, S2, C1, C3, C4, C5, C6, C8, C9, C10, P2, P8, U6, U8, M2, M8 and M9 were reproduced again with the binary. Not re-checked: the simulated NAS timings (P section), the TUI rendering claims (U1-U4; the code matches), and S3/S4 (reproduced only by the original reviewer).

## Rules for working in this repo

`AGENTS.md` holds them: the contracts DejaView keeps, the setup to weigh changes against, branching, commits, the changelog, and the `just` recipes for checks and the live run. For this list in particular:

- A finding is built from a GitHub issue. File it first (`docs/agents/issue-tracker.md`; one issue per finding, or per tight group such as S2 with P1), labelled `needs-triage`, and build it once the owner has moved it to `ready-for-agent`. The four decisions below come before the findings that hang on them.
- New report fields (C3), cache format changes (P2, P7) and the golden JSON (M1) touch the public formats `AGENTS.md` lists: the ticket says so, and the change carries a changelog line.
- A new dependency (decision 3) goes to the owner before it is added.

## Already done on this branch (do not redo)

Dependencies updated to latest (ratatui 0.30, crossterm 0.29, comfy-table 8, indicatif 0.18, colored 3, dirs 7, unused `hex` removed). Overlapping downloads/media roots rejected by path (canonicalised), and a link source inside media refused. Size groups split by physical file (dev, inode, mtime, ctime): hardlinked paths hashed once, all-one-file groups skipped, fully cached groups skip the quick hash, every hashed path gets a cache entry. Package and binary renamed `dejaview`; license field GPL-3.0-only. `.dejaview_*` files excluded from scans. Stale cache entries pruned (skipped after walk errors). TUI cancel with `c` (stdin `cancel` line to the child run; partial hashes saved; link stops between files). Quick-hash reads loop on short reads and EINTR. `link --only-source` hashes only the selected groups. Log rotates to `dejaview.log.1` past 5 MiB. Clippy clean. Flaky logger tests serialised. TUI mouse capture removed. Config parse errors name the file and the fix. Package description added (the `--help` about line). Collect, hash and apply take option structs (`CollectOptions`, `HashOptions`, `ApplyOptions`/`ApplyOutcome`). README and `docs/HOW_IT_WORKS.md` rewritten; TUI design docs marked historical. CI added. `AGENTS.md` rewritten as project conventions in place of the role protocol, with the contracts (M6's Invariants); `CODING_STANDARDS.md`, `docs/agents/issue-tracker.md`, `CHANGELOG.md` and a workflow closing issues on merges into `development` added; a `justfile` (`check`, `test`, `perf`, `sandbox`, `try`) that CI runs, whose `test` reruns the unit tests as `nobody` under root (M4's first point, M6's check script).

## Decisions for the owner

Ask before acting on these:

1. Decided: the symlink fallback stays the default (`docs/adr/0001-a-download-that-cannot-be-hardlinked-becomes-a-symlink.md`). S3's other fixes stand.
2. Decided: files under 1 MiB are skipped by default, empty files always (`docs/adr/0002-files-under-1-mib-are-left-alone.md`). This settles the size floor in S2 and P1.
3. Decided: add `ctrlc` with its `termination` feature, so SIGINT and SIGTERM cancel a CLI run and keep its hashes (P5). No `jwalk`: the parallel walk uses rayon, already a dependency (P3). No `libc`: set `rust-version = "1.89"` and take the lock with `std::fs::File::try_lock` (C2); the `fadvise` hint (P9) goes with it.
4. Change what the CLI returns: a distinct exit code for "finished with failures", and timestamped reports instead of one overwritten file.

## Findings

Severity: High, Medium, Low. Evidence tags: **[verified]** reproduced in the main session; **[reproduced]** reproduced by a reviewer with the binary; **[reasoned]** read from the code, not run.

### Data safety (fix first)

**S1. High. The same library seen through a second mount gets past the overlap guard and is rewritten.** **[verified]**
- Where: `src/config.rs:129-151` (`ensure_roots_disjoint`), `src/engine/linking.rs:51-74` (`ensure_source_outside_media_root`), `linking.rs:151-192`.
- The guard compares canonical paths, so it cannot see that two different paths are the same directory.
- Bind mounts (common in Docker): host `/data` mounted as downloads and `/data/media` as media. `config validate` passes. A library file that has an identical second copy in the library gets replaced by a relative symlink that dangles on the host. Reproduce: two identical files `media/A Movies/m.mkv` and `media/B Movies/m.mkv` under a host dir `data`, `mount --bind data c/downloads`, `mount --bind data/media c/media`, run `link`: `c/downloads/media/B Movies/m.mkv` becomes `-> ../../../media/A Movies/m.mkv`.
- mergerfs branch vs pool **[reproduced]**: downloads pointed at a branch path that contains the library, media at the pool view. Each library file appears as a "download" (branch device) and as "media" (FUSE device). The branch file was replaced by a symlink to its own pool path: the only copy was destroyed, and reading it hung the pool (FUSE self-loop).
- Fix (small): (a) while walking downloads, refuse if any directory has the media root's (dev, ino), and the reverse for the media walk; this catches bind mounts. (b) refuse a pair whose source and target have equal size, mtime_ns and ctime_ns but a different st_dev (the same file seen through two mounts; real copies almost never share ctime to the nanosecond). Add CLI tests using bind mounts if the CI runner allows `mount` (it may not; then unit-test the checks with injected metadata).

**S2. High. Empty files are planned and linked; later writes land in the library.** **[verified]**
- Where: `src/engine/collect.rs:87` and `158-164` (no size floor), `linking.rs:334-361`.
- Every empty download is linked to the first empty media file: an empty `.keep` was linked to the library's `.plexignore`. With `min_age` 0, a file a torrent client has created but not yet written qualifies; the client then writes into the shared inode and the library file changes. Tiny sidecars (a 9-byte .srt, .nfo files) are linked too, saving nothing and tying unrelated library files to downloads.
- Fix (tiny): skip `size == 0` in collect, whatever the floor. Then add `min_size`, default 1 MiB (P1, ADR 0002).

**S3. High. The silent symlink fallback is unsafe for *arr setups.** **[reproduced]**
- Where: `linking.rs:282-323`, `collect.rs:23-25` (the walk skips symlinks, so a symlinked download is never looked at again), `types.rs` `LinkMode { Best, SymlinkOnly }`.
- The hardlink fails, and DejaView quietly makes a symlink, when: the cron user does not own the library file and `fs.protected_hardlinks=1` (default on Debian/Ubuntu; EPERM); Docker mounts downloads and media separately or media is `:ro` (EXDEV even on one device); mergerfs path-preserving policies put the two files on different disks (EXDEV).
- Why it hurts: the symlink dangles when Sonarr/Radarr upgrades or deletes the library file, and the torrent breaks; a client re-downloading the missing file writes through the dangling link and creates a file in the library; a category move in the client breaks the relative link; container paths may not exist on the host.
- The planned row says "TRY HARDLINK" and the report's `fallback_reason` loses the errno (see C3).
- Fix: the symlink fallback stays the default (ADR 0001), so make it visible: make `config validate` try one real hardlink from media into downloads and explain EPERM (ownership, protected_hardlinks) separately from EXDEV (filesystem, mount or mergerfs branch); print "N linked as symlinks" in the summary; add a check that lists symlinks under downloads pointing into media that no longer resolve (scan warning or a `check` command).

**S4. Medium (High with separate users). A hardlinked download takes on the library file's owner, mode, ACLs, xattrs and SELinux label.** **[reproduced]**
- Where: `linking.rs:465-470` and `194-256`; nothing compares ownership.
- Example: download `qbt:qbt 0664`, library copy `arr:arr 0640`, DejaView run as root. After `link` the download is `arr:arr 0640`; the torrent user gets "Permission denied" and seeding stops. The report says applied/hardlink with no warning.
- Fix (small to medium): before linking, compare uid, gid and mode; skip (or require a flag) when the target gives the source's owner less access than the source had, and report why. Document ACL/xattr/SELinux (`:Z` labels) effects.

**S5. Medium. TUI `L` links files that were never shown for review.** **[reproduced]**
- Where: `src/tui/app.rs:488` (L opens the dialog with no scan required), `src/tui/actions.rs:131` (`run_link` starts a plain `link`, which rescans), dialog text at `src/tui/view/mod.rs:799`.
- Scan shows 8 rows; a new duplicate appears; L then y applies 9. L also works before any scan ("Estimated planned actions: 0") and links everything. The dialog reports "Rows with warnings or errors: 8" on a clean plan because every planned row carries the note "tries hardlink first; may fall back to symlink" in `fallback_reason`.
- Fix (medium): pass the reviewed rows to the child (a plan file with OS-string paths, or repeated `--only-source`), disable L until a scan has finished, count only rows with `error`, and drop "Current mode" from the dialog.

**S6. Low. Path-based link and rename after the safety checks (TOCTOU).** **[reasoned]**
- Where: `linking.rs:465-470`, `51-74`, `213-236`.
- A directory component of the source swapped for a symlink between the checks and the rename sends the rename outside downloads; run as root this replaces an arbitrary file. A target swapped for a symlink makes `linkat` hardlink the symlink, reported as "hardlink". Needs another local writer.
- Fix (medium): open the source's parent with `O_NOFOLLOW`, fstat-verify it, and use `linkat`/`renameat` relative to that dirfd; re-check the target with `AT_SYMLINK_NOFOLLOW`. Probably needs `libc` or `rustix`.

### Correctness and reports

**C1. Medium. Downloads that are hardlinks of each other (cross-seed) fail every first run, and the report overstates savings.** **[verified]**
- Where: `src/engine/types.rs:39-41` (source check includes ctime), `linking.rs:103-112`, `347-357`.
- `dl/t1/movie.mkv` and `dl/t2/movie.mkv` share one inode, not the library's. Replacing t1 changes the inode's link count and ctime, so t2 fails "source changed since planning". The report says applied 1, `total_saved_bytes` 12000000 (both planned paths, failed one included), while 0 bytes were freed (t2 still holds the data); exit code 0. `--strict` stops at the first failure, so a strict cron makes one group of progress per run.
- Fix (small): process paths sharing an inode back to back; after DejaView's own replacement, accept a ctime/nlink change on siblings whose size, mtime, dev and ino still match.

**C2. Medium. No lock against concurrent runs.** **[reproduced]**
- Where: `src/engine/mod.rs:131-174`, `report.rs:117-128`, `cache.rs:108-121`.
- Two `link` runs over 300 pairs: no data damage, but 129/171 and 171/129 spurious "source changed" failures, both exit 0, and the second overwrote the first's report. The cache is last-writer-wins (atomic, but hashes are lost). `cache cleanup-temp` during a run can delete an in-flight temp link and force a symlink fallback.
- Fix (small): a non-blocking exclusive lock (`File::try_lock`, decision 3) on `<downloads>/.dejaview.lock` for scan, link and cleanup-temp; exit with a clear message and a distinct code when held. Fix the README cron line too (absolute binary path, `flock -n`, stdout to /dev/null, dated `--report`).

**C3. Medium. Reports and errors don't match what happened.** **[reproduced]**
- Where: `src/engine/mod.rs:186` (total computed before apply), `report.rs:117` (one fixed path), `report.rs:130-160`, `linking.rs:252, 306, 476-479, 500-505`, `mod.rs:297-312`, `hashing.rs:316, 400`.
- `total_saved_bytes` in link mode is the planned sum, counted per path, not per inode, and includes failed entries (one 2 MB failure: reported 10488723, applied entries sum to 8391566).
- Failed entries keep the plan's `method: try-hardlink` and its `fallback_reason`.
- Errors are formatted with `err.to_string()`, which shows only anyhow's outer context, so the cause is lost: "mv ... ..." with no "Operation not permitted", "ln ...; temp hardlink path not created" with no EPERM/EXDEV. The README claims `fallback_reason` says which cause.
- `.dejaview_link_report.json` is overwritten on every run, including a no-op run, and a TUI single-row link leaves a 1-entry report. Reports carry no timestamp, version, roots or link mode.
- `--json --strict` prints nothing on stdout when it fails.
- Non-strict runs with failed links exit 0.
- Fix (small to medium): format errors with `{:#}` everywhere an anyhow error becomes a string; clear method and reason on failure; compute freed bytes from applied actions per inode (add `nlink` to FileInfo); add optional report fields `version`, `started_at`, `finished_at`, `downloads`, `media`, `link_mode`, `outcome` (complete, cancelled, failed), `symlinked` count, `already_linked` pairs/bytes, and split `planned_bytes` / `freed_bytes`; write timestamped reports or skip writing when nothing was planned; emit JSON before returning a strict error; document exit codes and add one for "completed with failures".

**C4. High (blocks Docker and hardened systemd). Logger setup is fatal and paths can't be overridden.** **[verified]**
- Where: `src/main.rs:169`, `src/logger.rs:35-40, 66, 70`, `src/config.rs:18-22`.
- An unwritable or uncreatable state dir makes every command fail, even `config show`, with a message that names no path (`XDG_STATE_HOME=/proc/x dejaview config show` gives "Error: No such file or directory (os error 2)"). With no HOME and no passwd entry (`docker run --user 1000` on a scratch image) every command fails with "Could not find state or config directory".
- Fix (small): make logger init best-effort with one stderr warning that names the path; add `DEJAVIEW_CONFIG` (or `--config`) and `DEJAVIEW_LOG` (a path, or `-` for stderr). CLI test with an unwritable state dir.

**C5. Low to Medium. mergerfs details.** **[reproduced]** on mergerfs 2.33.5
- Without `use_ino` (older mergerfs), hardlinked pool paths report different inodes: every `link` run re-plans and "applies" the same pair (the rename is a no-op), reports savings, and a remount changes inode numbers and forces a full rehash. With `use_ino` it was correct. The README's `inodecalc=hybrid-hash` advice is incomplete: mention `use_ino` for older versions.
- **[reasoned]** mergerfs `link-exdev`/`rename-exdev` options (present in 2.33.5, default `passthrough`) can turn a link into a symlink and still report success, so the report says "hardlink". Fix: after the rename, lstat the source and require a regular file with the target's dev/ino, else record the real method; treat "temp still exists after rename" as already linked.

**C6. Low to Medium. A download already hardlinked to a non-canonical library copy gets relinked.** **[reproduced]**
- Where: `linking.rs:345-350` (only the canonical `media[0]` inode is compared).
- The download shares an inode with `B Movies/m.mkv`; the library also has an identical `A Movies/m.mkv`. The download is relinked to A, reported as 3 MB saved, disk use unchanged, and the *arr hardlink is broken.
- Fix (tiny): skip a download whose (dev, ino) matches any media file in its hash group, or choose that file as its target.

**C7. Low. A permanent walk error disables cache pruning and makes `--strict` fail every run.** **[reproduced]**
- Where: `mod.rs:159-172`, `collect.rs:53-58`.
- An unreadable folder in the library (`#recycle`, `@eaDir`, `lost+found`) keeps stale cache entries forever and fails every strict run.
- Fix (small): prune entries not under a folder that failed to walk instead of skipping the prune; add exclude patterns (P8).

**C8. Low. `cache audit-temp` and `cleanup-temp` miss leftover symlink temp files.** **[reproduced]**
- Where: `src/engine/cache.rs:87` (`file_type().is_file()` is false for symlinks).
- A leftover `.dejaview_link.tmp.123.456` symlink is neither listed nor removed; the README says both are handled.
- Fix (tiny): accept `is_symlink()` too.

**C9. Low. Non-UTF-8 paths.** **[reproduced]**
- TUI `l` passes the lossy `display()` string as `--only-source`, so a non-UTF-8 source links nothing and reports no error; two names differing only in invalid bytes show identical sources in reports. Cache keys use `to_string_lossy` and collide (2 entries for 3 files), costing rehashes only (inode and ctime checks keep it safe).
- Fix: pass OS-string paths to the child (plan file); encode non-UTF-8 names losslessly in cache keys and reports.

**C10. Medium. Row order is random on every run (CLI table, JSON, TUI).** **[reproduced]**
- Where: `collect.rs:163` (`size_map.into_values()`), `linking.rs:373` (`hash_map.values()`).
- Three `scan --json` runs gave three orders; the TUI list reshuffles when the scan completes; reports can't be diffed.
- Fix (tiny): sort actions by source path (or size descending) once before building the table, report and events.

### Speed on a NAS

Measured by the reviewer on a 93,730-file library shaped like a Jellyfin/Kodi library (24k episodes with .nfo and thumbnails, 3k movies, posters), with real mergerfs and with an LD_PRELOAD shim adding 0.5 ms per stat/open/read/close and one shared 110 MB/s link ("sim"). Warm scan: local ext4 0.73 s, real mergerfs 7.75 s, sim NAS 91.2 s (walk 65.2 s, quick hash 25.7 s). P1 to P3 together are projected (not measured end to end) to bring the sim warm scan to about 4-5 s.

**P1. High. No minimum file size.** **[reproduced]**
- Where: `collect.rs:83-96` (the only filter is `min_age`).
- A 1 MiB floor cuts the files quick-hashed on a cold run of that library from 34,775 to 40 (groups 8,628 to 20); a 64 KiB floor leaves 2,981.
- Fix (small): `min_size` config and flag; skip before building FileInfo and before the TUI File event; report a `skipped_by_min_size` count like `skipped_by_min_age`.

**P2. High. Small files in shared-size groups are re-read on every run.** **[verified]**
- Where: `src/engine/hashing.rs:282-334` (phase 1 quick-hashes every physical file in a group that has any uncached member), `:326-332`, `sync_cache` at `:188-211` (only full-hashed files get entries).
- 200 same-size `tvshow.nfo` files with different content plus one same-size download: 201 files read on every scan, cache stays at 0 entries. On the 94k library: 34,525 files and 593 MB still re-read on the second (warm) run, against 34,775 on the cold run (25.7 s sim).
- Key fact: for files of 1 MiB or less, `sample_digest_from` reads the whole file and hashes it, so the quick hash already equals the full BLAKE3 (`full_digest` returns hex of the same value).
- Fix: for size <= 1 MiB, store the quick hash as the full hash and cache it (small, low risk; prototype gave 0 warm reads and an identical plan). For larger files, optionally add `sample: Option<...>` to `CacheEntry` with a serde default so old caches still load.
- The README's "rescanning ... costs very little" holds only for hardlinked videos; correct it.

**P3. High. The folder walk is single-threaded and is the floor for every run.** **[reproduced]**
- Where: `collect.rs:49-120` (WalkDir, one statx per file at :64).
- Real mergerfs: 4.2-5.7 s for 93.7k files vs 0.23 s on ext4. Prototype parallel walk (rayon over `read_dir` plus `symlink_metadata`): serial 5.25 s, 4 threads 1.45 s, 16 threads 0.67 s. Sim: serial 65 s, 16 threads 4.0 s.
- Fix (medium): parallel walk on rayon (decision 3) with its own concurrency (default about 16, not tied to CPU count); walk downloads and media at the same time; keep walk/metadata error counts, strict errors and cancel checks.

**P4. Medium. One thread setting for two kinds of I/O.** **[reproduced in sim]**
- Where: `main.rs:176-180` (rayon global pool, CPU count), `hashing.rs:282`, `:365`.
- Quick hashes are latency-bound (4 threads 25.7 s, 16 threads 8.6 s); full hashes are bandwidth-bound (more threads gain little, and on one spinning disk with seeks 4 threads were slower than 1 in the model).
- Fix (small): separate `sample_threads` (default about 16) and `full_threads` (default 1-2); queue quick-hash work per file, not per group, so one large same-size group doesn't run on one worker.

**P5. Medium-High. Ctrl+C, SIGTERM or a cron timeout on a CLI run throws away every finished hash.** **[reproduced]**
- Where: `main.rs:321-327` (cancel token wired only for `--tui-events`), `mod.rs:146-174` (cache saved once, after hashing).
- A first run over copied pairs can take hours on a NAS; any interruption restarts from zero.
- Fix: catch SIGINT/SIGTERM and call `CancelToken::cancel` (with `ctrlc`, decision 3); also save the cache periodically during the full pass (every few minutes or N GB).

**P6. Medium. TUI single-row link and link-all repeat the whole walk.** **[reproduced]**
- Where: `src/tui/actions.rs:127-148`, `src/engine/mod.rs:99-145`.
- Real mergerfs: `link --only-source <row>` took 6.67 s, of which 6.29 s walking; about 65 s per row in the sim.
- Fix (medium): pass the planned pair and both snapshots to the child, check both against the cache (the scan full-hashed both) and the existing pre-link checks, fall back to today's path otherwise. Do the same for L with the planned list (ties in with S5).

**P7. Low-Medium. Cache rewritten and fsynced every run; absolute-path keys.** **[reproduced]**
- Where: `mod.rs:174`, `cache.rs:14-34`, `fsutil.rs:15-46`.
- 200k entries: 78.5 MB file, 0.3 s load, 0.3-0.4 s save, 170 MB peak RSS (`read_to_string` holds the whole file). Two mount paths for the same share (Docker cron vs host TUI) prune each other's entries.
- Fix: skip the save when nothing changed; keys relative to the downloads or media root (old cache misses once); `from_reader(BufReader)`; optionally move the cache to `XDG_CACHE_HOME`.

**P8. Medium. Size groups that can't produce an action are still hashed; no exclude list.** **[reproduced]**
- Where: `hashing.rs:167-169` (only checks `inodes.len() < 2`), `collect.rs:49-63`.
- Groups with no download member, or no media member, are quick-hashed every run (20 unrelated same-size library files: 20 reads per run, never cached).
- NAS housekeeping folders (`@eaDir`, `#recycle`, `.Trash-*`) are walked and hashed; `#recycle` sorts first by bytes, so a recycle-bin copy can become the link target.
- Fix: drop groups lacking either side before hashing (tiny); add `exclude` glob lists per root with sensible defaults, applied with `filter_entry` so excluded trees are never read (small).

**P9. Low-Medium. Read path.** **[reproduced]**
- Where: `hashing.rs:26-55, 86-99`.
- `read_up_to` always does one extra read at end of file (a 4 KB file costs 2 reads; on mergerfs with `cache.files=off`, 20 of 30 daemon preads returned 0); a fresh zeroed 1 MiB buffer per quick hash; full hash reads 1 MiB at a time synchronously (10-15% under link rate in the sim).
- Fix: read exactly `min(size, 1 MiB)` for head and tail (size is known); reuse one buffer per thread; 4-8 MiB reads or one-chunk read-ahead for full hashes. Keep the short-read tolerance. BLAKE3 is not the limit (4.1 GB/s single thread).

**P10. Low. Per-link syscalls and an unbuffered log.** **[reproduced]**
- Where: `linking.rs:51-74` (two canonicalize per action, media root every time), `20, 86-101, 285-290` (target stat'ed three times), `:131` (`exists()` after rename), `logger.rs:93-108` (about 11 write calls per line).
- Fix: canonicalize the media root once per run; one lstat for the target; drop the post-rename `exists()`; write each log line with one `write_all`.

**P11. Low.** TUI event stream: 201k NDJSON lines (33.5 MB) for 93.7k files, one flush per event; it keeps up but costs 0.1-0.3 s locally. Throttle File/Count events to about every 100 ms and buffer stdout. Memory: about 0.7 KB per file (91 MB peak at 94k files); store `seen_paths` as 64-bit hashes and the hash as `[u8; 32]` if it matters.

### TUI and CLI usability

All reproduced by rendering the TUI in a pty with pyte at 60x20, 80x24, 100x30, 120x40, 200x50 unless marked.

**U1. High. Status feedback is invisible.** **[reproduced; confirmed in code]**
- Where: `src/tui/view/mod.rs:474` (a Paragraph over up to 64 log lines with no scroll: it shows the oldest lines); `app.rs:977`, `app.rs:853` (Cancelled and `fail_running_job` don't update `latest_activity_message`).
- After the first scan the Run Log is stuck on "TUI started... Running scan... Phase: COLLECTING FILES". "Scan finished", "Saved report to...", "Link run had 1 failure(s)", "Scan cancelled" never show. Progress lines logged every 10 files fill the ring. After cancelling: "IDLE READY TO SCAN ... 0 planned ... Verifying duplicate files: 2 / 8", which reads as "no duplicates". First run with no config: S gives "Scan failed: scan failed: Error: downloads path not set" (doubled prefix, no hint to press e).
- Fix (small): render the last N lines; set the activity line on Done, Cancelled and Error; keep per-file progress out of the log ring; open the path editor when paths are unset. Add 3-4 TestBackend render tests (view coverage is 49%, layout 0%).

**U2. High. Result rows can't be told apart.**
- Where: `view/mod.rs:639` (`trim_to_width(&row.source...)` ignores `expand_paths`).
- 350 episode rows all render as `/tmp/claude-0/-h....H.264-GROUP.mkv`; at 60 columns the SOURCE column shows just `/`. No SIZE or TARGET column. `p` only affects the sidebar and activity lines.
- Fix (small-medium): show source relative to the downloads root, trim keeping the file name, add SIZE and (when wide) TARGET relative to media, make `p` apply to the table.

**U3. Medium-High. At 80x24 the confirm dialogs clip their own y/n prompt.**
- Where: `src/tui/layout.rs:16-19` (fixed rows 9 + min 10 + 6 + 2 = 27 > 24), dialogs sized by percentage (`centered_rect`).
- LINK ALL / LINK SELECTED lose "Press y to continue or n/q to cancel."; Help stops at "R save report"; Run Log is 1 line; footer loses "? help"; metric titles truncate to "PLANN"/"APPL". At 60x20 the wordmark is cut and footer and log vanish.
- Fix (medium): collapse the wordmark below about 30 rows, hide the sidebar below about 100 columns, size dialogs to content, put "y link / n cancel" in the dialog border title.

**U4. Medium. Selection and navigation.**
- Where: `app.rs:1005` (`selected_row = 0` on Done), `app.rs:477` (only j/k/arrows), `app.rs:436` (j closes the JSON preview), `view/mod.rs:691` (JSON overlay has no scroll).
- The selected row differs only by background `#1c1f31` vs `#0b0d18`. Linking one row jumps back to row 0. No PgUp/PgDn/Home/End/g/G. JSON preview of a 350-entry report shows one entry and can't scroll; Help can't scroll either.
- Fix (small): highlight symbol plus reverse video; after a single link select the next planned row; paging keys; scrollable JSON/help overlays that close only on Esc/q/J.

**U5. High. The TUI's subprocess protocol has no structured error path.** **[reproduced]**
- Where: `src/engine/mod.rs:48-60`, `297-304`; `src/tui/actions.rs:34`, `100-116`; `src/tui/app.rs:991-1000` (compare `853-864`).
- Strict failures during scan/hash send no `error` event; the TUI shows raw stderr (116 lines with `RUST_BACKTRACE=1`). A strict link failure after some files were linked leaves both rows "PLANNED", header planned=2 applied=1, and no report in the TUI although the report on disk is correct. After the binary is replaced on disk (`cargo install`, package upgrade), `current_exe()` returns `<path> (deleted)` and an open TUI can't start any run. The protocol has no version.
- Fix (medium): keep the subprocess (kill-safe cancel, crash isolation). In tui-events mode emit `Error { message: format!("{err:#}"), report: Option<RunReport> }` for every error except Cancelled and have the TUI adopt the partial report; first event `hello { protocol, version }` checked by the parent; spawn `/proc/self/exe` on Linux; set `RUST_BACKTRACE=0` for the child; CLI tests that the stream ends in exactly one done/error/cancelled event for strict hash failure, strict link failure, cancel and config error.

**U6. Medium. CLI output.**
- Link with a failure prints "Potential space saved", warnings, "Done" and exits 0; with 0 applied there is no "Applied" line; a successful link never states space actually freed. Nothing found prints "Found duplicates:" over an empty table. The table header is misaligned because the coloured header cells count escape bytes (`linking.rs:384`). Each row repeats both absolute roots (350 rows is about 1,750 lines at 80 columns). No quiet mode for cron; `--json` mails the whole report.
- Fix (small): "Linked 7 of 8, 8.0 MiB freed, 1 failed: see <report>" and "No duplicates found."; uncoloured header; print roots once and paths relative; `--quiet` (print only when something was linked or failed); exit code for completed-with-failures (C3).

**U7. Low-Medium. `x` clears the cache with no confirmation.** Lowercase x (next to c, not documented) wipes the cache, which costs a full library rehash. Confirm X/x. Where: `app.rs:483`.

**U8. Medium. Error messages and the path editor.**
- `config set-downloads /srv/nope` gives "No such file or directory (os error 2)" without the path (`normalize_runtime_dir`). The TUI editor does not expand `~`, doesn't name the failing field, and clips long errors ("media path is inside the downloads path (/tmp/claude-0/-home-user/fd53d022-d"). The editor tip says "saving to the repo config file" (`view/mod.rs:785`) but the file is `~/.config/dejaview/config.json`.
- Fix (small): path context in `normalize_runtime_dir`, `~` expansion, wrapped status text, correct the tip.

**U9. Medium. Progress for large files.** The TUI sits on "0 / 8 duplicate files verified" for each large file (a 60 GB remux at 150 MB/s is about 7 minutes with no movement); the CLI bar counts files only. The counter also counts cancelled reads as done (`hashing.rs:387` increments before checking the result). Fix: bytes done of total, MB/s, ETA; count only successful hashes.

**Feature ideas worth building:** hardlink-only mode and dangling-symlink check (S3); `min_size` and per-root exclude globs (P1, P8); mark rows with space and link the marked set with L, plus a persistent skip list such as `.dejaview_skip.json`, since each `l` currently re-walks both trees (U2, P6).

### Maintainability, tests, packaging, CI

**M1. High value. Stringly-typed values.** "planned" appears at 17 sites, "applied" at 11, "try-hardlink" at 10; each of the 5 phase strings at 5-10 sites; 20 `eq_ignore_ascii_case` comparisons. Where: `src/protocol.rs`, `src/engine/report.rs:130-185`, `linking.rs:471-478`, `src/tui/app.rs`, `view/mod.rs`. Replace with enums (`Mode`, `EntryStatus`, `LinkMethod`, `Phase`) with serde renames so the JSON stays byte-identical; add a golden test serialising a RunReport and a few TuiEvents against checked-in JSON; merge `RunWarnings`/`WarningSummary` and the duplicate `warning_total` and `format_size`/`format_bytes`. About 300 mechanical lines, low risk.

**M2. High value. Releases and MSRV.** The real minimum Rust is exactly 1.88 (`cargo +1.88 build/test --locked` passes; 1.87 fails: ratatui 0.30.2, comfy-table, darling and instability need 1.88; let-chains need 1.88). Debian 13's rustc 1.85 can't build it. `cargo build --release --target x86_64-unknown-linux-musl` works out of the box (60 s, 3.3 MB static binary); aarch64-musl from x86 fails in blake3's C build without a cross sysroot. Recommend: `rust-version = "1.89"` in Cargo.toml (decision 3, for the lock); tag v0.1.0; a tag-triggered workflow building x86_64 and aarch64 `*-unknown-linux-musl` (aarch64 on `ubuntu-24.04-arm` or with cross), stripped, with sha256sums, attached to a GitHub Release; README install section using it; `[profile.release]` with `strip = true`, `lto = "thin"`. Optional container: `FROM scratch` plus the binary, `ENV XDG_CONFIG_HOME=/config XDG_STATE_HOME=/config`, run with `--user PUID:PGID`, mount `/data` with the same layout as the torrent client and *arrs, schedule from the host. Needs C4 first.

**M3. Medium. CI hardening.** CI uses `@stable` with `-D warnings`, so a new Rust release can break an unrelated change. Pin the toolchain in `rust-toolchain.toml`; add `--locked` to clippy and test; add a `cargo +1.89 check --locked --all-targets` job; add a weekly cargo-audit job (prebuilt via `taiki-e/install-action`). cargo-audit 0.22.2 currently reports 0 vulnerabilities across 228 lockfile crates. The lockfile's apparent duplicates (syn 1, nix 0.29, bitflags 1, thiserror 1) come only from ratatui's termwiz backend, which isn't compiled (a clean release build compiles 112 crates).

**M4. Medium. Test gaps.**
- Done in `just test`, which reruns the unit tests as `nobody` under root. A bare `cargo test` as root still reports the five as `ok`.
- TUI tests `starting_run_clears_stale_report_rows` and `rescan_clears_cache_then_starts_scan` really spawn the test binary with `scan --tui-events --no-progress`. Inject the job spawner into App so tests never spawn a process.
- Untested: cross-device symlink fallback end to end (CI can use `/dev/shm` when its device differs from temp_dir), corrupt cache (today silently replaced by an empty one at `cache.rs:18` with no log line; `cache info` shows 0 and a multi-TB library is rehashed without explanation; log a warning), concurrent runs, partial-failure and cancel reports, the TUI strict path (U5). `run_scan_respects_min_age_filter` has no positive control.
- Line coverage is 75.6%.

**M5. Medium. One definition of "same physical file".** The (size, mtime_ns, ctime_ns, dev, inode) tuple lives in FileInfo, FileSnapshot and CacheEntry, with two metadata-to-nanoseconds conversions and two comparison functions (`collect.rs:79-91`, `linking.rs:76-101`, `hashing.rs:147-153`, `types.rs`). Add `FileSnapshot::from_metadata`, embed FileSnapshot in FileInfo and in CacheEntry with `#[serde(flatten)]` (cache JSON unchanged), keep `matches_source`/`matches_target` as the only comparisons.

**M6. Medium. Guardrails for agent-driven changes.**
- Done: the check script (as `just check` and `just test`), the Invariants section (`AGENTS.md`, "What DejaView never does"), and the old rule 3, which went with the role protocol.
- `#![deny(clippy::print_stdout, clippy::print_stderr)]` on `src/engine` with `#[allow]` on the few human-output functions, to protect the NDJSON stream mechanically.
- Test boilerplate: 8 full `ScanRunOptions { .. }` literals, 8 copies of `unique_temp_dir`, 7 `WarningSummary` and 7 `TuiEvent::Count` literals; adding one field touches about 30 sites. Give `ScanRunOptions` a test builder; add `tests/common/mod.rs`; a temp-dir guard that cleans up on drop (or the `tempfile` dev-dependency, which needs approval).
- Clippy pedantic gives about 90 warnings, almost all style; not worth enabling wholesale.

**M7. Medium-Low. Split only where the bugs are.** Extract TUI event handling (`apply_tui_event`, `merge_report`, live counters, about `app.rs:814-1148`) into a pure `RunState` type testable without threads (that's where U5 lives). Move `validate_config` and `probe_directory_writable` from `main.rs` into `config.rs` as one `check()` shared by `config validate`, the runtime guard and the TUI editor (today the runtime guard skips the writability check and validate skips a hardlink probe). Not worth it: splitting `view/mod.rs` into files, or moving the engine in-process.

**M8. Low. macOS.** `cargo check --target x86_64-apple-darwin --all-targets` passes (with `CC_x86_64_apple_darwin=clang`; blake3 needs a C compiler). At runtime `dirs` uses `~/Library/Application Support` and ignores XDG, unlike the README. No CI job needed; at most a README note.

**M9. Low. Small cleanups.** `test-fixtures/fake-library/README.txt` describes a gitignored dataset with no generator in the repo (add the generator or drop it). Hide `tui-logo-preview` from `--help`. Log timestamps are unix seconds and newlines in messages aren't escaped.

### Docs to correct

- README "Cheap repeat runs" and "rescanning ... costs very little": true for hardlinked videos, not for small sidecars (P2).
- README mergerfs section: add `use_ino` for older mergerfs (C5) and the symlink-fallback risks (S3).
- README says `fallback_reason` names the cause; it doesn't until C3 is fixed.
- README cron example: absolute binary path, `flock -n`, stdout to /dev/null, dated report (C2).
- README says the config/log follow XDG; on macOS they don't (M8).

## Suggested order

1. Safety: S1, S2 (empty files), S3, S4, S5, C1, C6, C2 (lock), C4 (logging), C5, then S6.
2. NAS speed: P1, P2, P8, P3, P4, P7, then P5 and P6, then P9, P10, P11.
3. Reports and UI: C3, C10, U1, U5, U2, U3, U4, U6, U7, U8, U9, C7, C8, C9.
4. Maintenance and release: M1, M5, M6, M3, M2, M4, M7, M9, M8, docs.
