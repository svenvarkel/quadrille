//! Golden CLI behaviour: exact stdout JSON, stderr text and exit status.
mod support;

use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};

const CSV: &str = "id,name,note\r\n00123,Tallinn,\"line 1\nline 2\"\r\n00456,Tartu\r\n";

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_qd"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    Run {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// A tempdir holding data.csv, data.tsv, book.xlsx, book.ods and patch files.
fn fixtures() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::write(root.join("data.csv"), CSV).unwrap();
    fs::write(root.join("data.tsv"), "a\tb\n1\t2\n").unwrap();
    fs::write(root.join("semi.csv"), "a;b\n1;2\n").unwrap();
    fs::write(root.join("bad.csv"), b"a,b\n\xff,c\n").unwrap();
    fs::write(root.join("exists.csv"), "keep\n").unwrap();
    for (name, text) in [
        (
            "patch.json",
            r#"[{"cell":"B2","value":"patched"},{"cell":"a3","value":"00000"}]"#,
        ),
        ("empty.json", "[]"),
        ("broken.json", "[{"),
        ("object.json", "{}"),
        ("number.json", "[1]"),
        ("one.json", r#"[{"cell":"A1"}]"#),
        ("three.json", r#"[{"cell":"A1","value":"x","extra":1}]"#),
        ("cellnum.json", r#"[{"cell":1,"value":"x"}]"#),
        ("other.json", r#"[{"a":"A1","b":"x"}]"#),
        ("valuenum.json", r#"[{"cell":"A1","value":1}]"#),
        ("row0.json", r#"[{"cell":"A0","value":"x"}]"#),
        (
            "order.json",
            r#"[{"cell":"A0","value":"x"},{"cell":"B1","value":1}]"#,
        ),
    ] {
        fs::write(root.join(name), text).unwrap();
    }
    support::xlsx(&root);
    support::ods(&root);
    (dir, root)
}

fn src(root: &Path, name: &str) -> String {
    root.join(name).to_string_lossy().into_owned()
}

fn document(root: &Path, args: &[&str]) -> Value {
    let result = run(root, args);
    assert_eq!((result.code, result.stderr.as_str()), (0, ""), "{args:?}");
    assert!(result.stdout.ends_with('\n') && result.stdout.lines().count() == 1);
    serde_json::from_str(&result.stdout).unwrap()
}

/// `--check` output with its timing checked and removed.
fn checked(root: &Path, args: &[&str]) -> Value {
    let mut value = document(root, args);
    let elapsed = value
        .as_object_mut()
        .unwrap()
        .remove("elapsed_seconds")
        .unwrap();
    assert!(elapsed.as_f64().unwrap() >= 0.0);
    value
}

#[test]
fn golden_check_and_sheets() {
    let (_dir, root) = fixtures();
    let bytes = |name: &str| fs::metadata(root.join(name)).unwrap().len();
    assert_eq!(
        checked(&root, &["data.csv", "--check"]),
        json!({"records": 3, "bytes": CSV.len(), "index_bytes": 8})
    );
    let xlsx = checked(&root, &["--check", "book.xlsx"]);
    assert_eq!(
        xlsx.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "bytes",
            "format",
            "index_bytes",
            "records",
            "sheet",
            "source_bytes"
        ]
    );
    assert_eq!(
        (&xlsx["records"], &xlsx["sheet"], &xlsx["format"]),
        (&json!(5), &json!("Data"), &json!("XLSX"))
    );
    assert_eq!(xlsx["source_bytes"], bytes("book.xlsx"));
    let notes = checked(&root, &["book.ods", "--sheet", "Notes õ", "--check"]);
    assert_eq!(
        (&notes["records"], &notes["sheet"], &notes["format"]),
        (&json!(3), &json!("Notes õ"), &json!("ODS"))
    );
    assert_eq!(notes["source_bytes"], bytes("book.ods"));
    // --sheets reports the path as given, not canonicalized.
    assert_eq!(
        document(&root, &["--sheets", "book.xlsx"]),
        json!({"source": "book.xlsx", "sheets": ["Data", "Notes õ"]})
    );
    assert_eq!(
        document(&root, &["./book.ods", "--sheets"]),
        json!({"source": "./book.ods", "sheets": ["Data", "Notes õ"]})
    );
}

#[test]
fn golden_read_edit_sort_save() {
    let (_dir, root) = fixtures();
    let csv = src(&root, "data.csv");
    assert_eq!(
        document(&root, &["data.csv", "--read", "A2:C3"]),
        json!({
            "source": csv, "changes": [], "dry_run": false, "range": "A2:C3",
            "rows": [["00123", "Tallinn", "line 1\nline 2"], ["00456", "Tartu", null]]
        })
    );
    assert_eq!(
        document(&root, &["data.csv", "--read", "c3"]),
        json!({"source": csv, "changes": [], "dry_run": false, "range": "C3:C3", "rows": [[null]]})
    );
    assert_eq!(
        document(&root, &["data.csv", "--read", "B1:B1", "--dry-run"]),
        json!({"source": csv, "changes": [], "dry_run": true, "range": "B1:B1", "rows": [["name"]]})
    );
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--set",
                "B2",
                "changed",
                "--read",
                "B2",
                "--dry-run"
            ]
        ),
        json!({
            "source": csv, "dry_run": true, "range": "B2:B2", "rows": [["changed"]],
            "changes": [{"cell": "B2", "before": "Tallinn", "after": "changed"}]
        })
    );
    // Changes are sorted by cell, deduplicated, and unchanged values are omitted.
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--set",
                "C2",
                "c",
                "--set",
                "A3",
                "x",
                "--set",
                "A3",
                "y",
                "--set",
                "A2",
                "00123",
                "--dry-run"
            ]
        ),
        json!({
            "source": csv, "dry_run": true,
            "changes": [
                {"cell": "C2", "before": "line 1\nline 2", "after": "c"},
                {"cell": "A3", "before": "00456", "after": "y"}
            ]
        })
    );
    assert_eq!(
        document(&root, &["data.csv", "--apply", "empty.json", "--dry-run"]),
        json!({"source": csv, "changes": [], "dry_run": true})
    );
    assert_eq!(
        document(&root, &["data.csv", "--dry-run"]),
        json!({"source": csv, "changes": [], "dry_run": true})
    );
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--apply",
                "patch.json",
                "--output",
                "patched.csv"
            ]
        ),
        json!({
            "source": csv, "dry_run": false, "output": "patched.csv",
            "changes": [
                {"cell": "B2", "before": "Tallinn", "after": "patched"},
                {"cell": "A3", "before": "00456", "after": "00000"}
            ]
        })
    );
    assert_eq!(
        fs::read_to_string(root.join("patched.csv")).unwrap(),
        "id,name,note\r\n00123,patched,\"line 1\nline 2\"\r\n00000,Tartu\r\n"
    );
    assert_eq!(
        document(&root, &["data.csv", "-o", "copy.csv"]),
        json!({"source": csv, "changes": [], "dry_run": false, "output": "copy.csv"})
    );
    assert_eq!(fs::read_to_string(root.join("copy.csv")).unwrap(), CSV);
    let sort = |header: bool| json!({"columns": "B,-A", "header": header, "changes_coordinates": "source", "read_coordinates": "sorted_view"});
    assert_eq!(
        document(
            &root,
            &["data.csv", "--sort", "B,-A", "--read", "A1:B3", "--dry-run"]
        ),
        json!({
            "source": csv, "changes": [], "dry_run": true, "range": "A1:B3", "sort": sort(true),
            "rows": [["id", "name"], ["00123", "Tallinn"], ["00456", "Tartu"]]
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--sort",
                "B,-A",
                "--no-header",
                "--read",
                "A1:B3"
            ]
        ),
        json!({
            "source": csv, "changes": [], "dry_run": false, "range": "A1:B3", "sort": sort(false),
            "rows": [["00123", "Tallinn"], ["00456", "Tartu"], ["id", "name"]]
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--set",
                "B3",
                "Aa",
                "--sort",
                "B,-A",
                "--output",
                "sorted.csv"
            ]
        ),
        json!({
            "source": csv, "dry_run": false, "output": "sorted.csv", "sort": sort(true),
            "changes": [{"cell": "B3", "before": "Tartu", "after": "Aa"}]
        })
    );
    assert_eq!(
        fs::read_to_string(root.join("sorted.csv")).unwrap(),
        "id,name,note\n00456,Aa\n00123,Tallinn,\"line 1\nline 2\"\n"
    );
    // The source itself may be replaced.
    fs::write(root.join("inplace.csv"), "a\n1\n").unwrap();
    assert_eq!(
        document(
            &root,
            &["inplace.csv", "--set", "A2", "2", "--output", "inplace.csv"]
        ),
        json!({
            "source": src(&root, "inplace.csv"), "dry_run": false,
            "output": src(&root, "inplace.csv"),
            "changes": [{"cell": "A2", "before": "1", "after": "2"}]
        })
    );
}

