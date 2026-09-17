# Quadrille

A CSV cell editor for agents and humans, with a JSON CLI and a terminal UI. The first working version is deliberately
small: browse, sort, edit, undo, and save a copy without loading the entire file into RAM.

## Run

Requires Rust 1.85 or newer to build. Only the TUI needs an interactive terminal.

```sh
cargo build --release
./target/release/qd path/to/large.csv
```

For the local test dataset:

```sh
./target/release/qd test-data/Stablewood_PMAP_HMDA_All_Data_20250415.csv
```

Use `-d ';'` for semicolon-separated files or `-d tab` for TSV. Comma is the default;
there is no delimiter or encoding autodetection.

For a non-interactive scan with JSON record count, elapsed time, and sparse-index size:

```sh
./target/release/qd --check path/to/large.csv
```

## Agent CLI

```sh
# Read a range; every existing field is a JSON string, including numbers.
./target/release/qd large.csv --read A1:D20

# Preview a change without creating an output file.
./target/release/qd large.csv --set B7 '00123' --dry-run

# Apply a batch, then publish a new file.
./target/release/qd large.csv --apply changes.json --output corrected.csv
```

`changes.json` is an array of explicit cell edits:

```json
[
  {"cell": "B7", "value": "00123"},
  {"cell": "C9", "value": "A multiline\nvalue"}
]
```

`--set CELL VALUE` can also be repeated. Edits require `--dry-run` or `--output`.
The JSON result includes the changed cells with their original and final values;
no-op edits are omitted. Adding `--read` returns the selected rectangle **after**
applying the edits. Missing fields in uneven records are represented as `null`;
writing a nonexistent cell fails. Out-of-file ranges fail rather than truncate.
Reads are limited to 100,000 cells and 10,000 records per call; request larger
results in chunks. Read-only calls can finish before full-file indexing completes.

All edits are checked before an output file is published. JSON goes to stdout,
diagnostics to stderr, and failures return a nonzero exit status. `--output`
without edits creates a byte-identical copy. Batch edits live in memory; this
version is intended for targeted corrections, not millions of per-cell patches.

## Controls

| Key | Action |
| --- | --- |
| ? / h / F1 | Open command help; Esc closes it |
| s / F6 | Sort dialog; click a column heading to preselect it |
| Arrow keys, Tab / Shift+Tab | Move between cells |
| PageUp / PageDown | Move one screen |
| Home / End | First / last column in the current record |
| Ctrl+Home / Ctrl+End | First / last indexed record |
| Ctrl+G | Go to a record number; wait for indexing if necessary |
| Enter / F2 | Edit the selected cell |
| Ctrl+Z | Undo the last cell edit |
| Ctrl+S / F4 | Save to a **new** filename |
| + / - | Widen / narrow columns |
| q / Ctrl+Q / Ctrl+C / F10 | Quit; confirm if there are unsaved changes |

Help automatically includes Mac keyboard equivalents when running on macOS:
Fn+Left / Right for Home / End, Fn+Up / Down for PageUp / PageDown, and
Ctrl+Fn+Left / Right for the first / last indexed record. Ctrl means Control (⌃).
Function keys may require Fn, depending on keyboard settings. Terminal shortcuts
can intercept keys. Detection uses the OS running `qd`; over SSH, that is the
remote host, not the keyboard's platform.

Mouse capture is enabled while the editor is open: click or drag to select a cell,
double-click to edit, wheel to scroll vertically, and Shift+wheel or a horizontal
wheel to move across columns. The bottom function-key bar is clickable. Mouse
reporting must be supported by your terminal; capture is disabled on exit.

In the editor, typing replaces the initially selected value. Use Left / Right to
move the cursor and keep the existing text, Ctrl+A to select all, Ctrl+J to insert
a newline, Enter to apply, and Esc to cancel. Bracketed paste supports multiline
cell values. Control characters are shown as visible symbols in the grid.

Columns are labeled A, B, …, AA. Row 1 is the first CSV record, including the header
if the file has one. Blank physical lines are skipped by the CSV reader, and a
quoted multiline field belongs to one record. Yellow cells have edits; cyan marks
the selected cell. The selected value is also previewed below the grid.

## Sorting

Press **F6** (or `s`) and enter columns in priority order:

- `B` — column B, ascending text order.
- `-B` — column B, descending text order.
- `B:n` — column B, ascending numeric order (`2` before `10`).
- `B,-D:n` — B ascending as text, then D descending numerically.

The first record stays in place as the header by default; **F2 inside the sort
dialog** toggles this. Enter `clear` in the dialog to restore source order while
keeping cell edits. **Esc** requests cancellation during a background sort. Cell
editing and saving wait for the sort to finish; navigation and help remain usable.

