Fake dataset for manual DejaView testing.

Structure:
- downloads/
- media/

Notes:
- Large files are sparse, so apparent size is large but disk usage is small.
- Matching files across downloads/media are intentional duplicates.
- Files marked unique differ in content and should not be linked.

Fixture groups:
- Duplicate pairs:
  - 4 KiB: dupe-small-4k.bin
  - 10 MiB: dupe-medium-10m.bin
  - 512 MiB: dupe-large-512m.bin
  - 2 GiB: dupe-huge-2g.bin
  - 3 GiB: season-01/dupe-ultra-3g.bin <-> library/season-01/dupe-ultra-3g.bin
  - 5 GiB: archives/dupe-massive-5g.bin <-> archive/dupe-massive-5g.bin
  - 7 GiB: extras/Concert Night [1080p] {sample}-7g.bin
- Same-size but different content:
  - 128 MiB: same-size-unique-128m.bin
  - 3 GiB: extras/same-size-different-content-3g.bin
- One-sided files:
  - downloads-only-64m.bin
  - downloads-only-9g.bin
  - media-only-96m.bin
  - media-only-11g.bin

Sparse-file reminder:
- `ls -lh` shows logical size.
- `du -h` shows actual disk usage.
- Copying these files with some tools can expand them into real fully allocated files.
