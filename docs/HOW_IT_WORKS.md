# DejaView: How It Works

This guide explains the app in simple terms and shows the flow with Mermaid diagrams.

## 1) Big Picture

DejaView compares two folders:
- `downloads`: where duplicates usually exist
- `media`: canonical library

It finds exact duplicates and can replace duplicate files in `downloads` with:
- hardlinks (preferred when possible)
- symlinks (fallback)

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
    ROOT[DejaView] --> SCAN[scan]
    ROOT --> LINK[link]
    ROOT --> CFG[config]
    ROOT --> CACHE[cache]

    CFG --> SD[set-downloads]
    CFG --> SM[set-media]
    CFG --> SMA[set-min-age]
    CFG --> ST[set-threads]
    CFG --> CV[validate]
    CFG --> CS[show]

    CACHE --> CC[clear]
    CACHE --> CI[info]

    SCAN --> SJSON[--json]
    SCAN --> SREP[--report]
    SCAN --> SPROG[--progress/--no-progress]
    SCAN --> SSTRICT[--strict]

    LINK --> LJSON[--json]
    LINK --> LREP[--report]
    LINK --> LPROG[--progress/--no-progress]
    LINK --> LSTRICT[--strict]
    LINK --> LCONF[--confirm]
```

## 3) Run Pipeline (Scan + Link)

```mermaid
flowchart TD
    A[Load config] --> B{downloads/media set and valid?}
    B -- no --> X[Stop with actionable error]
    B -- yes --> C[Collect files from downloads + media]
    C --> D[Skip files younger than min_age]
    D --> E[Group by size]
    E --> F[Sample hash pass]
    F --> G[Full hash pass for candidates]
    G --> H[Build duplicate actions]
    H --> I{Mode?}
    I -- scan --> J[Show/emit plan only]
    I -- link --> K[Optional --confirm]
    K --> L[Apply actions: hardlink first, then symlink fallback]
    L --> M[Write report + print summary]
    J --> M
```

## 4) Link Method Decision

```mermaid
flowchart TD
    A[Need to replace a duplicate file] --> B{same filesystem device?}
    B -- yes --> C[Try hardlink]
    C -- success --> D[Done: method=hardlink]
    C -- fail --> E[Fallback to relative symlink]
    B -- no --> E
    E --> F[Done: method=symlink + fallback_reason]
```

## 5) What Gets Stored

```mermaid
flowchart LR
    A[~/.config/dejaview/config.json]:::cfg --> B[Runtime]
    B --> C[downloads/.dejaview_cache.json]:::cache
    B --> D[report JSON --report or default link report]:::report

    classDef cfg fill:#e8f1ff,stroke:#3b82f6,stroke-width:1px;
    classDef cache fill:#ecfeef,stroke:#16a34a,stroke-width:1px;
    classDef report fill:#fff7ed,stroke:#f97316,stroke-width:1px;
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
   - `DejaView config set-downloads /path/to/downloads`
   - `DejaView config set-media /path/to/media`
2. Validate setup:
   - `DejaView config validate`
3. If testing with very fresh files, set:
   - `DejaView config set-min-age 0`
4. Preview first:
   - `DejaView scan`
5. Apply:
   - `DejaView link --confirm`

## 8) Typical Examples

Preview (human):
```bash
DejaView scan
```

Preview (automation):
```bash
DejaView scan --json --no-progress --report /tmp/scan.json
```

Apply with confirmation:
```bash
DejaView link --confirm
```

Apply for automation:
```bash
DejaView link --json --no-progress --report /tmp/link.json
```
