# Changelog

## Unreleased

### After upgrading

- A scan, link or `cache cleanup-temp` that finds another run going exits with code 3 and changes nothing. A cron line that alerts on any non-zero code should treat 3 as nothing to report; the README's cron example does.
- A report entry's `status` can now also be `already-linked` (the download already was the library file, so nothing changed; such pairs used to be reported as `applied`) or `skipped` (left alone on purpose, with the reason in `error`). A script that counts statuses should know both.
- A pair that mergerfs's `link-exdev` option turned into a symlink is reported with `method` `symlink`, where it used to say `hardlink`.
- Building DejaView needs Rust 1.89 or newer.
- Files under 1 MiB are now skipped by default, so `.nfo` files, text subtitles and artwork are no longer linked. Set `dejaview config set-min-size 1` to link small files again.

### Added

- `DEJAVIEW_CONFIG` sets the config file's path and `DEJAVIEW_LOG` the log's, or `-` for stderr, for containers and services run as a user with no home folder.
- `dejaview config validate` tries one real hardlink from the library into downloads and says whether link runs will make hardlinks, and if not, why: different filesystems or mounts, or ownership and `fs.protected_hardlinks`.
- Scans and links list symlinks in downloads that point into the library and no longer resolve, on stderr and in the report's `dangling_symlinks`.
- A link run's summary says how many downloads became symlinks.
- `dejaview config set-min-size BYTES` and `--min-size BYTES` on `scan` and `link` set the smallest file a run considers, and the report counts what the floor skipped in `skipped_by_min_size`.

### Fixed

- Only one scan, link or `cache cleanup-temp` runs at a time per downloads folder. Before, two link runs failed each other's pairs, the later one overwrote the other's report and cache, and `cache cleanup-temp` could delete a link another run was about to rename into place.
- A download that already is its library file is reported as `already-linked` and counted apart from applied pairs. On mergerfs without `use_ino` or `inodecalc=hybrid-hash`, which gives one file two inode numbers, such pairs were reported as applied on every run.
- In the dashboard, `L` links only the planned rows the last scan showed, each to the library file it showed, and does nothing before a scan has finished; `l` likewise links its row only to the library file shown. Before, `L` ran a fresh link over everything, including duplicates that appeared after the scan and, before any scan, all of them. The confirmation counts rows with errors only.
- A log that cannot be written no longer stops every command: DejaView warns once, naming the path, and runs without it. Before, an unwritable state folder failed even `config show`, with an error that named no path.
- A run whose downloads folder reaches the library through a bind mount, or the reverse, stops before changing anything, and `config validate` reports it. Before, only paths were compared, so a library mounted inside downloads was walked as downloads and its files were rewritten.
- A download already hardlinked to one library copy is left alone when the library holds a second identical copy. Before, it was relinked to the copy whose path sorted first, which broke the hardlink Sonarr or Radarr made and freed nothing.
- A download that is a library file seen through a second mount (a bind mount, or a mergerfs branch while media points at the pool) is left alone, and the run warns and counts it in the report's `skipped_as_library_file`. Before, such a file could be replaced by a link to itself or to another copy, destroying the library's only copy.
- Downloads that are hardlinks of each other, as cross-seeding makes, are all linked in one run. Before, replacing the first path failed every other one with "source changed since planning", and a `--strict` run stopped there each time.
- A pair is left alone, with status `skipped` and the reason in the report, when linking would leave the download unreadable to its owner or group; the run, `--strict` included, carries on with the rest. Before, a hardlink handed the download the library file's owner and mode, and a torrent client locked out of its own file stopped seeding.
- A pair that fell back to a symlink has a `fallback_reason` naming the cause and the OS error. Before, it said only which command failed.
- On Linux, a download is replaced through its folder opened once, and both the new link and the download are checked against the plan right before the rename, so a folder or library path swapped during a run cannot redirect the replacement, and a library file replaced mid-run fails the pair instead of being linked. A download whose folder resolves outside the downloads folder is refused.
- Empty files are never linked. Before, every empty download was linked to an empty library file, and a torrent client that later wrote into a freshly created file wrote into the library.
