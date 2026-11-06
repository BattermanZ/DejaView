# DejaView: How It Works

This guide explains the app in simple terms and shows the flow with Mermaid diagrams.

## 1) Big Picture

DejaView compares two folders:
- `downloads`: where duplicates usually exist
- `media`: the canonical library, which DejaView only reads

It finds exact duplicates and can replace each duplicate file in `downloads` with:
- a hardlink (preferred when possible)
- a relative symlink (fallback)

The two folders must be separate: neither may contain the other.

```mermaid
flowchart LR
    A[downloads folder] --> C[DejaView scan engine]
    B[media folder] --> C
    C --> D[duplicate plan]
    D -->|scan| E[preview + summary]
    D -->|link| F[apply hardlink/symlink]
    F --> G[report JSON]
```

## 2) CLI Command Map

```mermaid
flowchart TD
    ROOT[dejaview] --> SCAN[scan]
    ROOT --> LINK[link]
    ROOT --> TUI[tui]
    ROOT --> CFG[config]
    ROOT --> CACHE[cache]

    CFG --> SD[set-downloads]
    CFG --> SM[set-media]
    CFG --> SMA[set-min-age]
    CFG --> SMS[set-min-size]
    CFG --> ST[set-threads]
    CFG --> SFT[set-full-threads]
    CFG --> SEX[set-exclude]
    CFG --> SLM[set-link-mode]
    CFG --> CV[validate]
    CFG --> CS[show]

    CACHE --> CC[clear]
    CACHE --> CI[info]
    CACHE --> CAT[audit-temp]
    CACHE --> CCT[cleanup-temp]

    SCAN --> SJSON[--json]
    SCAN --> SREP[--report]
    SCAN --> SPROG[--progress/--no-progress]
    SCAN --> SSTRICT[--strict]
    SCAN --> SSYM[--symlink-only]

    LINK --> LJSON[--json]
    LINK --> LREP[--report]
    LINK --> LPROG[--progress/--no-progress]
    LINK --> LSTRICT[--strict]
    LINK --> LSYM[--symlink-only]
    LINK --> LCONF[--confirm]

    TUI --> TSTRICT[--strict]
```

## 3) Run Pipeline (Scan + Link)

```mermaid
flowchart TD
    A[Load config] --> B{downloads/media set, valid and not overlapping?}
    B -- no --> X[Stop with actionable error]
    B -- yes --> C[Collect files from downloads + media]
    C --> D[Skip symlinks, .dejaview_ files, files under min_size and files younger than min_age]
    D --> E[Group by size]
    E --> F[Within a group, paths of one physical file count once]
    F --> G{More than one physical file?}
    G -- no --> SKIP[Skip group: nothing to read]
    G -- yes --> H{Every physical file cached?}
    H -- yes --> K[Use cached hashes]
    H -- no --> I[Sample hash pass]
    I --> J[Full hash pass for colliding samples]
    J --> K
    K --> L[Save cache, drop entries for paths not seen]
    L --> M[Build duplicate actions]
    M --> N{Mode?}
    N -- scan --> O[Show/emit plan only]
    N -- link --> P[Optional --confirm]
    P --> Q[Apply actions: hardlink first, then symlink fallback]
    Q --> R[Write report + print summary]
    O --> R
```

A run started from the TUI can be cancelled at any point. During collecting and hashing it stops almost at once, even partway through a large file, and still saves the hashes it finished. During the apply step it stops before the next file.

## 4) Link Method Decision

Before any replacement, DejaView checks that the source is outside the media folder, that the target is inside it and is not a symlink, and that neither file changed since the scan.

```mermaid
flowchart TD
    A[Need to replace a duplicate file] --> S{symlink-only mode?}
    S -- yes --> E[Relative symlink]
    S -- no --> B{same filesystem device?}
    B -- yes --> C[Try hardlink]
    C -- success --> D[Done: method=hardlink]
    C -- fail --> E
    B -- no --> E
    E --> F[Done: method=symlink + fallback_reason]
```

Either link is created under a temporary name next to the download and then renamed over it, so the download is never missing.

## 5) What Gets Stored

```mermaid
flowchart LR
    A[~/.config/dejaview/config.json]:::cfg --> B[Runtime]
    B --> C[downloads/.dejaview_cache.json]:::cache
    B --> D[report JSON: --report, or downloads/.dejaview_link_report.json for link]:::report
    B --> E[downloads/.dejaview_tui_report.json when saved from the TUI]:::report
    B --> F[~/.local/state/dejaview/dejaview.log]:::log

    classDef cfg fill:#e8f1ff,stroke:#3b82f6,stroke-width:1px;
    classDef cache fill:#ecfeef,stroke:#16a34a,stroke-width:1px;
    classDef report fill:#fff7ed,stroke:#f97316,stroke-width:1px;
    classDef log fill:#f4f4f5,stroke:#71717a,stroke-width:1px;
```

## 6) Output Modes

- Human mode (default): table + summary in terminal
- JSON mode (`--json`): machine-readable JSON on stdout
- File report (`--report <path>`): same structured data saved to disk

```mermaid
flowchart TD
    A[Run command] --> B{--json?}
    B -- yes --> C[Emit JSON to stdout]
    B -- no --> D[Emit human table + summary]
    C --> E{--report given or default link report?}
    D --> E
    E -- yes --> F[Write JSON report file]
    E -- no --> G[No report file written]
```

## 7) First-Run Checklist

1. Set paths:
   - `dejaview config set-downloads /path/to/downloads`
   - `dejaview config set-media /path/to/media`
2. Validate setup:
   - `dejaview config validate`
3. Preview first:
   - `dejaview scan`
4. Apply:
   - `dejaview link --confirm`

## 8) Typical Examples

Preview (human):
```bash
dejaview scan
```

Preview (automation):
```bash
dejaview scan --json --no-progress --report /tmp/scan.json
```

Apply with confirmation:
```bash
dejaview link --confirm
```

Apply for automation:
```bash
dejaview link --json --no-progress --report /tmp/link.json
```
