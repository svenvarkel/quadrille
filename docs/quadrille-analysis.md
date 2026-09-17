# Terminal Spreadsheet — Technical and Business Analysis

*Prepared on 16 September 2026. The project is now called **Quadrille** (`qd`).*

Measured, not assumed — but the niche is not empty, contrary to my earlier claim.

```
mmap + memchr row index, 1 core

1.00 GB CSV      →  12,000,001 rows
indexing        →  0.52 s   (1.94 GB/s cold)
warm cache      →  0.25 s   (4.02 GB/s)
sparse index    →  1.5 MB RAM, seek 1.46 µs
viewport parse  →  3.2 µs / 50 rows
```

---

## A1. Verdict

**Technically: green.** Opening a large file quickly is not the hard part — I measured it, and the algorithm is not the bottleneck. NVMe sequential read throughput is the limiting factor, rather than scanning speed.

**Commercially: red.** The realistic paying market is on the order of $100–300k *over the product's lifetime*, not per year. No terminal tool in this segment has monetized successfully.

**Strategically: yellow, but interesting.** The value is not in the TUI. It is in the *core* — an engine for editing large tabular files — and its customers are other programs and agents, not Vim users.

The most important correction to my earlier assessment: I said that an “NC-style cell editor that opens a 5 GB CSV instantly” did not exist. That is only half true. There are at least six terminal spreadsheets, and one project is already implementing *exactly* this retro TUI idea.

---

## A2. Competitive Landscape

Four distinct segments are commonly conflated. Quadrille sits between two of them.

### 1. Terminal spreadsheets with formulas

| Project | Language | Status | Notes |
|---|---|---|---|
| sc-im | C | Mature | De facto standard, Vim-style modal interface, partial xlsx support |
| TironCalc | Rust | Alpha | IronCalc's *official* TUI, ratatui. Saving was not available |
| l123 | Rust | Active | Lotus 1-2-3 R3.4a clone, 129★, 913-line specification |
| gridline | Rust | v0.2 | Rhai-scripted formulas, plain-text format |
| cell-sheet-tui | Rust | Released | Vim keybindings, `--read A1` / `--eval` batch mode |
| CacTui / TSHTS | Rust | Hobby projects | Excel clones with their own DSLs |

> **The most important finding.** **l123** is this idea, already in progress: Rust + ratatui + IronCalc, a DOS-era CUA menu, a three-line control panel, function-key controls, and a mode indicator. The specification is extraordinarily detailed and even includes a VisiData-style “Data Workbench” plugin behind Alt-F10. Its author has already done the architectural work you were still considering — including identifying that IronCalc lacks undo/redo and needs a command journal to provide it.

This does not mean the market is taken. It means the *aesthetic niche for a retro TUI spreadsheet* is taken, and competing there is a zero-sum game against someone who has already collected 129 stars. Differentiate elsewhere.

### 2. Large-file viewers

**csvlens** is the clear leader here and the real competitor. Tested with a 2.9 GB TSV containing 4.4 million records, it pages through the file like `less`, displaying the beginning of even enormous files immediately in the terminal. qsv uses csvlens as a library, lending it credibility for production use. Other players: tabiew, tidy-viewer, csview, VisiData.

The critical detail: **all of these are read-only.** They do not support editing. That is the gap.

### 3. GUI CSV editors — this is where the money is

| Product | Price | Large-file behavior |
|---|---|---|
| Modern CSV | ~$29–40 | Recommends read-only mode for files over 100 MB |
| EmEditor | $40–100 | 5 GB log in ~10 s; LibreOffice and Modern CSV failed on the same file |
| CSV Editor Pro | $29 | Windows only |
| Easy CSV Editor | $9.99 | Native macOS app |
| Tablecruncher | Free | 2 GB / 15 M rows; originally commercial, open-sourced in 2025 |

Conclusion: **willingness to pay for a CSV editor exists and is proven — $10–40 as a one-time purchase.** But that is in the GUI segment. Nobody has tested it in the terminal, and the reason is structural: terminal tool users are accustomed to everything being free and MIT-licensed.

### 4. SQL / analytics

