# Changelog

## Unreleased

### After upgrading

- A scan, link or `cache cleanup-temp` that finds another run going exits with code 3 and changes nothing. A cron line that alerts on any non-zero code should treat 3 as nothing to report; the README's cron example does. [#3]
- A report entry's `status` can now also be `already-linked` (the download already was the library file, so nothing changed; such pairs used to be reported as `applied`) or `skipped` (left alone on purpose, with the reason in `error`). A script that counts statuses should know both. [#3]
- A pair that mergerfs's `link-exdev` option turned into a symlink is reported with `method` `symlink`, where it used to say `hardlink`. [#3]
- Building DejaView needs Rust 1.89 or newer. [#3]
- Files under 1 MiB are now skipped by default, so `.nfo` files, text subtitles and artwork are no longer linked. Set `dejaview config set-min-size 1` to link small files again. [#3]
- `threads` now sets how many files are walked and quick-hashed at once, 16 when unset, where it was every hash's thread count and the number of CPUs when unset. Full reads have their own `full_threads`, 2 when unset; set it to 1 for a single spinning disk.
- The cache file changes form on the first run: each entry is keyed by its folder and its path below it, and may hold a quick hash only. The change hashes nothing again, but a version older than this one reads the new cache as unusable and hashes every candidate again.
- Walks now leave out the folders a NAS or an OS keeps for itself: `@eaDir`, `#recycle`, `#snapshot`, `@Recycle`, `.@__thumb`, `.recycle`, `.snapshots`, `.Trash-*`, `$RECYCLE.BIN` and `lost+found`. A file in one of them is no longer linked, nor used as the library file a download links to. `dejaview config set-exclude` changes the list, and with no pattern leaves nothing out.

### Added

- `dejaview config set-full-threads N` sets how many files are read in full at once.
- `dejaview config set-exclude [PATTERN]...` sets the file and folder names every walk leaves out, as globs, with everything under them.
- `DEJAVIEW_CONFIG` sets the config file's path and `DEJAVIEW_LOG` the log's, or `-` for stderr, for containers and services run as a user with no home folder. [#3]
- `dejaview config validate` tries one real hardlink from the library into downloads and says whether link runs will make hardlinks, and if not, why: different filesystems or mounts, or ownership and `fs.protected_hardlinks`. [#3]
- Scans and links list symlinks in downloads that point into the library and no longer resolve, on stderr and in the report's `dangling_symlinks`. [#3]
- A link run's summary says how many downloads became symlinks. [#3]
- `dejaview config set-min-size BYTES` and `--min-size BYTES` on `scan` and `link` set the smallest file a run considers, and the report counts what the floor skipped in `skipped_by_min_size`. [#3]

### Changed

- The same share mounted at two paths, such as in a Docker cron job and on the host the dashboard runs on, now uses one set of cache entries. Before, each run pruned the other's entries as gone and the next one hashed them again.
- A run that learns nothing new leaves the cache file as it is. Before, every run rewrote and flushed it, tens of megabytes on a large library.
- Downloads and media are walked at once, many folders in parallel, so a run on a network drive no longer waits on one `stat` after another before hashing starts.
- A repeat scan reads only files that are new or changed: the cache now keeps each file's quick hash too, so a file whose quick hash matched no other is not read again, and a file of 1 MiB or less is read once, since its quick hash covers it whole. Before, every file sharing its size with a new or changed one was read again on every run.
- Files of a size that no download shares with a library file are no longer read, nor are those where every download already is a library file: no link can come of them.

### Fixed

- Ctrl+C, SIGTERM (a cron timeout, `docker stop`) or SIGHUP stops a scan or link before its next file and keeps the hashes it finished; a second Ctrl+C stops at once. Before, it ended the process on the spot and every hash of the run was lost, so a first run over a network drive that was interrupted started over from nothing. While hashing, the cache is also saved every five minutes.
- A cache file that cannot be read or parsed is named in the log with the reason before DejaView starts over with an empty one. Before, it was replaced silently, and the next run hashed the whole library again with no explanation.
- The dashboard's Run Log shows its newest lines, and the activity line says how the last run ended. Before, the log stayed on its first few lines, so "Scan finished", a failed link and the reason for it (such as another run holding the lock) showed nowhere. The link confirmations show `y link` and `n cancel` on their border, so a small terminal no longer cuts the prompt off. [#3]
- Only one scan, link or `cache cleanup-temp` runs at a time per downloads folder. Before, two link runs failed each other's pairs, the later one overwrote the other's report and cache, and `cache cleanup-temp` could delete a link another run was about to rename into place. [#3]
- A download that already is its library file is reported as `already-linked` and counted apart from applied pairs. On mergerfs without `use_ino` or `inodecalc=hybrid-hash`, which gives one file two inode numbers, such pairs were reported as applied on every run. [#3]
- In the dashboard, `L` links only the planned rows the last scan showed, each to the library file it showed, and does nothing before a scan has finished; `l` likewise links its row only to the library file shown. Before, `L` ran a fresh link over everything, including duplicates that appeared after the scan and, before any scan, all of them. The confirmation counts rows with errors only. [#3]
- A log that cannot be written no longer stops every command: DejaView warns once, naming the path, and runs without it. Before, an unwritable state folder failed even `config show`, with an error that named no path. [#3]
- A run whose downloads folder reaches the library through a bind mount, or the reverse, stops before changing anything, and `config validate` reports it. Before, only paths were compared, so a library mounted inside downloads was walked as downloads and its files were rewritten. [#3]
- A download already hardlinked to one library copy is left alone when the library holds a second identical copy. Before, it was relinked to the copy whose path sorted first, which broke the hardlink Sonarr or Radarr made and freed nothing. [#3]
- A download that is a library file seen through a second mount (a bind mount, or a mergerfs branch while media points at the pool) is left alone, and the run warns and counts it in the report's `skipped_as_library_file`. Before, such a file could be replaced by a link to itself or to another copy, destroying the library's only copy. [#3]
- Downloads that are hardlinks of each other, as cross-seeding makes, are all linked in one run. Before, replacing the first path failed every other one with "source changed since planning", and a `--strict` run stopped there each time. [#3]
- A pair is left alone, with status `skipped` and the reason in the report, when linking would leave the download unreadable to its owner or group; the run, `--strict` included, carries on with the rest. Before, a hardlink handed the download the library file's owner and mode, and a torrent client locked out of its own file stopped seeding. [#3]
- A pair that fell back to a symlink has a `fallback_reason` naming the cause and the OS error. Before, it said only which command failed. [#3]
- On Linux, a download is replaced through its folder opened once, and both the new link and the download are checked against the plan right before the rename, so a folder or library path swapped during a run cannot redirect the replacement, and a library file replaced mid-run fails the pair instead of being linked. A download whose folder resolves outside the downloads folder is refused. [#3]
- Empty files are never linked. Before, every empty download was linked to an empty library file, and a torrent client that later wrote into a freshly created file wrote into the library. [#3]

[#3]: https://github.com/BattermanZ/DejaView/pull/3
