# A download that cannot be hardlinked becomes a symlink

DejaView exists to store each file once. On a mergerfs pool a download and its library copy are often on different disks, where no hardlink can join them, so refusing to link would leave the copy and save nothing. The default link mode stays `best`: a hardlink where the filesystem allows one, and a relative symlink into the library where it does not. Settled on 2026-11-06, against the review's suggestion to make hardlink-only the default (`docs/FINDINGS.md`, S3).

## Consequences

A symlinked download depends on the library file staying where it is. When Sonarr or Radarr upgrades, renames or deletes that file, the symlink points at nothing and the torrent stops seeding. That is the accepted price, so DejaView's job is to make every symlink visible rather than avoid it. The rest of S3's fixes follow from this:

- the report's `fallback_reason` names the OS error (C3);
- the summary says how many downloads became symlinks;
- `config validate` tries one real hardlink and says which cause applies: different disks, or file ownership and `fs.protected_hardlinks`, which a person can fix to get hardlinks instead;
- a check lists symlinks in the downloads folder that point into the library and no longer resolve.

## Considered options

- **Hardlink-only as the default, skipping a pair that cannot be hardlinked.** Rejected: on mergerfs it keeps every copy whose two files sit on different disks, which defeats the point of DejaView.
