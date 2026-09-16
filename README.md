# Quadrille

A terminal spreadsheet for files that are too big for a spreadsheet.

> **Status: pre-release.** `0.0.0` reserves the name. There is no working binary yet.
> The design is settled and benchmarked; the implementation is starting. If you found
> this looking for a tool to use today, come back later — or see [prior art](#prior-art)
> for things that work now.

---

## What it will do

Two distinct capabilities behind a format router, plus one architectural stance.

**1. Edit CSV files larger than RAM.** Memory-mapped, sparsely indexed, streaming.
Unbounded file size, constant memory, instant open. Edits are held in an overlay and
written out as a streaming rewrite with an atomic rename — the source file is never
modified in place.

**2. Open and edit xlsx, ods and xls workbooks.** Fully materialized in memory, bounded
by the formats' own ~1M row limit. Round-trip preserving: parts Quadrille does not model
are kept intact and written back unchanged.

**3. Everything is reachable from the CLI and from MCP**, not just the keyboard. The TUI,
the CLI and the MCP server are three thin dispatchers over one command core. Every
operation is a serializable value, which means undo, macro recording, the CLI grammar,
MCP tool calls and the audit log are all the same data structure.

The interface is CUA — menu bar, function keys, framed dialogs, a status line. Closer to
Norton Commander than to vim.

## Why it might be worth building

The interesting gap is not "a spreadsheet in the terminal" — several of those exist. It
is the intersection nobody covers:

|                      | terminal | edits | larger than RAM |
| -------------------- | :------: | :---: | :-------------: |
| csvlens              |    ✓     |   ✗   |        ✓        |
| sc-im, TironCalc     |    ✓     |   ✓   |        ✗        |
| Modern CSV           |    ✗     |   ✓   |     partial     |
| EmEditor             |    ✗     |   ✓   |    ✓ (Windows)  |
| duckdb               |    ✓     |   ✗   |  ✓ (not an editor) |
| **Quadrille**        |  **✓**   | **✓** |      **✓**      |

## Measured, not assumed

The core performance claim was benchmarked before any code was committed.
1.00 GB / 12,000,001 row CSV, one core:

| Metric                               | Result             |
| ------------------------------------ | ------------------ |
| Sparse newline index (stride 64)     | **0.21 s, 1.5 MB RAM** |
| Dense index, for comparison          | 0.52 s, 96 MB RAM  |
| Random row seek                      | **1.46 µs**        |
| Viewport parse, 50 rows              | **3.2 µs**         |
| Throughput, warm / cold-ish          | 4.02 / 1.94 GB/s   |

Scanning is not the bottleneck; sequential disk read is. A 5 GB file indexes in roughly
1–3 seconds and costs about 7.5 MB of resident memory.

One important caveat, stated up front because it is the project's biggest correctness
risk: those numbers are for a naive newline scan. RFC 4180 permits newlines inside
quoted fields, so the shipping scanner has to be quote-aware, which costs throughput.
Measuring that is the first milestone gate, not an optimization to revisit later.

## Correctness before speed

A CSV editor that corrupts a 5 GB file once is finished. Two promises, and the second
outranks the first:

1. A 5 GB CSV opens in under 2 seconds and edits without perceptible lag.
2. **No file is ever silently corrupted.** Byte-exact round-trip for anything Quadrille
   did not explicitly change. Leading zeros survive. A string that looks like a date
   stays a string. Quoted multi-line fields are handled correctly. Encodings —
   including Windows-1252 and CP1257 — are detected, displayed, and preserved.

Enforced by property tests (apply-then-undo returns the original bytes), fuzzing of the
scanner and dialect sniffer, and a corpus of real-world malformed files.

## Non-goals

Charts. Pivot tables. Printing. WYSIWYG. A formula evaluator. Collaborative editing. A
web UI. Plugins in v1. Writing `.xls`. Anything requiring a LibreOffice installation.

Formula evaluation, if it ever arrives, will be delegated to an existing engine. It will
never be reimplemented here.

## Roadmap

| Phase | Deliverable |
| ----- | ----------- |
| 0     | Quote-aware sparse indexer, command core, property tests, fuzz harness |
| 1     | CSV engine: overlay edits, sort, filter, streaming atomic save |
| 2     | Format router, xlsx/ods backends, constant-memory conversion |
| 3     | Complete CLI: grammar, JSON output, dry-run planning, machine-readable help |
| 4     | MCP server with plan/diff/commit and a live view into an attached TUI |
| 5–6   | TUI: grid, editing, menu bar, dialogs, command palette |

The CLI and MCP surfaces deliberately precede the TUI. If the project stalls after
phase 4, what exists is still a useful headless tool.

## Prior art

Worth your time today, and worth crediting:

- [csvlens](https://github.com/YS-L/csvlens) — streaming CSV viewer, the reference for
  large-file reading
- [sc-im](https://github.com/andmarti1424/sc-im) — the mature terminal spreadsheet
- [VisiData](https://www.visidata.org/) — data exploration across many formats
- [IronCalc](https://github.com/ironcalc/IronCalc) / TironCalc — modern Rust spreadsheet
  engine and its terminal skin
- [l123](https://github.com/duane1024/l123) — a Lotus 1-2-3 R3.4a TUI, and a genuinely
  impressive piece of specification work
- [qsv](https://github.com/dathere/qsv), [duckdb](https://duckdb.org/) — where to go when
  you want queries rather than cells

## Contributing

Not yet — the foundations are in flux. Issues and design discussion are welcome;
please open an issue before writing code.

## License

Licensed under either of Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or MIT license ([LICENSE-MIT](LICENSE-MIT)) at your
option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you shall be dual licensed as above, without any additional
terms or conditions.

Copyright © 2026 Wasabi OÜ
