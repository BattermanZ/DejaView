# Changelog

## Unreleased

### After upgrading

- Files under 1 MiB are now skipped by default, so `.nfo` files, text subtitles and artwork are no longer linked. Set `dejaview config set-min-size 1` to link small files again.

### Added

- `dejaview config validate` tries one real hardlink from the library into downloads and says whether link runs will make hardlinks, and if not, why: different filesystems or mounts, or ownership and `fs.protected_hardlinks`.
- Scans and links list symlinks in downloads that point into the library and no longer resolve, on stderr and in the report's `dangling_symlinks`.
- A link run's summary says how many downloads became symlinks.
- `dejaview config set-min-size BYTES` and `--min-size BYTES` on `scan` and `link` set the smallest file a run considers, and the report counts what the floor skipped in `skipped_by_min_size`.

### Changed

- A report entry can have the status `already-linked`: at link time the download turned out to be the library file already (mergerfs without `use_ino` or `inodecalc=hybrid-hash` gives one file two inode numbers), so nothing changed. Before, such pairs were reported as applied, with their size counted as saved, on every run.

### Fixed

- A run whose downloads folder reaches the library through a bind mount, or the reverse, stops before changing anything, and `config validate` reports it. Before, only paths were compared, so a library mounted inside downloads was walked as downloads and its files were rewritten.
- A download already hardlinked to one library copy is left alone when the library holds a second identical copy. Before, it was relinked to the copy whose path sorted first, which broke the hardlink Sonarr or Radarr made and freed nothing.
- A download that is a library file seen through a second mount (a bind mount, or a mergerfs branch while media points at the pool) is left alone, and the run warns and counts it in the report's `skipped_as_library_file`. Before, such a file could be replaced by a link to itself or to another copy, destroying the library's only copy.
- Downloads that are hardlinks of each other, as cross-seeding makes, are all linked in one run. Before, replacing the first path failed every other one with "source changed since planning", and a `--strict` run stopped there each time.
- A pair is left alone, with the reason in the report, when linking would leave the download unreadable to its owner or group. Before, a hardlink handed the download the library file's owner and mode, and a torrent client locked out of its own file stopped seeding.
- A pair that fell back to a symlink has a `fallback_reason` naming the cause and the OS error. Before, it said only which command failed.
- A link that mergerfs's `link-exdev` option turned into a symlink is reported as `symlink` with the reason, not as `hardlink`.
- On Linux, a download is replaced through its folder opened once, and the link is checked against the planned library file before it replaces the download, so a folder or library path swapped for a symlink during a run cannot redirect the replacement. A download whose folder resolves outside the downloads folder is refused.
- Empty files are never linked. Before, every empty download was linked to an empty library file, and a torrent client that later wrote into a freshly created file wrote into the library.
