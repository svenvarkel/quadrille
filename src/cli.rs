use std::{
    ffi::OsString,
    fs::File,
    io::{self, Write},
    path::PathBuf,
    time::Instant,
};

use quadrille::{
    FindQuery, Result, Sheet,
    a1::{self, Address, Range},
    ops::{self, Edit, Wait},
    parse_sort,
};
use serde_json::Value;

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
      --find TEXT       Find cells containing TEXT (case-sensitive); emit their addresses
      --exact           With --find: the whole value must equal TEXT; '' finds empty cells
      --ignore-case     With --find: compare Unicode-lowercased text (not case folding)
      --columns B,D     With --find: search only these columns
      --from CELL       With --find: start at CELL, inclusive (default A1)
      --limit N         With --find: stop after N matches (default 100, maximum 10,000)
  -o, --output FILE     Save to FILE; the input itself may be replaced
  -h, --help            Show this help

Examples:
  qd large.csv
  qd large.csv --read A1:D20
  qd large.csv --set B7 '00123' --dry-run
  qd large.csv --find Tallinn --columns B --limit 20
  qd large.csv --apply changes.json --output corrected.csv

Rows are 1-based CSV records, including any header; columns are A, B, ..., AA.
Values are strings. Missing fields in ragged records read as null.
Edits require --dry-run or --output. --read combined with edits shows the result.
JSON goes to stdout; errors go to stderr with a nonzero exit status.

Find searches row by row the values --read would show (edits applied, cached
formula results), without waiting for indexing. With --sort, cells are sorted-view
coordinates. Pass "next" as --from for the next page; null means the end was
reached. --find cannot be combined with --read, --output or --check.

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

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    /// List the sheets of this workbook.
    Sheets(PathBuf),
    /// Open a file: the TUI, unless `Open::headless`.
    Open(Box<Open>),
}

#[derive(Debug, PartialEq)]
pub struct Open {
    pub path: PathBuf,
    pub delimiter: u8,
    pub sheet: Option<String>,
    pub check: bool,
    pub read: Option<Range>,
    pub edits: Vec<Edit>,
    /// Specification and whether the first record is a header that stays first.
    pub sort: Option<(String, bool)>,
    pub find: Option<Find>,
    pub dry_run: bool,
    pub output: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct Find {
    pub query: FindQuery,
    pub from: Option<Address>,
    pub limit: Option<usize>,
}

impl Open {
    fn headless(&self) -> bool {
        self.check
            || self.read.is_some()
            || self.find.is_some()
            || !self.edits.is_empty()
            || self.dry_run
            || self.output.is_some()
    }

    /// Reads can finish as soon as the requested rows are indexed; edits/saves
    /// wait for the complete scan so an encoding error cannot publish a partial result.
    /// Find validates what it scans itself; with edits or a sort it waits like them.
    fn wait(&self) -> Option<Wait> {
        if self.find.is_some() && self.edits.is_empty() && self.sort.is_none() {
            return None;
        }
        Some(match self.read {
            Some(range)
                if self.edits.is_empty()
                    && self.output.is_none()
                    && !self.check
                    && !self.dry_run
                    && self.sort.is_none() =>
            {
                Wait::Rows(range.last.0 + 1)
            }
            _ => Wait::All,
        })
    }
}

fn next_text(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| format!("Missing value for {option}"))?
        .into_string()
        .map_err(|_| format!("{option} requires UTF-8 text").into())
}