duckdb, qsv, xsv, polars. These are not cell editors, but they *take over* many of the use cases for which someone would otherwise open a spreadsheet. This shrinks the market more than direct competitors do.

---

## A3. Where the Real Gap Is

```
                    terminal    editing    files > RAM
csvlens                ✓           ✗            ✓
sc-im / TironCalc       ✓           ✓            ✗
Modern CSV             ✗           ✓         partially
EmEditor               ✗           ✓            ✓   (Windows)
duckdb                 ✓           ✗            ✓   (not an editor)
───────────────────────────────────────────────────────────────
gap                    ✓           ✓            ✓
```

The positioning that follows is not “LibreOffice in the terminal.” It is:

> **An editor for precise cell-level changes to tabular files too large to fit in memory — one that never corrupts your data.**

The second half of that promise is an underrated selling point. Google Sheets and Excel both automatically reformat data during import — stripping leading zeros and converting text to dates — making them dangerous for CSV files that feed databases or APIs. A byte-preserving round trip is technically straightforward and a strong marketing proposition.

*Confidence: high that the gap exists; medium regarding its commercial value.*

---

## A4. Technical Validation

A benchmark, not speculation. One core, glibc `memchr` (the same AVX2 capability provided by Rust's `memchr` crate), a 1.00 GB CSV with 12 M rows.

| Metric | Result | Implication |
|---|---|---|
| Full index, cold(ish) | 0.518 s — 1.94 GB/s | 5 GB ≈ 2.6 s |
| Full index, warm | 0.250 s — 4.02 GB/s | The ceiling is disk throughput, not CPU |
| Full-index RAM | 96 MB | **Problem:** 5 GB → ~480 MB |
| Sparse index (every 64th row) | 1.5 MB | 64× less memory, 5 GB → ~7.5 MB |
| Random row seek with sparse index | 1.46 µs | Rescanning 63 rows is effectively free |
| Viewport parsing | 3.2 µs / 50 rows | ~300,000 frames/s. Not a concern. |

### What this settles

- **A sparse index is the right design, not an optimization.** A full index uses 64× the memory for no measurable benefit. A stride of 64 gives constant memory usage and seeks below 2 µs.
- **Index lazily in the background.** The first 10k rows appear instantly; the rest are indexed in a worker thread — opening feels O(1), even at 50 GB.
- **Parsing needs no optimization.** At 3.2 µs per frame, you can naively reparse the entire viewport on every keystroke. Do not build a cache you do not need.
- **mmap, not read().** Use `MADV_SEQUENTIAL` for indexing, then `MADV_RANDOM` afterward.

### Editing model

```rust
struct CsvSheet {
    map:      Mmap,                       // immutable source file
    index:    Vec<u64>,                   // sparse, stride 64
    overlay:  HashMap<(u64,u32), String>,  // modified cells only
    inserted: BTreeMap<u64, Vec<Row>>,    // inserted rows
    deleted:  RoaringBitmap,              // deleted rows
    journal:  Vec<Cmd>,                   // undo/redo
}
```

Reads: check the overlay → fall back to mmap. Memory scales with *changes*, not file size. Saving is a streaming rewrite with `fsync` + atomic `rename` — O(n) disk I/O and unavoidable, but incurred only when saving.

Use `RoaringBitmap` for deleted rows because “delete all rows where column X is empty” can produce millions of deletions, which would make a `HashSet<u64>` prohibitively large.

---

## A5. Technical Pitfalls

> **My benchmark is wrong.** I counted raw `\n` characters. RFC 4180 permits line breaks inside quoted fields — `"Tallinn,\nEesti"` is one field, not two rows. A naive `memchr` index silently breaks such files, a fatal flaw in a data tool. The scanner must track quoting state. Throughput is estimated to fall by a factor of 2–3, still leaving it above 1 GB/s — this does not kill the project, but it cannot be bolted on later.
>
> *Confidence: high regarding the problem (from the specification); low regarding the 2–3× estimate — measure it yourself.*

The remaining pitfalls, in order of importance:

1. **Trust is binary.** A CSV editor that corrupts a 5 GB file once is dead. Property-based tests, fuzzing, round-trip invariants, and *never* writing in place. Budget more time for this than for the UI.
2. **Column sizing requires scanning.** Finding the optimal width would require reading the entire column. Solution: sample 1,000 rows and allow a manual override.
3. **Unicode in the grid.** Grapheme clusters, double-width CJK characters, emoji, RTL text, ZWJ sequences. `unicode-width` + `unicode-segmentation` are mandatory, and terminal emulators behave inconsistently.
4. **Encodings.** Real-world CSV files use Windows-1252, CP1257 (Estonia!), and UTF-16LE with a BOM. Use `encoding_rs` + heuristic detection, and display the detected encoding in the status bar.
5. **Undo for operations on millions of rows.** The command journal must record the *operation*, not the modified cells.

---

## A6. TAM — Bottom Up

Spreadsheet software market reports give 2026 figures of $5 billion, $12.35 billion, $30.39 billion, and $281 billion. A 56× discrepancy. Useless — I am discarding them and doing the calculation myself.

### User base

| Segment | Count | Rationale |
|---|---|---|
| Professional developers | ~21–29 M | JetBrains: 20.8 M (2025); Statista: 28.7 M |
| Terminal-centric roles | ~8–10 M | Backend, DevOps, SRE, data engineering, sysadmin ≈ 35–40% |
| Regularly work with tabular data | ~4–5 M | ~50% of those |
| Would use a TUI spreadsheet | ~400–500 k | ~10% — the rest use duckdb/pandas/GUI tools |
| Would pay for it | ~2–5 k | 0.5–1% conversion, typical for an OSS developer tool |

### Revenue

```
SOM  =  450,000 × 0.8% × $35        ≈  $126,000   (cumulative, 3–5 years)
optimistic: 450,000 × 2% × $45      ≈  $405,000
pessimistic: essentially $0 — terminal users do not pay
```

For comparison: sc-im, csvlens, VisiData, lazygit, k9s, btop, ripgrep, fzf — none is a paid product. There is no direct precedent for a paid terminal spreadsheet. That does not prove it is impossible, but the burden of proof is on you.

> **The honest conclusion.** This is not a business. The best realistic outcome for a standalone product is “a good side income for one person over a few years,” and the most likely outcome is zero euros and 3,000 GitHub stars.

---

## A7. Monetization, Ranked

1. **Sell the core, not the TUI.** A large tabular file indexing and editing engine, distributed as a dual-licensed crate. csvlens has validated the pattern: being a library brought it qsv as a user and credibility for production use. Potential customers include ETL products, data quality SaaS platforms, and Electron editors. This is the only route where a single sale is worth four figures rather than two.
2. **An internal tool for SW.** The same core serves Stablewood's data pipelines — sanity-checking CSV files before loading them into PostgreSQL and inspecting RabbitMQ payloads. ROI is immediate and does not depend on any external users.
3. **Free OSS + reputation.** Measurable value for consulting and recruiting. Realistically, this is what will actually happen.
4. **Open core.** It does not work. A local file editor provides no server-side foothold on which to build a team or enterprise tier.
5. **Sponsorship.** The median is zero.

---

## A8. A Forward-Looking Angle

Everything above analyzes the market of 2015. In 2026, the more interesting question is different.

Agents run in the terminal. They generate, transform, and corrupt tabular data. There is currently no interface where an *agent manipulates* and a *human inspects* the same dataset within the same process. A person downloads a CSV, opens it in Excel, finds an error, and describes it to the agent in words. This is an absurd cycle.

A terminal spreadsheet is the right form for this if it provides all of the following:

- **Headless access** — `--read A1:C99`, `--set B7=…`, `--eval`. `cell-sheet-tui` already does this, confirming that the direction is right.
- **An MCP server in the same binary.** The agent gets tools such as `read_range`, `write_range`, `describe_columns`, and `find_anomalies` — while a person views the same file in an open TUI, with changes appearing in real time.
- **Built-in cell-level diffs.** Show exactly what the agent changed, cell by cell, before it is written to disk. A feature no spreadsheet in the world handles well.

This framing also changes the business logic: a “TUI spreadsheet” sells to Vim users. A “human oversight interface for agent-driven data changes” sells to companies running agents in production.

*Confidence: low to medium. This is a thesis, not a conclusion — but it is the only part of the analysis where I see a path beyond $1M.*

---

## A9. The Name

### Why “Excel” works

Three things at once: (1) a real English verb meaning “to be outstanding”; (2) it *flatters the user* — the name describes not the product, but who you become by using it; (3) two syllables, easy to inflect and build a brand around.

A common claim is that “exCEL” contains a hidden pun on spreadsheet cells. **This is probably a retrospective folk etymology** — Microsoft's own positioning was about “excellence” against Lotus.

### A CLI name has additional requirements

It is typed dozens of times a day: ≤5 characters, no Shift key, no hyphens, a unique tab-completion prefix. So **the product name and binary name can differ** — as with 1Password/`op` and ripgrep/`rg`.

### crates.io availability (checked)

| Name | crates.io | Assessment |
|---|---|---|
| **Quadrille** → `qd` | Available ✓ | Quadrille paper *is* squared graph paper. A real word, relevant to the domain, pronounceable. **Selected.** |
| Numerate → `num` | Available | Flatters the user just like Excel. The closest structural analogue. Long. |
| Reticle → `rt` | Available | An optical crosshair — grid + precision. Technical, sharp. |
| Abakus | Available | Estonian/German spelling, but the abacus is an overused metaphor. |
| Cels | Available | Four letters, but animation terminology and a weak brand. |
| Sumit | Available | sum + summit. Looks like a typo. |
| Foolscap | Available | A lovely word, but “fool” in a product built around trust in data? No. |
| Tally / Cellar / Folio / Quire / Tabula / Ledger / Pivot | Taken | Some are inactive name-squatting packages |
| `excel` | Available, **do not use it** | The crates.io name is available; Microsoft's Class 9 trademark is not |

**Selected: Quadrille, binary `qd`.** A bonus discovered later: quadrille derives from the Latin *quadra* (four), and in English, *quad-ruled* and *quadrille* are synonyms when referring to paper — `qd` points to another branch of the same root, rather than merely being an abbreviation.

Domain: `quadrille.dev` ($17/year). `.com` and `.io` are taken. `.dev` is on the HSTS preload list, which means mandatory TLS from day one. The schema URL lives on a separate subdomain: `schema.quadrille.dev/v1`.

---

## A10. Recommended Scope

Do not build a spreadsheet. Build a cell editor for large files and add spreadsheet functionality later, if at all.

| Phase | Scope | Effort |
|---|---|---|
| 0 | Quote-aware indexer + property-based tests. *Before the UI.* | 1–2 weeks |
| 1 | `Sheet` trait + `CsvSheet` (mmap, sparse index, overlay, journal, streaming save) | 2–3 weeks |
| 2 | Router + xlsx/ods backends + conversion | 2–3 weeks |
| 3 | Full CLI: grammar, JSON, dry-run, machine-readable help | 2–3 weeks |
| 4 | MCP server + live view | 1–2 weeks |
| 5–6 | TUI: grid, menu bar, dialogs, command palette | 6–9 weeks |

*Confidence: low. These are part-time estimates. Multiply by 1.5–2×.*

CLI and MCP deliberately come **before** the TUI. If the project stops after phase 4, a useful headless tool will still exist.

### Do not build

Charts. Pivot tables. Printing. WYSIWYG. Your own formula engine — a mistake l123 rightly avoids.

### Risks, ranked by likelihood

1. **csvlens adds editing.** The single biggest risk. It already leads in viewing, has library consumers, and adding an overlay editing layer is not difficult.
2. **A correctness bug destroys trust.** Mitigation: phase 0 before anything else.
3. **l123 takes all the oxygen in the retro TUI niche.** Mitigation: compete on file size, not aesthetics.
4. **The project grows to LibreOffice's size.** Mitigation: the `Sheet` trait keeps IronCalc as an optional plugin.
5. **Scope creeps into formulas.** Mitigation: put the “do not build” list in the README on day one.

---

*Benchmarks were run on one core using glibc `memchr`; Rust's `memchr` crate should deliver comparable results, but verify them on your own hardware. All market figures are bottom-up estimates — market reports in this category differed by 56×.*
