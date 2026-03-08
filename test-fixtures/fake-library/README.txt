Fake dataset for manual DejaView testing.

Structure:
- downloads/
- media/library/

Notes:
- Files are sparse, so logical size is large while real disk usage stays small.
- Matching files across downloads/media are intentional duplicates.
- Same-size files with different seeded prefixes should hash differently and not link.
- The fixture tree starts clean with no `.dejaview_*.tmp.*` artifacts.

Fixture groups:
- Exact duplicate pairs:
  - `downloads/movies/dupe-alpha-8m.bin` <-> `media/library/movies/dupe-alpha-8m.bin`
  - `downloads/concerts/live-show-3g.bin` <-> `media/library/concerts/live-show-3g.bin`
  - `downloads/season-one/episode-s01e01-700m.bin` <-> `media/library/season-one/episode-s01e01-700m.bin`
  - `downloads/boxsets/anthology-disc-a-11g.bin` <-> `media/library/boxsets/anthology-disc-a-11g.bin`
- Multi-copy duplicates in downloads:
  - `downloads/season-one/episode-s01e01-copy-700m.bin`
  - `downloads/boxsets/anthology-disc-a-copy-11g.bin`
  - `downloads/boxsets/anthology-disc-a-remux-11g.bin`
- Same size, different content:
  - `downloads/misc/same-size-different-content-256m.bin`
  - `media/library/misc/same-size-different-content-256m.bin`
- Downloads-only files:
  - `downloads/misc/downloads-only-5g.bin`
  - `downloads/misc/Multi Part Finale (Disc 2 of 3) - director cut sample.bin`
- Media-only files:
  - `media/library/misc/media-only-9g.bin`
  - `media/library/misc/[Archived] Bonus Feature {1080p}.bin`
  - `media/library/concerts/live-show-alt-copy-3g.bin`

Useful checks:
- `find test-fixtures/fake-library -type f | sort`
- `du -h test-fixtures/fake-library`
- `ls -lh test-fixtures/fake-library/downloads/boxsets`

Suggested local config:
- downloads: `test-fixtures/fake-library/downloads`
- media: `test-fixtures/fake-library/media/library`
