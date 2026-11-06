# Changelog

## Unreleased

### After upgrading

- Files under 1 MiB are now skipped by default, so `.nfo` files, text subtitles and artwork are no longer linked. Set `dejaview config set-min-size 1` to link small files again.

### Added

- `dejaview config set-min-size BYTES` and `--min-size BYTES` on `scan` and `link` set the smallest file a run considers, and the report counts what the floor skipped in `skipped_by_min_size`.

### Fixed

- Empty files are never linked. Before, every empty download was linked to an empty library file, and a torrent client that later wrote into a freshly created file wrote into the library.