Text ordering is case-sensitive Unicode order. Numeric mode accepts exact plain
decimals within a 96-bit coefficient / 28-digit scale; invalid or out-of-range
numbers fail the sort rather than rounding them. Empty and missing keys sort last
in either direction. Equal keys retain original source order. Sorting uses current
cell edits, but subsequent edits do not automatically re-sort the view.

The same operation is available to agents:

```sh
./target/release/qd large.csv --sort 'B,-D:n' --read A1:F20
./target/release/qd large.csv --sort 'B:n' --output sorted.csv
```

Use `--no-header` to include the first record in sorting. When combined with edits,
`--set` / `--apply` addresses always refer to **source** coordinates and are applied
before sorting; `--read` addresses refer to the **sorted view**. The JSON result
states this explicitly. In the TUI, edits address the visible cell and stay attached
to its source record when the view is re-sorted or cleared.

Sorting retains keys and record locations with an estimated **512 MiB key-building
budget**. The full CSV is never loaded. Exceeding the budget leaves the previous
view intact. This is not an overall process RSS cap: record buffers, edits, allocator
overhead, and the current/resulting row orders also consume memory. Disk-backed
sorting is not implemented yet. After sorting, the retained order uses 24 bytes per
record; cancelling during comparison takes effect after that comparison phase.

**Save As writes the visible sorted order.** Sorted output uses LF record endings,
removes skipped blank lines, and retains an initial UTF-8 BOM. Unedited field bytes
are copied; edited records may be requoted. Clear the sort before saving if you
want the original order and original record-ending preservation.

## What this version supports

- UTF-8 CSV, with or without a BOM, including quoted delimiters, escaped quotes,
  multiline fields, LF / CRLF record endings, and uneven record lengths.
- All values remain text. Leading zeros and date-like strings are not converted.
- Background indexing, with the first records available before the scan finishes.
  Each index entry stores the byte offset of every 64th CSV record.
- Only the visible records, edits, undo history, and sparse index are retained.
  Memory therefore depends on record size, visible rows, edits, and record count;
  it is **not** constant. No memory mapping is used in this version.
- Background save with progress. Navigation stays available; further edits and
  quitting wait until the save finishes.

## Save behavior and limits

Saves always create a new file. The default is `original.edited.csv` beside the
source. Existing destinations, including the source itself, are refused. Output is
written to a temporary file in the destination directory, flushed and synced, then
published without overwriting an existing path. Detected source changes or write
errors prevent publication.

When no sort is active, unedited records are copied as raw bytes. **Edited records are reserialized**:
field values and record endings are preserved, but optional quoting within those
records may change. This is not yet byte-exact preservation of unedited fields
inside an edited record. With no sort active, an unchanged save or a save after undoing all edits
produces a byte-identical copy.

After saving, the session still views the original file plus its edits; it does not
switch to the new file. Further saves need another unused filename.

The source must remain unchanged while open. Length, modification time, and (on
Unix) file identity are checked, but there is no filesystem snapshot or exclusive
lock. Save is enabled only after the entire file has been scanned successfully.
Unsupported encodings produce an error rather than being silently transcoded.
The underlying `csv` parser is permissive about malformed quoting: `--check` checks
readability and UTF-8, **not** strict RFC 4180 conformance. Use well-formed CSV files
for this proof of concept.

This version has no formulas, filtering, row insertion/deletion, workbook
formats, or MCP server. The CLI and TUI use the same editing engine, but separate
processes do not share a live editing session. Very large individual
records still require proportional memory, and large pasted edits/undo history
are held in memory.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
# After cargo build --release, on macOS/Linux:
python3 tests/tui_smoke.py
```

Tests cover multiline and quoted fields, UTF-8, BOM and line endings, sparse seeks,
copy/edit/undo round trips, source-change detection, refusal to overwrite existing
files, CLI patches and dry runs, sorting with stable edit identity, mouse navigation,
and terminal input/rendering. The original exploration is in
[the technical and business analysis](docs/quadrille-analysis.md); its broader
roadmap and preliminary performance estimates are not shipped capabilities.

## Prior art

- [csvlens](https://github.com/YS-L/csvlens) — large CSV viewing
- [VisiData](https://www.visidata.org/) — tabular exploration and editing
- [sc-im](https://github.com/andmarti1424/sc-im) — terminal spreadsheet
- [IronCalc](https://github.com/ironcalc/IronCalc) — spreadsheet engine
- [l123](https://github.com/duane1024/l123) — Lotus-style terminal spreadsheet

## License

MIT OR Apache-2.0. Copyright © 2026 Wasabi OÜ.