/// Parse and validate arguments (without the program name). Errors come in argument
/// order, then the combination rules in a fixed order. Only `--apply` reads a file.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command> {
    let mut args = args.into_iter().peekable();
    if args.peek().is_none() {
        return Ok(Command::Help);
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
    let mut find = None;
    let mut exact = false;
    let mut ignore_case = false;
    let mut find_columns = None;
    let mut from = None;
    let mut limit = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") if !positional => positional = true,
            Some("-h" | "--help") if !positional => return Ok(Command::Help),
            Some("--check") if !positional => check = true,
            Some("--sheets") if !positional => list_sheets = true,
            Some("--sheet") if !positional => {
                selected_sheet = Some(next_text(&mut args, "--sheet")?)
            }
            Some("--sort") if !positional => sort = Some(next_text(&mut args, "--sort")?),
            Some("--no-header") if !positional => header = false,
            Some("--dry-run") if !positional => dry_run = true,
            Some("--find") if !positional => find = Some(next_text(&mut args, "--find")?),
            Some("--exact") if !positional => exact = true,
            Some("--ignore-case") if !positional => ignore_case = true,
            Some("--columns") if !positional => {
                find_columns = Some(a1::parse_columns(&next_text(&mut args, "--columns")?)?)
            }
            Some("--from") if !positional => {
                from = Some(a1::parse_cell(&next_text(&mut args, "--from")?)?)
            }
            Some("--limit") if !positional => {
                let text = next_text(&mut args, "--limit")?;
                limit = Some(
                    text.parse::<usize>()
                        .ok()
                        .and_then(|n| ops::find_limit(Some(n)).ok())
                        .ok_or_else(|| format!("--{}", ops::FIND_LIMIT_ERROR))?,
                );
            }
            Some("--read") if !positional => {
                read = Some(ops::read_range(&next_text(&mut args, "--read")?)?)
            }
            Some("--set") if !positional => {
                edits_requested = true;
                let cell = a1::parse_cell(&next_text(&mut args, "--set CELL")?)?;
                edits.push((cell, next_text(&mut args, "--set VALUE")?));
            }
            Some("--apply") if !positional => {
                edits_requested = true;
                let path = args.next().ok_or("Missing JSON patch filename")?;
                let patch: Value = serde_json::from_reader(File::open(path)?)?;
                edits.extend(ops::parse_edits(&patch)?);
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
    if find.is_none()
        && (exact || ignore_case || find_columns.is_some() || from.is_some() || limit.is_some())
    {
        return Err("--exact, --ignore-case, --columns, --from and --limit require --find".into());
    }
    if find.as_deref() == Some("") && !exact {
        return Err("--find needs text; use --exact --find '' to find empty cells".into());
    }
    if find.is_some() && (read.is_some() || output.is_some() || check) {
        return Err("--find cannot be combined with --read, --output or --check".into());
    }
    if list_sheets {
        if check
            || find.is_some()
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
        return Ok(Command::Sheets(path.ok_or("Missing workbook filename")?));
    }
    if edits_requested && !dry_run && output.is_none() {
        return Err("Edits require --dry-run or --output NEW_FILE".into());
    }
    if !header && sort.is_none() {
        return Err("--no-header requires --sort".into());
    }
    if sort.is_some() && read.is_none() && find.is_none() && !dry_run && output.is_none() {
        return Err(
            "Use --sort with --read, --find, --dry-run or --output; use F6 to sort in the TUI"
                .into(),
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
    Ok(Command::Open(Box::new(Open {
        path: path.ok_or("Missing filename")?,
        delimiter,
        sheet: selected_sheet,
        check,
        read,
        edits,
        sort: sort.map(|spec| (spec, header)),
        find: find.map(|text| Find {
            query: FindQuery {
                text,
                columns: find_columns,
                exact,
                ignore_case,
            },
            from,
            limit,
        }),
        dry_run,
        output,
    })))
}

/// Execute a headless command, or return the opened sheet for the TUI.
pub fn run(command: Command) -> Result<Option<Sheet>> {
    let open = match command {
        Command::Help => {
            println!("{HELP}\n{}", super::PLATFORM_HELP);
            return Ok(None);
        }
        Command::Sheets(path) => {
            emit(&ops::sheets(&path)?.to_json())?;
            return Ok(None);
        }
        Command::Open(open) => *open,
    };
    let started = Instant::now();
    let mut sheet = Sheet::open_sheet(&open.path, open.delimiter, open.sheet.as_deref())?;
    if !open.headless() {
        return Ok(Some(sheet));
    }
    if let Some(wait) = open.wait() {
        ops::wait_for_index(&sheet, wait)?;
    }
    if open.check {
        emit(&ops::check(&sheet, started)?.to_json())?;
        return Ok(None);
    }
    if let Some(range) = &open.read {
        ops::in_bounds(&sheet, range)?;
    }
    let changes = ops::apply_edits(&mut sheet, open.edits)?;
    let sort = open
        .sort
        .map(|(spec, header)| ops::sort(&mut sheet, &spec, header))
        .transpose()?;
    if let Some(find) = open.find {
        let mut found = ops::find(&sheet, find.query, find.from, find.limit)?;
        found.sort = sort;
        found.preview = open.dry_run.then_some(changes);
        emit(&found.to_json())?;
        return Ok(None);
    }
    let read = open
        .read
        .map(|range| ops::read(&mut sheet, range))
        .transpose()?;
    let output = open
        .output
        .map(|path| ops::save(&sheet, &path))
        .transpose()?;
    let report = ops::Report {
        source: ops::Source::of(&sheet),
        changes,
        dry_run: open.dry_run,
        sort,
        read,
        output,
    };
    emit(&report.to_json())?;
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
        assert_eq!(a1::parse_cell("A1").unwrap(), (0, 0));
        assert_eq!(a1::parse_cell("aa65").unwrap(), (64, 26));
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
            assert!(a1::parse_cell(bad).is_err(), "{bad}");
        }
        for bad in ["B2:A1", "A1:A10001", "A1:ZZZ100", "A1:B2:C3"] {
            assert!(ops::read_range(bad).is_err(), "{bad}");
        }
        assert_eq!(ops::read_range("C3").unwrap().first, (2, 2));
    }

    fn parse_args(list: &[&str]) -> Result<Command> {
        parse(list.iter().map(OsString::from))
    }

    fn open(list: &[&str]) -> Open {
        match parse_args(list).unwrap() {
            Command::Open(open) => *open,
            other => panic!("{list:?}: {other:?}"),
        }
    }

    fn error(list: &[&str]) -> String {
        match parse_args(list) {
            Ok(command) => panic!("{list:?} parsed as {command:?}"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    #[should_panic(expected = "Help")]
    fn open_helper_refuses_other_commands() {
        open(&["--help"]);
    }

    #[test]
    #[should_panic(expected = "parsed as")]
    fn error_helper_refuses_success() {
        error(&["a.csv"]);
    }

    fn query(text: &str) -> FindQuery {
        FindQuery {
            text: text.into(),
            ..FindQuery::default()
        }
    }

    #[test]
    fn help_and_sheets() {
        for list in [
            &[][..],
            &["-h"],
            &["--help"],
            &["a.csv", "--read", "A1", "--help"],
            &["--help", "--bogus"],
        ] {
            assert_eq!(parse_args(list).unwrap(), Command::Help, "{list:?}");
        }
        assert_eq!(
            parse_args(&["--sheets", "book.xlsx"]).unwrap(),
            Command::Sheets("book.xlsx".into())
        );
        // The default delimiter, given explicitly, is no conflict.
        assert_eq!(
            parse_args(&["book.xlsx", "--sheets", "-d", ","]).unwrap(),
            Command::Sheets("book.xlsx".into())
        );
        assert_eq!(open(&["--", "--sheets"]).path, PathBuf::from("--sheets"));
    }

    #[test]
    fn every_option() {
        let plain = open(&["a.csv"]);
        assert_eq!(
            plain,
            Open {
                path: "a.csv".into(),
                delimiter: b',',
                sheet: None,
                check: false,
                read: None,
                edits: vec![],
                sort: None,
                find: None,
                dry_run: false,
                output: None,
            }
        );
        assert!(!plain.headless());
        let book = open(&["--sheet", "Notes õ", "book.xlsx"]);
        assert_eq!(book.sheet.as_deref(), Some("Notes õ"));
        assert!(!book.headless());
        assert!(open(&["a.csv", "--check"]).check);
        for (list, delimiter) in [
            (&["-d", "tab"][..], b'\t'),
            (&["--delimiter", "\\t"], b'\t'),
            (&["-d", "\t"], b'\t'),
            (&["-d", ";"], b';'),
            (&["-d", "\""], b'"'),
        ] {
            let mut all = vec!["a.csv"];
            all.extend(list);
            assert_eq!(open(&all).delimiter, delimiter, "{list:?}");
        }
        let edited = open(&[
            "a.csv",
            "--set",
            "b2",
            "x",
            "--read",
            "A1:C3",
            "--set",
            "B2",
            "",
            "--dry-run",
        ]);
        assert_eq!(
            edited.edits,
            [((1, 1), "x".into()), ((1, 1), String::new())]
        );
        assert_eq!(edited.read, Some(a1::parse_range("A1:C3").unwrap()));
        assert!(edited.dry_run);
        let saved = open(&["a.csv", "--sort", "B,-D:n", "--no-header", "-o", "out.csv"]);
        assert_eq!(saved.sort, Some(("B,-D:n".into(), false)));
        assert_eq!(saved.output, Some("out.csv".into()));
        assert_eq!(
            open(&["a.csv", "--sort", "A", "--output", "o.csv"]).sort,
            Some(("A".into(), true))
        );
        let found = open(&[
            "a.csv",
            "--find",
            "x",
            "--exact",
            "--ignore-case",
            "--columns",
            "c,b,B",
            "--from",
            "b2",
            "--limit",
            "10000",
        ]);
        assert_eq!(
            found.find,
            Some(Find {
                query: FindQuery {
                    text: "x".into(),
                    columns: Some(vec![1, 2]),
                    exact: true,
                    ignore_case: true,
                },
                from: Some((1, 1)),
                limit: Some(10_000),
            })
        );
        assert_eq!(
            open(&["a.csv", "--find", "", "--exact"])
                .find
                .unwrap()
                .query,
            FindQuery {
                exact: true,
                ..query("")
            }
        );
        assert_eq!(
            open(&["a.csv", "--find", "x", "--limit", "1"])
                .find
                .unwrap(),
            Find {
                query: query("x"),
                from: None,
                limit: Some(1),
            }
        );
        // After --, options are file names; before it, a lone - is still an option.
        assert_eq!(open(&["--", "--check"]).path, PathBuf::from("--check"));
        assert!(open(&["--check", "--", "-"]).check);
        assert_eq!(error(&["-", "--check"]), "Unknown option: -");
        assert_eq!(
            error(&["--", "a.csv", "--", "b.csv"]),
            "Open one file at a time"
        );
    }

    #[test]
    fn apply_reads_patch_files_in_argument_order() {
        let dir = tempfile::tempdir().unwrap();
        let patch = dir.path().join("patch.json");
        std::fs::write(
            &patch,
            r#"[{"cell":"B2","value":"x"},{"value":"y","cell":"a1"}]"#,
        )
        .unwrap();
        let patch = patch.to_str().unwrap();
        assert_eq!(
            open(&["a.csv", "--set", "C3", "z", "--apply", patch, "--dry-run"]).edits,
            [
                ((2, 2), "z".into()),
                ((1, 1), "x".into()),
                ((0, 0), "y".into())
            ]
        );
        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, "[]").unwrap();
        let empty = empty.to_str().unwrap();
        let open_empty = open(&["a.csv", "--apply", empty, "--dry-run"]);
        assert!(open_empty.edits.is_empty() && open_empty.headless());
        // An empty patch is still an edit request.
        assert_eq!(
            error(&["a.csv", "--apply", empty]),
            "Edits require --dry-run or --output NEW_FILE"
        );
        assert_eq!(
            error(&["a.csv", "--apply", empty, "--check", "--dry-run"]),
            "--check cannot be combined with read/edit/save options"
        );
        assert_eq!(
            error(&["a.csv", "--apply", empty, "--check", "-o", "x.csv"]),
            "--check cannot be combined with read/edit/save options"
        );
        assert_eq!(
            error(&["book.xlsx", "--sheets", "--apply", empty]),
            "Use --sheets by itself with a workbook filename"
        );
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "[{").unwrap();
        assert_eq!(
            error(&["a.csv", "--apply", broken.to_str().unwrap(), "--bogus"]),
            "EOF while parsing an object at line 1 column 2"
        );
        let missing = dir.path().join("missing.json");
        assert_eq!(
            error(&["a.csv", "--apply", missing.to_str().unwrap()]),
            "No such file or directory (os error 2)"
        );
        let invalid = dir.path().join("invalid.json");
        std::fs::write(&invalid, r#"[{"cell":"A0","value":"x"}]"#).unwrap();
        assert_eq!(
            error(&["a.csv", "--apply", invalid.to_str().unwrap()]),
            "Rows start at 1"
        );
    }

    #[test]
    fn every_rejection() {
        for (list, message) in [
            (&["--check"][..], "Missing filename"),
            (&["a.csv", "b.csv"], "Open one file at a time"),
            (&["a.csv", "--bogus"], "Unknown option: --bogus"),
            (&["a.csv", "-x", "--read", "A0"], "Unknown option: -x"),
            (&["a.csv", "--read", "A0", "--bogus"], "Rows start at 1"),
            (&["a.csv", "--read", "A0", "--help"], "Rows start at 1"),
            (&["a.csv", "--read"], "Missing value for --read"),
            (&["a.csv", "--sheet"], "Missing value for --sheet"),
            (&["a.csv", "--sort"], "Missing value for --sort"),
            (&["a.csv", "--find"], "Missing value for --find"),
            (&["a.csv", "--columns"], "Missing value for --columns"),
            (&["a.csv", "--from"], "Missing value for --from"),
            (&["a.csv", "--limit"], "Missing value for --limit"),
            (&["a.csv", "-d"], "Missing value for --delimiter"),
            (&["a.csv", "--delimiter"], "Missing value for --delimiter"),
            (&["a.csv", "--set"], "Missing value for --set CELL"),
            (&["a.csv", "--set", "B2"], "Missing value for --set VALUE"),
            (
                &["a.csv", "--set", "2B", "x"],
                "Use a cell address such as B7",
            ),
            (&["a.csv", "--apply"], "Missing JSON patch filename"),
            (&["a.csv", "-o"], "Missing output filename"),
            (&["a.csv", "--output"], "Missing output filename"),
            (
                &["a.csv", "-d", "ab"],
                "Delimiter must be one ASCII character or 'tab'",
            ),
            (
                &["a.csv", "-d", "õ"],
                "Delimiter must be one ASCII character or 'tab'",
            ),
            (
                &["a.csv", "-d", ""],
                "Delimiter must be one ASCII character or 'tab'",
            ),
            (&["a.csv", "--read", ""], "Use a cell address such as B7"),
            (
                &["a.csv", "--read", "$A$1"],
                "Use a cell address such as B7",
            ),
            (
                &["a.csv", "--read", "A1:B2:C3"],
                "Use a cell address such as B7",
            ),
            (
                &["a.csv", "--read", "A18446744073709551616"],
                "number too large to fit in target type",
            ),
            (
                &["a.csv", "--read", "AAAAAAAAAAAAAAAAAAAA1"],
                "Column address is too large",
            ),
            (
                &["a.csv", "--read", "B2:A1"],
                "Range must run from top-left to bottom-right",
            ),
            (
                &["a.csv", "--read", "A1:A10001"],
                "Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks",
            ),
            (
                &["a.csv", "--find", "a", "--limit", "0"],
                "--limit must be a whole number from 1 to 10,000",
            ),
            (
                &["a.csv", "--find", "a", "--limit", "10001"],
                "--limit must be a whole number from 1 to 10,000",
            ),
            (
                &["a.csv", "--find", "a", "--limit", "-1"],
                "--limit must be a whole number from 1 to 10,000",
            ),
            (
                &["a.csv", "--find", "a", "--limit", "many"],
                "--limit must be a whole number from 1 to 10,000",
            ),
            (
                &["a.csv", "--find", "a", "--columns", "A, B"],
                "Use column letters such as B, not \" B\"",
            ),
            (
                &[
                    "a.csv",
                    "--find",
                    "a",
                    "--columns",
                    "ZZZZZZZZZZZZZZZZZZZZZZZZ",
                ],
                "Column address is too large",
            ),
            (&["a.csv", "--find", "a", "--from", "A0"], "Rows start at 1"),
            (
                &["a.csv", "--find", "a", "--from", "B"],
                "Use a cell address such as B7",
            ),
            (
                &["a.csv", "--exact"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["a.csv", "--ignore-case"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["a.csv", "--columns", "A"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["a.csv", "--from", "A1"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["a.csv", "--limit", "5"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["book.xlsx", "--sheets", "--exact"],
                "--exact, --ignore-case, --columns, --from and --limit require --find",
            ),
            (
                &["a.csv", "--find", ""],
                "--find needs text; use --exact --find '' to find empty cells",
            ),
            (
                &["a.csv", "--find", "a", "--read", "A1"],
                "--find cannot be combined with --read, --output or --check",
            ),
            (
                &["a.csv", "--find", "a", "-o", "o.csv"],
                "--find cannot be combined with --read, --output or --check",
            ),
            (
                &["a.csv", "--find", "a", "--check"],
                "--find cannot be combined with --read, --output or --check",
            ),
            (
                &["a.csv", "--find", "a", "--read", "A1", "--sheets"],
                "--find cannot be combined with --read, --output or --check",
            ),
            (&["--sheets"], "Missing workbook filename"),
            (
                &["book.xlsx", "--sheets", "--check"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--find", "a"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--read", "A1"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--set", "A1", "x"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--dry-run"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "-o", "x.xlsx"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--sort", "A"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--sheet", "Data"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "--no-header"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["book.xlsx", "--sheets", "-d", ";"],
                "Use --sheets by itself with a workbook filename",
            ),
            (
                &["a.csv", "--set", "B2", "x"],
                "Edits require --dry-run or --output NEW_FILE",
            ),
            (
                &["a.csv", "--find", "a", "--set", "A1", "x"],
                "Edits require --dry-run or --output NEW_FILE",
            ),
            (
                &["a.csv", "--no-header", "--dry-run"],
                "--no-header requires --sort",
            ),
            (
                &["a.csv", "--find", "a", "--no-header"],
                "--no-header requires --sort",
            ),
            (
                &["a.csv", "--sort", "A"],
                "Use --sort with --read, --find, --dry-run or --output; use F6 to sort in the TUI",
            ),
            (
                &["a.csv", "--sort", "A", "--check"],
                "Use --sort with --read, --find, --dry-run or --output; use F6 to sort in the TUI",
            ),
            (
                &["a.csv", "--sort", "A,A", "--dry-run"],
                "A sort column may only appear once",
            ),
            (
                &["a.csv", "--find", "a", "--sort", "A,A"],
                "A sort column may only appear once",
            ),
            (
                &["a.csv", "--sort", "1", "--read", "A1"],
                "Use column letters, commas, - for descending and :n for numbers; e.g. B,-D:n",
            ),
            (
                &["a.csv", "--sort", "AAAAAAAAAAAAAAAAAAAAAAAA", "--dry-run"],
                "Column is too large",
            ),
            (
                &["a.csv", "--dry-run", "-o", "new.csv"],
                "Choose --dry-run or --output, not both",
            ),
            (
                &["a.csv", "--sort", "A,A", "--dry-run", "-o", "new.csv"],
                "A sort column may only appear once",
            ),
            (
                &["a.csv", "--check", "--dry-run"],
                "--check cannot be combined with read/edit/save options",
            ),
            (
                &["a.csv", "--check", "--read", "A1"],
                "--check cannot be combined with read/edit/save options",
            ),
            (
                &["a.csv", "--check", "-o", "new.csv"],
                "--check cannot be combined with read/edit/save options",
            ),
            (
                &["a.csv", "--check", "--set", "A1", "x", "-o", "n.csv"],
                "--check cannot be combined with read/edit/save options",
            ),
            (
                &["a.csv", "--check", "--sort", "A", "--read", "A1"],
                "--check cannot be combined with read/edit/save options",
            ),
        ] {
            assert_eq!(error(list), message, "{list:?}");
        }
    }

    #[test]
    fn non_utf8_values_are_rejected_but_file_names_are_not() {
        use std::os::unix::ffi::OsStringExt;
        let bad = || OsString::from_vec(vec![0xff]);
        for option in [
            "--sheet",
            "--sort",
            "--find",
            "--columns",
            "--from",
            "--limit",
            "--read",
            "-d",
            "--delimiter",
        ] {
            let list = [OsString::from("a.csv"), option.into(), bad()];
            let expected = if option == "-d" {
                "--delimiter"
            } else {
                option
            };
            assert_eq!(
                parse(list).unwrap_err().to_string(),
                format!("{expected} requires UTF-8 text")
            );
        }
        assert_eq!(
            parse([OsString::from("a.csv"), "--set".into(), bad()])
                .unwrap_err()
                .to_string(),
            "--set CELL requires UTF-8 text"
        );
        assert_eq!(
            parse([OsString::from("a.csv"), "--set".into(), "A1".into(), bad()])
                .unwrap_err()
                .to_string(),
            "--set VALUE requires UTF-8 text"
        );
        assert_eq!(
            parse([bad(), "-o".into(), bad()]).unwrap(),
            Command::Open(Box::new(Open {
                path: bad().into(),
                output: Some(bad().into()),
                ..open(&["a.csv"])
            }))
        );
    }

    #[test]
    fn index_waits_follow_the_operations() {
        let range = |text| Some(a1::parse_range(text).unwrap());
        let edit = vec![((0, 0), String::new())];
        let base = open(&["a.csv"]);
        let find = || {
            Some(Find {
                query: query("x"),
                from: None,
                limit: None,
            })
        };
        for (open, wait) in [
            (
                Open {
                    read: range("A1:B5"),
                    ..open(&["a.csv"])
                },
                Some(Wait::Rows(5)),
            ),
            (
                Open {
                    read: range("C10"),
                    ..open(&["a.csv"])
                },
                Some(Wait::Rows(10)),
            ),
            (
                Open {
                    read: range("A1"),
                    dry_run: true,
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    read: range("A1"),
                    edits: edit.clone(),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    read: range("A1"),
                    output: Some("o.csv".into()),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    read: range("A1"),
                    check: true,
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    read: range("A1"),
                    sort: Some(("A".into(), true)),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    check: true,
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    output: Some("o.csv".into()),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    find: find(),
                    ..open(&["a.csv"])
                },
                None,
            ),
            (
                Open {
                    find: find(),
                    dry_run: true,
                    ..open(&["a.csv"])
                },
                None,
            ),
            (
                Open {
                    find: find(),
                    edits: edit.clone(),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
            (
                Open {
                    find: find(),
                    sort: Some(("A".into(), true)),
                    ..open(&["a.csv"])
                },
                Some(Wait::All),
            ),
        ] {
            assert!(open.headless(), "{open:?}");
            assert_eq!(open.wait(), wait, "{open:?}");
        }
        assert!(!base.headless());
        assert!(
            Open {
                edits: edit,
                ..open(&["a.csv"])
            }
            .headless()
        );
        assert!(
            !Open {
                sort: Some(("A".into(), true)),
                ..base
            }
            .headless()
        );
    }
}
