# Files under 1 MiB are left alone

DejaView skips any file smaller than 1 MiB (1,048,576 bytes) by default, in both the downloads folder and the library, and a person can lower or raise the floor in the config. Below 1 MiB sit `.nfo` files, text subtitles and small artwork: linking them saves almost nothing, but each one is a read on every run over a network drive, and on a mergerfs pool each one that crosses disks becomes a symlink that can break when the library is renamed (ADR 0001). In the review's test library the floor cut the files read on a first run from 34,775 to 40 (`docs/FINDINGS.md`, P1). Settled on 2026-11-06.

## Consequences

- **Empty files are always skipped, whatever the floor.** Every empty file matches every other, and a torrent client creates a file empty before it writes into it, so linking one sends the client's writes into the library (S2). The floor cannot be set below 1 byte.
- A run reports how many files the floor skipped, beside the count `min_age` skipped.

## Considered options

- **Skip only empty files.** Rejected: it keeps tens of thousands of reads per run to save a few kilobytes each.
- **A 10 MB floor.** Rejected as the default: it saves no reads worth having over 1 MiB, and it would skip what does save space between the two, such as image subtitles from Blu-ray rips and music tracks, which a Lidarr setup seeds by the thousand. A person whose downloads hold only films and series can set it.
