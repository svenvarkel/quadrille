use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::{self, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use quadrille::{Result, Sheet, parse_sort};
use serde_json::{Value, json};

const HELP: &str = r#"Quadrille — CSV, XLSX and ODS cell editor for humans and agents

Usage: qd [OPTIONS] FILE

Without headless options, opens the terminal UI.

  -d, --delimiter CHAR  Separator (default: comma; 'tab' for TSV)
      --sheets          List workbook sheet names as JSON
      --sheet NAME      Open a workbook sheet (default: first sheet)
      --check           Scan the file; emit JSON counts, timing and index size
      --read A1:C10     Read a rectangle as JSON (at most 100,000 cells / 10,000 rows)
      --set CELL VALUE  Change one cell; repeat for multiple edits
      --apply FILE      Apply a JSON array of {"cell":"B7","value":"text"} edits
      --sort B,-D:n     Sort B ascending as text, D descending numerically
      --no-header       Include the first record in sorting (default: keep it)
      --dry-run         Preview edits/sort as JSON without writing a file
  -o, --output FILE     Save to FILE; the input itself may be replaced
  -h, --help            Show this help

Examples:
  qd large.csv
  qd large.csv --read A1:D20
  qd large.csv --set B7 '00123' --dry-run
  qd large.csv --apply changes.json --output corrected.csv

Rows are 1-based CSV records, including any header; columns are A, B, ..., AA.
Values are strings. Missing fields in ragged records read as null.
Edits require --dry-run or --output. --read combined with edits shows the result.
JSON goes to stdout; errors go to stderr with a nonzero exit status.

TUI: ? / h / F1 help, s / F6 sort, click select, double-click edit, wheel scroll.
Arrows / PgUp / PgDn move, Home / End first / last column,
Ctrl+Home / Ctrl+End first / last indexed row, Ctrl+G go to row,
Enter / F2 edit, Ctrl+Z undo, Ctrl+S / F4 save as, +/- column width,
q / Ctrl+Q quit. In an editor: Ctrl+A select all, Ctrl+J insert newline.

Workbooks: --sheet selects one sheet; --sheets lists them. Values are read as
text. In XLSX, edits beginning with = create formulas; existing formulas are
read-only, with cached results only. Unused XLSX columns can be edited.
Native saves preserve other sheets/styles and require source row order.
Use a .csv destination to export the selected sheet, including a sorted view.

UTF-8 CSV: without sorting, unedited records retain their bytes. Edited
records may be requoted. Sorted exports use LF endings and skip blank lines.
--check checks readability and UTF-8, not strict CSV syntax.
"#;

type Address = (u64, usize);

#[derive(Clone, Copy)]
struct Range {
    first: Address,
    last: Address,
}

fn address(text: &str) -> Result<Address> {
    let split = text
        .find(|c: char| c.is_ascii_digit())
        .ok_or("Use a cell address such as B7")?;
    let (letters, digits) = text.split_at(split);
    if letters.is_empty()
        || !letters.bytes().all(|b| b.is_ascii_alphabetic())
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("Use a cell address such as B7".into());
    }
    let mut column = 0usize;
    for letter in letters.bytes() {
        column = column
            .checked_mul(26)
            .and_then(|c| c.checked_add((letter.to_ascii_uppercase() - b'A' + 1) as usize))
            .ok_or("Column address is too large")?;
    }
    let row = digits
        .parse::<u64>()?
        .checked_sub(1)
        .ok_or("Rows start at 1")?;
    Ok((row, column - 1))
}

fn range(text: &str) -> Result<Range> {
    let (start, end) = text.split_once(':').unwrap_or((text, text));
    let (first, last) = (address(start)?, address(end)?);
    if first.0 > last.0 || first.1 > last.1 {
        return Err("Range must run from top-left to bottom-right".into());
    }
    let rows = last.0 - first.0 + 1;
    let columns = last.1 - first.1 + 1;
    if rows > 10_000
        || (rows as usize)
            .checked_mul(columns)
            .is_none_or(|n| n > 100_000)
    {
        return Err("Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks".into());
    }
    Ok(Range { first, last })
}

fn next_text(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| format!("Missing value for {option}"))?
        .into_string()
        .map_err(|_| format!("{option} requires UTF-8 text").into())
}

