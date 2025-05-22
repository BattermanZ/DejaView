# DejaView

**DejaView** is a Rust-based CLI utility designed to detect and replace duplicate media files across separate folders (e.g., a download directory and an organised media library). It scans for exact duplicates by file size and BLAKE3 hash, then replaces redundant copies in the download tree with relative symbolic links pointing to the canonical file in the media library. A persistent JSON cache speeds up subsequent runs by skipping unchanged files.

---

## Features

* **Size-prefiltered, hash-verified duplicates**: Groups files by size and confirms duplicates using BLAKE3 hashes.
* **Persistent cache**: Stores `(path, size, mtime, hash)` in `.dejaview_cache.json` to skip rehashing unchanged files.
* **Relative symlinks**: Creates portable links that remain valid if the directory structure is mounted identically (important for Docker setups).
* **Concurrency**: Utilises Rayon to parallelise hashing across multiple CPU cores.
* **Progress UI**: Combines `indicatif` spinners and bars for a responsive terminal experience.
* **Table output**: Renders a dynamic, wrapped table of planned replacements using `comfy_table` and ANSI colours.
* **Dry-run support**: Preview planned actions without modifying any files.

---

## Installation

1. Ensure you have Rust and Cargo installed (via [rustup](https://rustup.rs/)).

2. Add required dependencies to `Cargo.toml`:

   ```toml
   [dependencies]
   anyhow = "1.0"
   clap = { version = "4.5", features = ["derive"] }
   colored = "2.1"
   indicatif = "0.17"
   pathdiff = "0.2"
   rayon = "1.8"
   blake3 = "1.5"
   serde = { version = "1.0", features = ["derive"] }
   serde_json = "1.0"
   walkdir = "2.5"
   comfy-table = "7.1"
   ```

3. Build the project:

   ```bash
   cargo build --release
   ```

---

## Usage

Run from your project root (where `Cargo.toml` lives):

```bash
./target/release/dejaview \
  --downloads /path/to/downloads \
  --media     /path/to/media \
  [--dry-run] [--threads <n>] [--min-age <seconds>]
```

* `-d, --downloads` \<path>: Directory where torrents are stored and seeded.
* `-m, --media` \<path>: Organised media library directory.
* `-n, --dry-run`: Show planned deletions and symlinks without applying changes.
* `--threads` \<n>: Override Rayon thread-pool size (defaults to physical cores).
* `--min-age` \<secs>: Ignore files modified in the last N seconds (default: 300).

**Example**:

```bash
cargo run --release -- \
  -d /mnt/mediazilla/downloads \
  -m /mnt/mediazilla/tv \
  -n
```

---

## How it works

1. **File collection**: Recursively scans both the download and media directories, skipping symlinks and files younger than `min-age`.
2. **Duplicate grouping**: Buckets files by size; only buckets with more than one file enter the hashing phase.
3. **Hashing**: Computes a full BLAKE3 digest for each candidate file, with thread-safe caches to avoid redundant work.
4. **Cache update**: Writes updated `(path, size, mtime, hash)` entries to `.dejaview_cache.json` in the download root.
5. **Duplicate resolution**: Builds a table of `(download path, media path)` pairs for identical files.
6. **Symlink replacement**: (Non-dry-run) Deletes download copies and creates relative symlinks to the media library.

---

## Best Practices

* **Maintain mount consistency** for relative symlinks (e.g., Docker bind `/mnt/mediazilla` identically).
* **Commit `Cargo.lock`** in your repo to lock dependency versions for reproducible builds.
* **Clean cache** periodically by deleting `.dejaview_cache.json` if directory structure drastically changes.
* **Run as needed**; you can schedule via cron or invoke manually after media imports.

---

## License

MIT
