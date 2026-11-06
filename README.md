# DejaView

Find files in your downloads folder that are byte-for-byte copies of files in your media library, and replace each copy with a hardlink to the library file.

This is for the usual torrent plus Sonarr/Radarr setup, where a finished download keeps seeding from the downloads folder while the library holds its own copy. When an import copied instead of hardlinking, that file is stored twice. DejaView finds those pairs by content, not by name, and turns the download into a hardlink, so the data is stored once and the torrent client keeps seeding. It never writes to the library.

```text
$ dejaview scan
▶ Found duplicates:
╭──────────────────────────────────────────────────────────────────┬─────────────────────────────────────────────╮
│ Download                                                         ┆ Media                                       │
╞══════════════════════════════════════════════════════════════════╪═════════════════════════════════════════════╡
│ /srv/torrents/complete/Arrival.2016.1080p/Arrival.2016.1080p.mkv ┆ /srv/media/Movies/Arrival (2016).mkv        │
├╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┼╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┤
│ /srv/torrents/complete/Show.S01/Show.S01E01.mkv                  ┆ /srv/media/TV/Show/Season 1/Show S01E01.mkv │
╰──────────────────────────────────────────────────────────────────┴─────────────────────────────────────────────╯
💾 Potential space saved: 6.10 MB
ℹ Files considered: 6
ℹ Skipped by min-size: 0
ℹ Skipped by min-age: 0
ℹ Planned actions: 2
ℹ Dry-run; no changes.
✔ Done.
```

There is also a terminal dashboard, `dejaview tui`, for scanning, reviewing and linking row by row.

## What you get

- **Matching by content.** Files are grouped by size, then compared with BLAKE3 hashes: a quick hash of the first and last MiB, and a full hash only when quick hashes collide. Names and folders play no part.
- **Hardlink first.** On the same filesystem the download becomes a hardlink and the space comes back. Across filesystems, or if the hardlink fails, DejaView falls back to a relative symlink and records why in the report.
- **Cheap repeat runs.** A cache in the downloads folder remembers hashes. Hardlinked paths are hashed once, and an already-linked pair with no other file of the same size is not read at all, so rescanning a library that the *arr apps already hardlink costs very little.
- **Dry run by default.** `scan` only shows the plan. `link` changes files, and `link --confirm` asks first.
- **Reports.** `--json` prints a run summary for scripts, and every `link` run writes a JSON report of what it changed.
- **Terminal dashboard.** Live progress per hashing thread, link one row or all of them, and cancel a running scan or link with `c`.

## Who it is for

You keep a downloads folder and a separate media library, and you want each file the two share to take the space of one copy.

What it is not:

- Not a general duplicate finder. It only acts on a file in downloads that has an identical file in media. Duplicates inside the library, or inside downloads with no library copy, are left alone.
- Not an organiser. It never renames, moves or deletes a file outright; it replaces a download with a link to identical content.
- Not for Windows. It relies on Unix file metadata (inodes and hardlinks).

## Safety model

- **The library is read-only to DejaView.** Every replaced file is in downloads, and a link run refuses any source that resolves inside the media folder.
- **Downloads and media must be separate folders.** If one is inside the other, or both are the same folder (also through a symlink), `scan`, `link`, `config validate` and the TUI path editor refuse to run. A bind mount can show one folder at two paths, so every walk also compares each folder's device and inode with the other root, and a run that finds the library inside downloads that way stops before changing anything.
- **A library file is never mistaken for a download.** A download that shares an inode with any library file is already linked and left alone. One on another device with the same size and timestamps as a library file is that file seen through a second mount, such as a mergerfs branch, and is left alone too; the run warns and counts it in the report's `skipped_as_library_file`.
- **Each replacement is atomic.** DejaView creates the link under a temporary name next to the download, then renames it over the download. An interrupted run leaves either the old file or the link, plus at worst a `.dejaview_*.tmp.*` file that `dejaview cache cleanup-temp` removes.
- **Nothing changes behind its back.** Right before replacing a file, it checks the size, timestamps and inode of both files again and skips the pair if either changed since the scan. It also refuses a target that is a symlink or sits outside the media folder. On Linux it then opens the download's folder once and does the rest through that open folder, so a folder swapped for a symlink in the meantime cannot send the replacement anywhere else, and it checks that the link it made is the planned library file before that link replaces the download.
- **A link never locks out the torrent client.** A hardlinked download is the library file, so it takes on that file's owner and permissions (and its ACLs, extended attributes and SELinux label); a symlinked one is read with them. Before linking, DejaView checks that the download's owner and group could still read it, and leaves the pair alone with the reason in the report when they could not.
- **`scan` leaves your files alone.** It does write its cache, `.dejaview_cache.json`, in the downloads folder, so downloads must be writable.

## Quick start