/// Execute a headless command, or return the same engine for the TUI.
pub fn open() -> Result<Option<Sheet>> {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.peek().is_none() {
        println!("{HELP}\n{}", super::PLATFORM_HELP);
        return Ok(None);
    }
    let mut path = None;
    let mut delimiter = b',';
    let mut selected_sheet = None;
    let mut list_sheets = false;
    let mut check = false;
    let mut read = None;
    let mut edits = Vec::new();
    let mut edits_requested = false;
    let mut output = None;
    let mut dry_run = false;
    let mut sort = None;
    let mut header = true;
    let mut positional = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") if !positional => positional = true,
            Some("-h" | "--help") if !positional => {
                println!("{HELP}\n{}", super::PLATFORM_HELP);
                return Ok(None);
            }
            Some("--check") if !positional => check = true,
            Some("--sheets") if !positional => list_sheets = true,
            Some("--sheet") if !positional => {
                selected_sheet = Some(next_text(&mut args, "--sheet")?)
            }
            Some("--sort") if !positional => sort = Some(next_text(&mut args, "--sort")?),
            Some("--no-header") if !positional => header = false,
            Some("--dry-run") if !positional => dry_run = true,
            Some("--read") if !positional => read = Some(range(&next_text(&mut args, "--read")?)?),
            Some("--set") if !positional => {
                edits_requested = true;
                let cell = address(&next_text(&mut args, "--set CELL")?)?;
                edits.push((cell, next_text(&mut args, "--set VALUE")?));
            }
            Some("--apply") if !positional => {
                edits_requested = true;
                let path = args.next().ok_or("Missing JSON patch filename")?;
                let patch: Value = serde_json::from_reader(File::open(path)?)?;
                for edit in patch
                    .as_array()
                    .ok_or("Patch must be an array of {cell, value} objects")?
                {
                    let object = edit
                        .as_object()
                        .ok_or("Each patch entry must be an object")?;
                    if object.len() != 2 {
                        return Err("Each patch entry must contain only cell and value".into());
                    }
                    let cell = edit
                        .get("cell")
                        .and_then(Value::as_str)
                        .ok_or("Patch cell must be an A1-style string")?;
                    let value = edit
                        .get("value")
                        .and_then(Value::as_str)
                        .ok_or("Patch value must be a string; no automatic type conversion")?;
                    edits.push((address(cell)?, value.to_owned()));
                }
            }
            Some("-o" | "--output") if !positional => {
                output = Some(PathBuf::from(args.next().ok_or("Missing output filename")?))
            }
            Some("-d" | "--delimiter") if !positional => {
                let value = next_text(&mut args, "--delimiter")?;
                delimiter = if value == "tab" || value == "\\t" {
                    b'\t'
                } else if value.len() == 1 {
                    value.as_bytes()[0]
                } else {
                    return Err("Delimiter must be one ASCII character or 'tab'".into());
                };
            }
            Some(value) if !positional && value.starts_with('-') => {
                return Err(format!("Unknown option: {value}").into());
            }
            _ => {
                if path.replace(PathBuf::from(arg)).is_some() {
                    return Err("Open one file at a time".into());
                }
            }
        }
    }
    if list_sheets {
        if check
            || read.is_some()
            || edits_requested
            || dry_run
            || output.is_some()
            || sort.is_some()
            || selected_sheet.is_some()
            || !header
            || delimiter != b','
        {
            return Err("Use --sheets by itself with a workbook filename".into());
        }
        let path = path.ok_or("Missing workbook filename")?;
        emit(&json!({"source": path, "sheets": Sheet::sheet_names(&path)?}))?;
        return Ok(None);
    }
    if edits_requested && !dry_run && output.is_none() {
        return Err("Edits require --dry-run or --output NEW_FILE".into());
    }
    if !header && sort.is_none() {
        return Err("--no-header requires --sort".into());
    }
    if sort.is_some() && read.is_none() && !dry_run && output.is_none() {
        return Err(
            "Use --sort with --read, --dry-run or --output; use F6 to sort in the TUI".into(),
        );
    }
    if let Some(spec) = &sort {
        parse_sort(spec)?;
    }
    if dry_run && output.is_some() {
        return Err("Choose --dry-run or --output, not both".into());
    }
    if check && (read.is_some() || edits_requested || dry_run || output.is_some() || sort.is_some())
    {
        return Err("--check cannot be combined with read/edit/save options".into());
    }
    let started = Instant::now();
    let mut sheet = Sheet::open_sheet(
        &path.ok_or("Missing filename")?,
        delimiter,
        selected_sheet.as_deref(),
    )?;
    let headless = check || read.is_some() || !edits.is_empty() || dry_run || output.is_some();
    if !headless {
        return Ok(Some(sheet));
    }
    // Reads can finish as soon as the requested rows are indexed; edits/saves
    // wait for the complete scan so an encoding error cannot publish a partial result.
    loop {
        let p = sheet.progress();
        if let Some(error) = p.error {
            return Err(error.into());
        }
        if p.done
            || (read.is_some_and(|r| p.rows > r.last.0)
                && edits.is_empty()
                && output.is_none()
                && !check
                && !dry_run
                && sort.is_none())
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let p = sheet.progress();
    if check {
        let mut result = json!({"records": p.rows, "bytes": p.total_bytes, "index_bytes": p.index_bytes, "elapsed_seconds": started.elapsed().as_secs_f64()});
        if let Some(name) = sheet.sheet_name() {
            result["sheet"] = json!(name);
            result["format"] = json!(sheet.format());
            result["source_bytes"] = json!(std::fs::metadata(&sheet.path)?.len());
        }
        emit(&result)?;
        return Ok(None);
    }
    if read.is_some_and(|r| r.last.0 >= p.rows) {
        return Err("Requested range extends beyond the last row".into());
    }
    let mut changes = BTreeMap::new();
    for ((row, col), value) in edits {
        let records = sheet.window(row, 1)?;
        let original = records.first().and_then(|r| r.get(col)).unwrap_or("");
        changes
            .entry((row, col))
            .or_insert_with(|| (original.to_owned(), String::new()))
            .1 = value.clone();
        sheet.set(row, col, value)?;
    }
    let changes: Vec<Value> = changes.into_iter().filter(|(_, (before, after))| before != after)
        .map(|((row, col), (before, after))| json!({"cell": format!("{}{}", super::column_name(col), row + 1), "before": before, "after": after})).collect();
    let mut result =
        json!({"source": sheet.path.to_string_lossy(), "changes": changes, "dry_run": dry_run});
    if let Some(name) = sheet.sheet_name() {
        result["sheet"] = json!(name);
        result["format"] = json!(sheet.format());
        result["formula_results"] = json!(
            "existing results are cached; new formulas are calculated when the saved file opens"
        );
        result["edit_type"] = json!("text; XLSX values beginning with = are formulas");
    }
    if let Some(spec) = &sort {
        let job = sheet.start_sort(parse_sort(spec)?, header)?;
        let order = job.result.recv()??;
        sheet.apply_sort(order)?;
        result["sort"] = json!({"columns": spec, "header": header, "changes_coordinates": "source", "read_coordinates": "sorted_view"});
    }
    if let Some(range) = read {
        let records = sheet.window(range.first.0, (range.last.0 - range.first.0 + 1) as usize)?;
        let rows: Vec<Vec<Value>> = records
            .iter()
            .enumerate()
            .map(|(i, row)| {
                (range.first.1..=range.last.1)
                    .map(|col| {
                        sheet
                            .cell_value(range.first.0 + i as u64, col, row)
                            .map(|value| json!(value))
                            .unwrap_or(Value::Null)
                    })
                    .collect()
            })
            .collect();
        result["rows"] = json!(rows);
        if sheet.sheet_name().is_some() {
            let mut formulas = serde_json::Map::new();
            for row in range.first.0..=range.last.0 {
                for col in range.first.1..=range.last.1 {
                    if let Some(formula) = sheet.formula(row, col) {
                        formulas.insert(
                            format!("{}{}", super::column_name(col), row + 1),
                            json!(formula),
                        );
                    }
                }
            }
            result["formulas"] = Value::Object(formulas);
        }
        result["range"] = json!(format!(
            "{}{}:{}{}",
            super::column_name(range.first.1),
            range.first.0 + 1,
            super::column_name(range.last.1),
            range.last.0 + 1
        ));
    }
    if let Some(output) = output {
        let saved = sheet.save_as(&output)?.result.recv()??;
        result["output"] = json!(saved.to_string_lossy());
    }
    emit(&result)?;
    Ok(None)
}

fn emit(value: &Value) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_bounded_ranges() {
        assert_eq!(address("A1").unwrap(), (0, 0));
        assert_eq!(address("aa65").unwrap(), (64, 26));
        for bad in [
            "",
            "A0",
            "1",
            "A-1",
            "A1x",
            "🦀1",
            "A18446744073709551616",
            "AAAAAAAAAAAAAAAAAAAA1",
        ] {
            assert!(address(bad).is_err(), "{bad}");
        }
        for bad in ["B2:A1", "A1:A10001", "A1:ZZZ100", "A1:B2:C3"] {
            assert!(range(bad).is_err(), "{bad}");
        }
        assert_eq!(range("C3").unwrap().first, (2, 2));
    }
}