#[test]
fn golden_delimiters_and_positional_arguments() {
    let (_dir, root) = fixtures();
    let rows = |name: &str, args: &[&str]| {
        let mut all = vec![name];
        all.extend(args);
        let value = document(&root, &all);
        assert_eq!(value["source"], src(&root, name));
        value["rows"].clone()
    };
    for args in [["-d", "tab"], ["--delimiter", "\\t"], ["-d", "\t"]] {
        assert_eq!(
            rows("data.tsv", &[args[0], args[1], "--read", "A1:B2"]),
            json!([["a", "b"], ["1", "2"]])
        );
    }
    assert_eq!(
        rows("semi.csv", &["--delimiter", ";", "--read", "B2"]),
        json!([["2"]])
    );
    // Without a delimiter option the whole line is one field.
    assert_eq!(
        rows("data.tsv", &["--read", "A1:B1"]),
        json!([["a\tb", null]])
    );
    // After --, option-like arguments are file names.
    fs::write(root.join("-dash.csv"), "x\n").unwrap();
    let value = document(&root, &["--read", "A1", "--", "-dash.csv"]);
    assert_eq!(value["source"], src(&root, "-dash.csv"));
    assert_eq!(value["rows"], json!([["x"]]));
}

#[test]
fn golden_workbooks() {
    let (_dir, root) = fixtures();
    let xlsx = src(&root, "book.xlsx");
    let notes =
        json!("existing results are cached; new formulas are calculated when the saved file opens");
    let edit_type = json!("text; XLSX values beginning with = are formulas");
    assert_eq!(
        document(&root, &["book.xlsx", "--read", "C2:E2"]),
        json!({
            "source": xlsx, "sheet": "Data", "format": "XLSX", "changes": [], "dry_run": false,
            "formula_results": notes, "edit_type": edit_type, "range": "C2:E2",
            "rows": [["Tallinn", "12", ""]],
            "formulas": {"D2": "SUM(B2:B3)", "E2": "1+1"}
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "book.xlsx",
                "--set",
                "G2",
                "=1+1",
                "--set",
                "b2",
                "11",
                "--read",
                "B2:G2",
                "--dry-run"
            ]
        ),
        json!({
            "source": xlsx, "sheet": "Data", "format": "XLSX", "dry_run": true,
            "formula_results": notes, "edit_type": edit_type, "range": "B2:G2",
            "rows": [["11", "Tallinn", "12", "", "", "=1+1"]],
            "formulas": {"D2": "SUM(B2:B3)", "E2": "1+1", "G2": "1+1"},
            "changes": [
                {"cell": "B2", "before": "10", "after": "11"},
                {"cell": "G2", "before": "", "after": "=1+1"}
            ]
        })
    );
    assert_eq!(
        document(
            &root,
            &["book.ods", "--sheet", "Notes õ", "--read", "A3:B3"]
        ),
        json!({
            "source": src(&root, "book.ods"), "sheet": "Notes õ", "format": "ODS",
            "changes": [], "dry_run": false, "formula_results": notes, "edit_type": edit_type,
            "range": "A3:B3", "rows": [["", "Tallinn notes"]], "formulas": {}
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "book.ods",
                "--sort",
                "B:n",
                "--read",
                "B1:B3",
                "--output",
                "sorted.csv"
            ]
        ),
        json!({
            "source": src(&root, "book.ods"), "sheet": "Data", "format": "ODS",
            "changes": [], "dry_run": false, "formula_results": notes, "edit_type": edit_type,
            "range": "B1:B3", "rows": [["amount"], ["2"], ["10"]], "formulas": {},
            "output": "sorted.csv",
            "sort": {"columns": "B:n", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"}
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "book.xlsx",
                "--find",
                "Tallinn",
                "--columns",
                "C",
                "--sort",
                "-A"
            ]
        ),
        json!({
            "source": xlsx, "sheet": "Data", "format": "XLSX", "coordinates": "sorted_view",
            "find": {"text": "Tallinn", "columns": ["C"], "exact": false, "ignore_case": false, "from": "A1", "limit": 100},
            "matches": [{"cell": "C3", "value": "Tallinn"}], "next": null, "records_scanned": 5,
            "sort": {"columns": "-A", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"}
        })
    );
}

#[test]
fn golden_find() {
    let (_dir, root) = fixtures();
    let csv = src(&root, "data.csv");
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--set",
                "B3",
                "Tallinn",
                "--sort",
                "A",
                "--no-header",
                "--find",
                "tallinn",
                "--ignore-case",
                "--exact",
                "--from",
                "B1",
                "--limit",
                "1",
                "--dry-run"
            ]
        ),
        json!({
            "source": csv, "coordinates": "sorted_view", "dry_run": true,
            "find": {"text": "tallinn", "columns": null, "exact": true, "ignore_case": true, "from": "B1", "limit": 1},
            "matches": [{"cell": "B1", "value": "Tallinn"}], "next": "C1", "records_scanned": 1,
            "changes": [{"cell": "B3", "before": "Tartu", "after": "Tallinn"}],
            "sort": {"columns": "A", "header": false, "changes_coordinates": "source", "read_coordinates": "sorted_view"}
        })
    );
    assert_eq!(
        document(&root, &["data.csv", "--find", "x", "--dry-run"]),
        json!({
            "source": csv, "coordinates": "source", "dry_run": true, "changes": [],
            "find": {"text": "x", "columns": null, "exact": false, "ignore_case": false, "from": "A1", "limit": 100},
            "matches": [], "next": null, "records_scanned": 3
        })
    );
    assert_eq!(
        document(
            &root,
            &[
                "data.csv",
                "--find",
                "0",
                "--columns",
                "c,A,a",
                "--from",
                "a3"
            ]
        ),
        json!({
            "source": csv, "coordinates": "source",
            "find": {"text": "0", "columns": ["A", "C"], "exact": false, "ignore_case": false, "from": "A3", "limit": 100},
            "matches": [{"cell": "A3", "value": "00456"}], "next": null, "records_scanned": 1
        })
    );
}