You need a Rust toolchain ([rustup](https://rustup.rs/)). From a clone of this repository:

```bash
cargo install --path .
```

Point DejaView at your two folders:

```bash
dejaview config set-downloads /srv/torrents
```

```bash
dejaview config set-media /srv/media
```

```bash
dejaview config validate
```

See what it would do, then do it:

```bash
dejaview scan
```

```bash
dejaview link --confirm
```

## Usage

### Commands

| Command | What it does |
|---|---|
| `dejaview scan` | Find duplicates and print the plan. Changes no files except the cache. |
| `dejaview link` | Find duplicates and replace each download copy with a link. |
| `dejaview tui` | Open the terminal dashboard. |
| `dejaview config show` | Print the configuration. |
| `dejaview config validate` | Check that both folders exist and are separate (also through a mount), downloads is writable and media is readable, and try one real hardlink from the library into downloads to say whether links will be hardlinks or symlinks, and why. |
| `dejaview config set-downloads PATH` | Set the downloads folder. The path is stored resolved and absolute. |
| `dejaview config set-media PATH` | Set the media folder. |
| `dejaview config set-min-age SECONDS` | Skip files modified more recently than this. |
| `dejaview config set-min-size BYTES` | Skip files smaller than this. At least 1: empty files are always skipped. |
| `dejaview config set-threads N` | Set the number of hashing threads. |
| `dejaview config set-link-mode best\|symlink-only` | Choose hardlink with symlink fallback, or symlinks only. |
| `dejaview cache info` | Show where the cache is and how many entries it holds. |
| `dejaview cache clear` | Delete the cache. The next scan hashes everything again. |
| `dejaview cache audit-temp` | List temporary link files left in downloads by an interrupted run. |
| `dejaview cache cleanup-temp` | Delete them. |

### Options for scan and link

| Option | Effect |
|---|---|
| `--json` | Print the run summary as JSON on stdout. Turns progress output off. |
| `--report PATH` | Also write the summary to PATH. Without it, `link` writes `.dejaview_link_report.json` in downloads and `scan` writes nothing. |
| `--progress auto\|on\|off` | Progress bars. `auto` (the default) shows them on a terminal. |
| `--no-progress` | Same as `--progress off`. |
| `--strict` | Exit with an error if any file could not be read, hashed or linked. |
| `--symlink-only` | Use symlinks for this run, never hardlinks. |
| `--min-size BYTES` | Skip files smaller than this for this run, in place of the configured floor. |
| `--confirm` | `link` only. Ask before changing anything. Needs a terminal and cannot be combined with `--json`. |

For a cron job:

```bash
dejaview link --json --no-progress --report /var/log/dejaview-link.json
```

### Terminal dashboard

```bash
dejaview tui
```

Add `--strict` to run the dashboard's scans and links in strict mode.

| Key | Action |
|---|---|
| `S` | Scan |
| `X` | Clear the cache, then scan |
| `l` | Link the selected row (asks first) |
| `L` | Link every planned row (asks first) |
| `c` | Cancel the running scan or link |
| `j` / `k` or arrows | Move the selection |
| `e` | Edit the downloads and media paths |
| `p` | Show full or shortened paths |
| `R` | Save the current report to `.dejaview_tui_report.json` in downloads |
| `J` | Show the current report as JSON |
| `?` | Help |
| `q` | Close an overlay, or quit when no run is active |

Cancelling a scan stops it almost at once, even partway through a large file, and keeps every hash already finished in the cache, so the next scan picks up where it stopped. Cancelling a link stops before the next file: files already linked stay linked and the rest stay planned. Quitting is blocked while a run is active, so cancel first.

Linking a single row lists both folders again to re-check that row, but only hashes files of the same size as it.

## Configuration

The configuration lives in `~/.config/dejaview/config.json` (under `$XDG_CONFIG_HOME` when that is set). Change it with the `config` commands rather than by hand.

| Setting | Default | Purpose |
|---|---|---|
| `downloads` | unset | Folder whose duplicates get replaced. Must be writable. |
| `media` | unset | The library. Read, never written. |
| `min_age` | `0` | Seconds. Files modified more recently are skipped, which keeps DejaView away from files a client is still writing. |
| `min_size` | `1048576` (1 MiB) | Bytes. Smaller files are skipped: `.nfo` files, text subtitles and artwork save almost nothing and cost a read on every run. Empty files are always skipped, because a torrent client creates a file empty before writing into it. |
| `threads` | number of CPUs | Hashing threads. Lower it if parallel reads thrash spinning disks. |
| `link_mode` | `best` | `best` hardlinks and falls back to relative symlinks. `symlink-only` always uses relative symlinks. |

### Files DejaView writes

| File | When |
|---|---|
| `<downloads>/.dejaview_cache.json` | Every scan and link. Entries for files that no longer exist are dropped after each complete run. |
| `<downloads>/.dejaview_link_report.json` | Every link run without `--report`. |
| `<downloads>/.dejaview_tui_report.json` | When you press `R` in the dashboard. |
| `~/.local/state/dejaview/dejaview.log` | Every command (under `$XDG_STATE_HOME` when set). Once it passes 5 MiB it moves to `dejaview.log.1`, replacing the previous one, so the two together stay near 10 MiB. |

Files whose name starts with `.dejaview_` are never treated as duplicates.

## mergerfs and network drives

- **Check mergerfs's `inodecalc`.** DejaView recognises an existing hardlink by its inode number. The default, `inodecalc=hybrid-hash`, gives hardlinks one inode, which is what DejaView expects. With `path-hash`, every path gets its own inode, so already-linked pairs look like copies and get hashed on every run; at link time DejaView finds they are one file, changes nothing and reports them as `already-linked`. With `passthrough`, different files on different branches can report the same inode. DejaView only treats two paths as one file when their timestamps match as well, but `hybrid-hash` is the setting to use.
- **Older mergerfs needs `use_ino`.** Versions without `inodecalc` give each path of a hardlinked file its own inode unless mounted with `use_ino`, with the same `already-linked` result on every run.
- **`link-exdev` and `rename-exdev`.** mergerfs can answer a hardlink across disks with a symlink and report success. DejaView checks what was made and reports it as `symlink` with that reason, not as a hardlink.
- **Hardlinks cannot cross mergerfs branches.** If the download and the library copy sit on different underlying disks, the hardlink fails and DejaView falls back to a symlink. The report shows `symlink` and the reason, and the summary counts them. Such a symlink dangles once Sonarr or Radarr upgrades the library file; scans list those that do.
- **Flaky connections.** If the folder walk hits errors, DejaView keeps the cache entries for paths it could not see, so a dropped connection does not force a full rehash on the next run.
- **Docker.** Symlinks are relative, so they resolve the same inside a container as long as both folders are mounted with the same layout.

## Troubleshooting

**`dejaview: command not found` after installing.** Older builds installed the binary as `DejaView`. Run `cargo uninstall DejaView`, then `cargo install --path .` again, and check that `~/.cargo/bin` is on your `PATH`.

**`config validate` says the media path is inside the downloads path.** The two folders overlap, so a link run would rewrite files in your library. Point media at the library folder itself. The two can share a parent, such as `/srv/media` and `/srv/torrents`, but neither can contain the other.

**`link` made symlinks instead of hardlinks.** The summary says how many, and each entry's `fallback_reason` in the link report gives the cause and the OS error: different filesystems or mounts (two mergerfs disks, two Docker volumes), or not permitted (the library file belongs to another user and `fs.protected_hardlinks` lets only its owner hardlink it). `dejaview config validate` tries one real hardlink and says the same. A symlink breaks when Sonarr or Radarr upgrades, renames or deletes its library file; every scan and link lists the symlinks in downloads that point into the library and no longer resolve (`dangling_symlinks` in the `--json` report).

**A pair failed with "could no longer read".** The library file belongs to another user and is closed to the download's owner or group, so after linking the torrent client could not read its own file. Give the torrent client and Sonarr or Radarr a shared group and group-readable files (umask 002), or make the library world-readable, then run again. ACLs and SELinux labels are not compared: in Docker with SELinux, a `:Z` label on the library mount relabels files the downloads container may then not read, so use `:z` on both.

**A pair failed with "source changed since planning".** The download changed between the scan and the link, usually because a client was still writing it. Run again later, or raise `min_age`.

**Files named `.dejaview_hardlink.tmp.*` or `.dejaview_link.tmp.*` in downloads.** A link run was killed in the middle of a replacement. `dejaview cache audit-temp` lists them and `dejaview cache cleanup-temp` deletes them.

**A scan reads a file it read before.** A cache entry is valid only while the file's size, modification time, change time and inode stay the same. A link run or a `chmod` changes the change time, so that file is hashed once more.

## How it works

1. Walk both folders without following symlinks. Skip files smaller than `min_size` (and every empty file), files younger than `min_age`, and DejaView's own files.
2. Group files by size. Paths that are already the same file count once, and a group that is all one file is skipped.
3. Quick hash (BLAKE3 of the first and last MiB) of each remaining file, unless every file in the group is already cached.
4. Full BLAKE3 hash of the files whose quick hashes collide.
5. In each set of identical files, the first media file by path is the target. Every download copy that is not already a link to it gets an action.
6. `link` only: check both files again, link to a temporary name, rename it over the download.

[docs/HOW_IT_WORKS.md](docs/HOW_IT_WORKS.md) has diagrams of the pipeline and the link decision.

## Development

```bash
cargo test
```

An ignored performance harness times a cold and a warm scan on generated datasets:

```bash
cargo test --release --test perf_harness -- --ignored --nocapture
```

Each profile has a time limit you can tighten with `DEJAVIEW_PERF_SMALL_COLD_MS`, `DEJAVIEW_PERF_SMALL_WARM_MS`, and the matching `MEDIUM` and `LARGE` variables (milliseconds).

`dejaview tui-logo-preview` shows the wordmark variants the dashboard chooses between.

## License

GNU General Public License v3.0. See [LICENSE](LICENSE).
