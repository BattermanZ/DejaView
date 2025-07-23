# DejaView

**DejaView** is a Rust-based CLI utility designed to detect and replace duplicate media files across separate folders (e.g., a download directory and an organised media library). It scans for exact duplicates by file size and BLAKE3 hash, then replaces redundant copies in the download tree with relative symbolic links pointing to the canonical file in the media library. A persistent JSON cache speeds up subsequent runs by skipping unchanged files.

---

## Features

*   **Size-prefiltered, hash-verified duplicates**: Groups files by size and confirms duplicates using BLAKE3 hashes.
*   **Two-phase hashing**: Uses a fast sample hash to quickly filter out non-duplicates, then performs a full hash only on potential matches.
*   **Persistent cache**: Stores `(path, size, mtime, hash)` in `.dejaview_cache.json` to skip rehashing unchanged files.
*   **Relative symlinks**: Creates portable links that remain valid if the directory structure is mounted identically (important for Docker setups).
*   **Concurrency**: Utilises Rayon to parallelise hashing across multiple CPU cores.
*   **Progress UI**: Combines `indicatif` spinners and bars for a responsive terminal experience.
*   **Table output**: Renders a dynamic, wrapped table of planned replacements using `comfy_table` and ANSI colours.
*   **Space saved calculation**: Reports the total disk space that would be saved by linking duplicates.

---

## Installation

1.  Ensure you have Rust and Cargo installed (via [rustup](https://rustup.rs/)).

2.  Build the project:

    ```bash
    cargo build --release
    ```

---

## Usage

DejaView is now controlled via subcommands. All commands should be run from your project root (where `Cargo.toml` lives) using `cargo run -- <subcommand> [args...]`.

### 1. Initial Configuration (One-Time Setup)

Before running any scans or links, you need to tell DejaView where your downloads and media folders are. This is stored in a configuration file (`~/.config/dejaview/config.json`) so you only need to do it once.

*   **Set Downloads Path:**
    ```bash
    cargo run -- config set-downloads /path/to/your/downloads
    ```

*   **Set Media Path:**
    ```bash
    cargo run -- config set-media /path/to/your/media
    ```

*   **Show Current Configuration:**
    ```bash
    cargo run -- config show
    ```

### 2. Scanning for Duplicates (Dry Run)

This command will scan your configured directories for duplicates and report them, including the potential space savings. No files will be modified.

```bash
cargo run -- scan
```

### 3. Linking Duplicates

This command will perform the actual file operations: deleting duplicates in your downloads folder and replacing them with symbolic links to the media folder.

```bash
cargo run -- link
```

### 4. Cache Management

DejaView uses a cache to speed up subsequent runs. You can manage it with these commands:

*   **Clear Cache:**
    ```bash
    cargo run -- cache clear
    ```

*   **Show Cache Info:**
    ```bash
    cargo run -- cache info
    ```

---

## How it works

1.  **File collection**: Recursively scans both the configured download and media directories, skipping symlinks and files younger than the `min-age` setting (default: 300 seconds).
2.  **Duplicate grouping**: Buckets files by size; only buckets with more than one file enter the hashing phase.
3.  **Two-phase Hashing**: 
    *   **Phase 1 (Sample Hash):** For files in a group, a fast BLAKE3 hash is computed using only the first and last megabyte of the file.
    *   **Phase 2 (Full Hash):** Only if multiple files share the same sample hash, a full BLAKE3 digest is computed for each of them to confirm true duplication. This significantly reduces the number of full file reads.
4.  **Cache update**: Writes updated `(path, size, mtime, hash)` entries to `.dejaview_cache.json` in the download root.
5.  **Duplicate resolution**: Builds a table of `(download path, media path)` pairs for identical files.
6.  **Symlink replacement**: (Only for `link` command) Deletes download copies and creates relative symlinks to the media library.

---

## Best Practices

*   **Maintain mount consistency** for relative symlinks (e.g., Docker bind `/mnt/mediazilla` identically).
*   **Run `scan` first** to preview changes before running `link`.
*   **Clear cache** periodically by running `cargo run -- cache clear` if your directory structure drastically changes or if you suspect cache inconsistencies.
*   **Schedule via cron or invoke manually** after media imports for automated cleanup.

---

## License

MIT