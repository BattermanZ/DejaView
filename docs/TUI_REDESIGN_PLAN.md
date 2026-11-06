# DejaView TUI Redesign Plan

> **Status: implemented in March 2026.** This plan was carried out in commit `c13a8ec` (`feat(tui): rebuild visual dashboard`) and the fixes after it. The progress strip it calls out as out of scope was added afterwards with live engine events. The [README](../README.md#terminal-dashboard) lists the current keys and behaviour.

## Purpose
This document captures the proposed second-pass visual redesign for the DejaView TUI before implementation.

The current TUI is functionally usable, but the visual direction is not meeting the intended product goals. The redesign should move away from a boxed, filled-panel look and toward a cleaner, more deliberate terminal dashboard with stronger hierarchy, clearer status encoding, and a more cohesive media-workspace identity.

This plan is implementation guidance only. It does not apply code changes by itself.

## Goals
- Replace the current filled-surface look with a blacker, cleaner canvas.
- Reduce the feeling of "three big boxes plus a log box".
- Make the results workspace the clear focal point.
- Keep the left sidebar always visible, but visually quieter and more navigational.
- Make the right inspector structured, outcome-first, and easier to scan.
- Preserve visible keyboard hints and full-width logs.
- Use Catppuccin-inspired dark neutrals with teal as the primary accent.
- Make warning, fallback, and failure states obvious.
- Keep the TUI technical and modern, not playful or soft.

## Design Principles

### 1. Black Stage, Minimal Fill
- Use a near-black global canvas as the dominant background.
- Avoid large mid-gray filled areas.
- Reserve filled surfaces for compact cards, chips, selected rows, and focused controls.
- Prefer separators, rails, title bars, and row treatments over large colored rectangles.

### 2. Strong Hierarchy
- Create three visual text tiers:
  - product / section headline
  - panel / subsection label
  - metadata / body text
- Use spacing and grouping to establish hierarchy first.
- Use color to reinforce hierarchy, not replace it.

### 3. Modular Interface Pieces
- Replace long paragraphs with modular UI units:
  - chips
  - metric cards
  - row states
  - inspector sections
  - activity strip
- Make each region feel intentional and compositional.

### 4. Honest Status
- Do not fabricate percentages, throughput numbers, or item-level savings that do not exist.
- Progress UI may show state and activity, but it must remain truthful to the data available.

## Visual Direction

### Palette
- Base canvas: almost black
- Surface lines: dark neutral
- Primary accent: Catppuccin teal
- Secondary title accent: lavender
- Success: green
- Warning: yellow
- Danger: red

### Styling Rules
- Accent color is for:
  - selected/focused states
  - activity strip
  - key actions
  - status chips
  - important titles
- Avoid using accent on every border and every heading.
- Reduce broad panel fills; emphasize edges and local components instead.

## Layout Direction

### Header
Replace the current top area with a more cohesive command deck:
- Left: product banner / brand block
- Right: compact dashboard cluster

Header contents:
- DejaView wordmark
- short subtitle or operating mode label
- current run state
- compact metric cards:
  - files
  - planned
  - applied
  - saved
- persistent activity strip
- truthful progress presentation:
  - state
  - progress context
  - no invented numeric progress unless actual data exists

### Main Body
Retain the conceptual four-zone structure, but rebalance it:
- left: slim persistent sidebar
- center: dominant results workspace
- right: compact structured inspector
- bottom: full-width run log

Target proportions:
- left should be narrower than now
- center should dominate
- right should be compact but useful

### Footer
- Keep always-visible keyboard hints
- Reduce to the highest-value commands only
- Present them as compact chips or tagged hints rather than a dense sentence

## Panel Plans

### Sidebar
Current problem:
- Reads like a paragraph dump inside a box

Target structure:
- `PATHS`
- `SETTINGS`
- `ACTIONS`

Behavior:
- Keep always visible
- Use truncation for long paths
- Favor labels + values instead of wrapped paragraphs
- Use minimal chrome

### Results Workspace
Current problem:
- Large slab, weak hierarchy, too much empty fill

Target structure:
- prominent panel title
- smaller, stronger set of columns
- more row-level encoding

Proposed columns:
- State
- Source
- Outcome

Optional future column:
- Saved
Only if actual per-row saved space is available and truthful

Row treatment:
- selected row highlighted by color, not inversion
- state displayed as chips
- outcome visually emphasized
- source path treated as the primary readable row content

Empty state:
- clean and minimal
- one short headline
- one next action
- no wall of instructional text

### Inspector
Current problem:
- Still too text-forward and not sufficiently modular

Target structure:
- `OUTCOME`
- `RUN TOTAL`
- `SOURCE`
- `TARGET`
- `NOTES`

Rules:
- outcome is the most visually prominent item
- total saved is clearly labeled as run-level, not row-level
- paths are subordinate to outcome
- notes/fallback/errors use obvious semantic coloring

### Run Log
- Keep full width
- Keep visually secondary to results
- Use low-chrome treatment
- Preserve legibility and a stable title

## Progress / Activity Strategy

### Musts
- Always visible
- Feels alive
- Emphasizes current activity

### Truthfulness Constraint
- No fake percentages
- No fake throughput
- No fabricated counts

### What to show with current data
- run state: idle / scanning / linking / completed
- latest log line
- whether a subprocess is active
- stable labels such as:
  - `SCAN ACTIVE`
  - `LINK ACTIVE`
  - `READY`
  - `DONE`

### Future enhancement path
If real engine progress becomes available later:
- replace placeholder state strip with actual progress ratio
- show current phase and counts derived from engine events

## Typography Strategy
- Product banner should be visually dominant.
- Section titles should be stronger and more separated from content.
- Metadata labels should be quieter.
- Body text should remain readable monospace, but not all text should have equal emphasis.
- Use whitespace and grouping to create hierarchy rather than trying to fake font-size differences everywhere.

## Implementation Scope

### Files Expected To Change
- `src/tui/view/mod.rs`
- `src/tui/theme.rs`
- `src/tui/layout.rs`
- `src/tui/app.rs`

### File Likely Not To Change Much
- `src/tui/model.rs`
- `src/tui/events.rs`
- `src/tui/actions.rs`

### Why
- Most of the redesign is render-layer and presentation-state work.
- Engine behavior should remain unchanged.
- Avoid scope creep into scan/link business logic.

## Implementation Steps

### Step 1. Reset the Visual Foundation
- Reduce the number of filled panel backgrounds.
- Introduce a blacker app canvas.
- Rework the theme helpers to support:
  - canvas
  - line chrome
  - compact cards
  - chips
  - focused rows

### Step 2. Rebuild the Header
- Replace the current header composition.
- Introduce a left product block and right dashboard cluster.
- Add metric cards and a persistent activity strip.
- Ensure all activity labels are truthful.

### Step 3. Rebuild the Sidebar
- Replace paragraph styling with grouped labeled rows.
- Make path handling concise and readable.
- Keep commands visible but better organized.

### Step 4. Rebuild the Results Workspace
- Reduce the column count.
- Add stronger row-level treatment.
- Make the empty state minimal and intentional.
- Keep selection handling intact.

### Step 5. Rebuild the Inspector
- Convert the inspector to stacked labeled sections.
- Emphasize outcome and clearly-labeled run total.
- Keep source/target readable with trimmed paths.

### Step 6. Rebalance Log and Footer
- Keep the log full-width.
- Simplify the footer into essential key hints only.

### Step 7. Regression Protection
- Preserve existing behavior tests for:
  - config editor
  - quit blocking
  - confirmation overlay
  - row warning chips
- Add or update view tests for:
  - sidebar grouping
  - empty state copy
  - inspector labels
  - row chip semantics where practical

## Risks
- Narrow terminal widths may still constrain the header/dashboard composition.
- Ratatui title and block rendering can easily drift back into a boxed look if not kept disciplined.
- Without real engine progress events, the activity strip must remain intentionally modest.
- A more ambitious visual layout can accidentally reduce readability if too many accents or chips are introduced.

## Out of Scope
- Engine progress streaming changes
- New scan/link business logic
- Additional actions or workflows
- Search/filter/sort redesign
- Non-TUI CLI changes

## Verification Plan
Because this will change production Rust code, the affected checks should be:

```bash
cargo fmt
cargo check
cargo test tui::
```

Expected review focus after implementation:
- no fabricated progress claims
- clearer hierarchy than the current version
- reduced boxiness / reduced filled slab appearance
- structured inspector
- dominant results workspace
- visible but calmer sidebar and footer

## Approval Trigger
Once this plan is accepted, the next implementation response should:
1. restate the exact files to touch,
2. implement the redesign,
3. run the TUI-scoped verification commands,
4. report results.