#[test]
fn golden_help() {
    let (_dir, root) = fixtures();
    let help = run(&root, &[] as &[&str]);
    assert_eq!((help.code, help.stderr.as_str()), (0, ""));
    assert!(
        help.stdout.starts_with(include_str!("golden/help.txt")),
        "{}",
        help.stdout
    );
    let platform = &help.stdout[include_str!("golden/help.txt").len()..];
    assert!(platform.contains("Ctrl means Control") && platform.ends_with(".\n"));
    for args in [
        &["-h"][..],
        &["--help"],
        &["data.csv", "--read", "A1", "-h"],
        &["--help", "--bogus"],
    ] {
        let other = run(&root, args);
        assert_eq!(
            (other.code, &other.stdout, other.stderr.as_str()),
            (0, &help.stdout, ""),
            "{args:?}"
        );
    }
}

#[test]
fn golden_rejections() {
    let (_dir, root) = fixtures();
    let cases: &[(&[&str], &str)] = &[
        // Argument errors are reported in argument order.
        (&["--check"], "Missing filename"),
        (&["a.csv", "b.csv"], "Open one file at a time"),
        (&["data.csv", "--bogus"], "Unknown option: --bogus"),
        (&["data.csv", "-x", "--read", "A0"], "Unknown option: -x"),
        (&["data.csv", "--read", "A0", "--bogus"], "Rows start at 1"),
        (&["data.csv", "--read", "A0", "--help"], "Rows start at 1"),
        (&["data.csv", "--read"], "Missing value for --read"),
        (&["data.csv", "--sheet"], "Missing value for --sheet"),
        (&["data.csv", "--sort"], "Missing value for --sort"),
        (&["data.csv", "-d"], "Missing value for --delimiter"),
        (&["data.csv", "--set"], "Missing value for --set CELL"),
        (
            &["data.csv", "--set", "B2"],
            "Missing value for --set VALUE",
        ),
        (
            &["data.csv", "--set", "2B", "x"],
            "Use a cell address such as B7",
        ),
        (&["data.csv", "--apply"], "Missing JSON patch filename"),
        (&["data.csv", "-o"], "Missing output filename"),
        (&["data.csv", "--output"], "Missing output filename"),
        (
            &["data.csv", "-d", "ab"],
            "Delimiter must be one ASCII character or 'tab'",
        ),
        (
            &["data.csv", "-d", "õ"],
            "Delimiter must be one ASCII character or 'tab'",
        ),
        (
            &["data.csv", "-d", "", "--check"],
            "Delimiter must be one ASCII character or 'tab'",
        ),
        (
            &["data.csv", "-d", "\"", "--check"],
            "Delimiter must be a single ASCII byte other than a quote, CR, LF or NUL",
        ),
        // Cell and range syntax.
        (&["data.csv", "--read", ""], "Use a cell address such as B7"),
        (
            &["data.csv", "--read", "1"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "B"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "A1x"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "A-1"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "🦀1"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "$A$1"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "A1:"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "A1:B2:C3"],
            "Use a cell address such as B7",
        ),
        (
            &["data.csv", "--read", "A18446744073709551616"],
            "number too large to fit in target type",
        ),
        (
            &["data.csv", "--read", "AAAAAAAAAAAAAAAAAAAA1"],
            "Column address is too large",
        ),
        (
            &["data.csv", "--read", "B2:A1"],
            "Range must run from top-left to bottom-right",
        ),
        (
            &["data.csv", "--read", "B1:A2"],
            "Range must run from top-left to bottom-right",
        ),
        (
            &["data.csv", "--read", "A1:A10001"],
            "Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks",
        ),
        (
            &["data.csv", "--read", "A1:ZZZ100"],
            "Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks",
        ),
        (
            &["data.csv", "--read", "A1:XFD10000"],
            "Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks",
        ),
        (
            &["data.csv", "--read", "A4"],
            "Requested range extends beyond the last row",
        ),
        (
            &["data.csv", "--read", "A2:A4"],
            "Requested range extends beyond the last row",
        ),
        // Patches.
        (
            &["data.csv", "--apply", "missing.json"],
            "No such file or directory (os error 2)",
        ),
        (
            &["data.csv", "--apply", "broken.json"],
            "EOF while parsing an object at line 1 column 2",
        ),
        (
            &["data.csv", "--apply", "object.json"],
            "Patch must be an array of {cell, value} objects",
        ),
        (
            &["data.csv", "--apply", "number.json"],
            "Each patch entry must be an object",
        ),
        (
            &["data.csv", "--apply", "one.json"],
            "Each patch entry must contain only cell and value",
        ),
        (
            &["data.csv", "--apply", "three.json"],
            "Each patch entry must contain only cell and value",
        ),
        (
            &["data.csv", "--apply", "cellnum.json"],
            "Patch cell must be an A1-style string",
        ),
        (
            &["data.csv", "--apply", "other.json"],
            "Patch cell must be an A1-style string",
        ),
        (
            &["data.csv", "--apply", "valuenum.json"],
            "Patch value must be a string; no automatic type conversion",
        ),
        (&["data.csv", "--apply", "row0.json"], "Rows start at 1"),
        (&["data.csv", "--apply", "order.json"], "Rows start at 1"),
        (
            &["data.csv", "--apply", "empty.json"],
            "Edits require --dry-run or --output NEW_FILE",
        ),
        (
            &["data.csv", "--apply", "patch.json", "--bogus"],
            "Unknown option: --bogus",
        ),
        // Combinations.
        (
            &["data.csv", "--set", "B2", "x"],
            "Edits require --dry-run or --output NEW_FILE",
        ),
        (
            &["data.csv", "--no-header", "--dry-run"],
            "--no-header requires --sort",
        ),
        (
            &["data.csv", "--sort", "A"],
            "Use --sort with --read, --find, --dry-run or --output; use F6 to sort in the TUI",
        ),
        (
            &["data.csv", "--sort", "A,A", "--dry-run"],
            "A sort column may only appear once",
        ),
        (
            &["data.csv", "--sort", "1", "--dry-run"],
            "Use column letters, commas, - for descending and :n for numbers; e.g. B,-D:n",
        ),
        (
            &["data.csv", "--sort", "", "--dry-run"],
            "Use column letters, commas, - for descending and :n for numbers; e.g. B,-D:n",
        ),
        (
            &[
                "data.csv",
                "--sort",
                "AAAAAAAAAAAAAAAAAAAAAAAA",
                "--dry-run",
            ],
            "Column is too large",
        ),
        (
            &["data.csv", "--sort", "D", "--dry-run"],
            "Sort column 4 is outside the header",
        ),
        (
            &["data.csv", "--dry-run", "-o", "new.csv"],
            "Choose --dry-run or --output, not both",
        ),
        (
            &["data.csv", "--check", "--dry-run"],
            "--check cannot be combined with read/edit/save options",
        ),
        (
            &["data.csv", "--check", "--read", "A1"],
            "--check cannot be combined with read/edit/save options",
        ),
        (
            &["data.csv", "--check", "-o", "new.csv"],
            "--check cannot be combined with read/edit/save options",
        ),
        (
            &["data.csv", "--check", "--sort", "A", "--read", "A1"],
            "--check cannot be combined with read/edit/save options",
        ),
        (
            &["data.csv", "--sheets"],
            "--sheets requires an XLSX or ODS workbook",
        ),
        (&["--sheets"], "Missing workbook filename"),
        (
            &["book.xlsx", "--sheets", "--check"],
            "Use --sheets by itself with a workbook filename",
        ),
        (
            &["book.xlsx", "--sheets", "--sheet", "Data"],
            "Use --sheets by itself with a workbook filename",
        ),
        (
            &["book.xlsx", "--sheets", "-d", ";"],
            "Use --sheets by itself with a workbook filename",
        ),
        (
            &["book.xlsx", "--sheets", "--no-header"],
            "Use --sheets by itself with a workbook filename",
        ),
        (
            &["book.xlsx", "--sheets", "--apply", "empty.json"],
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
            &["book.xlsx", "--sheets", "--read", "A1"],
            "Use --sheets by itself with a workbook filename",
        ),
        // Find rules precede --sheets and edit rules.
        (
            &["book.xlsx", "--sheets", "--exact"],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["data.csv", "--find", "a", "--read", "A1", "--sheets"],
            "--find cannot be combined with --read, --output or --check",
        ),
        // Opening. Without headless options the TUI needs a terminal.
        (
            &["data.csv"],
            "The editor needs a terminal. Use --read, --apply or --check for headless commands",
        ),
        (
            &["book.ods", "--sheet", "Notes õ"],
            "The editor needs a terminal. Use --read, --apply or --check for headless commands",
        ),
        (&["missing.csv"], "No such file or directory (os error 2)"),
        (
            &["missing.csv", "--check"],
            "No such file or directory (os error 2)",
        ),
        (
            &["data.csv", "--sheet", "Data", "--check"],
            "--sheet applies to XLSX and ODS workbooks only",
        ),
        (
            &["book.xlsx", "-d", ";", "--check"],
            "--delimiter applies to CSV files only",
        ),
        (
            &["book.xlsx", "--sheet", "missing", "--read", "A1"],
            "No sheet named \"missing\"; use --sheets",
        ),
        (
            &["bad.csv", "--check"],
            "CSV parse error: record 1 (line 2, field: 0, byte: 4): invalid utf-8: invalid UTF-8 in field 0 near byte index 0",
        ),
        (
            &["bad.csv", "--read", "A2"],
            "CSV parse error: record 1 (line 2, field: 0, byte: 4): invalid utf-8: invalid UTF-8 in field 0 near byte index 0",
        ),
        // Edits and saves.
        (
            &["data.csv", "--set", "D3", "x", "--dry-run"],
            "No cell at this position",
        ),
        (
            &["data.csv", "-o", "exists.csv"],
            "Destination already exists; choose a new filename",
        ),
        (
            &["data.csv", "-o", "new.xlsx"],
            "CSV to workbook conversion is not supported; use a CSV destination",
        ),
        (
            &["book.xlsx", "--set", "D2", "1", "--dry-run"],
            "Formula and merged/array cells are read-only in this version",
        ),
        (
            &["book.xlsx", "--set", "XFE1", "x", "--dry-run"],
            "No cell at this position",
        ),
        (
            &["book.xlsx", "-o", "x.ods"],
            "Save as .xlsx to preserve the workbook, or .csv to export the selected sheet",
        ),
        (
            &["book.xlsx", "--sort", "A", "-o", "x.xlsx"],
            "Native workbook saves require source row order: clear the sort, or export the sorted view as .csv",
        ),
    ];
    for &(args, message) in cases {
        let result = run(&root, args);
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr.as_str()),
            (1, "", format!("qd: {message}\n").as_str()),
            "{args:?}"
        );
    }
    // Every line of the existing find rejection table, with its full text.
    for (args, message) in [
        (
            &["--exact"][..],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["--ignore-case"],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["--columns", "A"],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["--from", "A1"],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["--limit", "5"],
            "--exact, --ignore-case, --columns, --from and --limit require --find",
        ),
        (
            &["--find", ""],
            "--find needs text; use --exact --find '' to find empty cells",
        ),
        (&["--find"], "Missing value for --find"),
        (&["--find", "a", "--columns"], "Missing value for --columns"),
        (&["--find", "a", "--from"], "Missing value for --from"),
        (&["--find", "a", "--limit"], "Missing value for --limit"),
        (
            &["--find", "a", "--read", "A1"],
            "--find cannot be combined with --read, --output or --check",
        ),
        (
            &["--find", "a", "--output", "out.csv"],
            "--find cannot be combined with --read, --output or --check",
        ),
        (
            &["--find", "a", "--check"],
            "--find cannot be combined with --read, --output or --check",
        ),
        (
            &["--find", "a", "--set", "A1", "x"],
            "Edits require --dry-run or --output NEW_FILE",
        ),
        (
            &["--find", "a", "--limit", "0"],
            "--limit must be a whole number from 1 to 10,000",
        ),
        (
            &["--find", "a", "--limit", "10001"],
            "--limit must be a whole number from 1 to 10,000",
        ),
        (
            &["--find", "a", "--limit", "-1"],
            "--limit must be a whole number from 1 to 10,000",
        ),
        (
            &["--find", "a", "--limit", "many"],
            "--limit must be a whole number from 1 to 10,000",
        ),
        (
            &["--find", "a", "--columns", ""],
            "Use column letters such as B, not \"\"",
        ),
        (
            &["--find", "a", "--columns", "A,,B"],
            "Use column letters such as B, not \"\"",
        ),
        (
            &["--find", "a", "--columns", "A, B"],
            "Use column letters such as B, not \" B\"",
        ),
        (
            &["--find", "a", "--columns", "A,"],
            "Use column letters such as B, not \"\"",
        ),
        (
            &["--find", "a", "--columns", "B1"],
            "Use column letters such as B, not \"B1\"",
        ),
        (
            &["--find", "a", "--columns", "ZZZZZZZZZZZZZZZZZZZZZZZZ"],
            "Column address is too large",
        ),
        (&["--find", "a", "--from", "A0"], "Rows start at 1"),
        (
            &["--find", "a", "--from", "ZZZZZZZZZZZZZZZZZZZZZZZZ1"],
            "Column address is too large",
        ),
        (
            &["--find", "a", "--from", "B"],
            "Use a cell address such as B7",
        ),
        (
            &["--find", "a", "--no-header"],
            "--no-header requires --sort",
        ),
        (
            &["--find", "a", "--sort", "A,A"],
            "A sort column may only appear once",
        ),
        (
            &["--find", "a", "--sheets"],
            "Use --sheets by itself with a workbook filename",
        ),
    ] {
        let mut all = vec!["data.csv"];
        all.extend(args);
        let result = run(&root, &all);
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr.as_str()),
            (1, "", format!("qd: {message}\n").as_str()),
            "{all:?}"
        );
    }
}

#[test]
fn golden_non_utf8_arguments() {
    let (_dir, root) = fixtures();
    let bad = OsStr::from_bytes(b"\xff");
    for option in [
        "--sheet",
        "--sort",
        "--find",
        "--columns",
        "--from",
        "--limit",
        "--read",
        "--delimiter",
    ] {
        let result = run(&root, &[OsStr::new("data.csv"), OsStr::new(option), bad]);
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr),
            (1, "", format!("qd: {option} requires UTF-8 text\n")),
        );
    }
    for (args, message) in [
        (vec!["data.csv", "--set"], "--set CELL requires UTF-8 text"),
        (
            vec!["data.csv", "--set", "B2"],
            "--set VALUE requires UTF-8 text",
        ),
    ] {
        let mut all: Vec<&OsStr> = args.into_iter().map(OsStr::new).collect();
        all.push(bad);
        let result = run(&root, &all);
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr),
            (1, "", format!("qd: {message}\n")),
            "{all:?}"
        );
    }
    // Non-UTF-8 file names are passed through to the file system.
    let result = run(&root, &[OsStr::new("data.csv"), OsStr::new("--apply"), bad]);
    assert_eq!(
        (result.code, result.stderr.as_str()),
        (1, "qd: No such file or directory (os error 2)\n")
    );
}
