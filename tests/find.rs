//! Find over XLSX and ODS packages built here; no binary fixtures.
mod support;

use quadrille::{FindQuery, FindResult, Sheet};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use support::{ods, xlsx};

fn open(path: &Path, sheet: Option<&str>) -> Sheet {
    let sheet = Sheet::open_sheet(path, b',', sheet).unwrap();
    let start = Instant::now();
    while !sheet.progress().done {
        assert!(start.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(1));
    }
    sheet
}

fn query(text: &str, exact: bool) -> FindQuery {
    FindQuery {
        text: text.into(),
        exact,
        ..FindQuery::default()
    }
}

fn cells(result: &FindResult) -> Vec<(u64, usize, &str)> {
    result
        .matches
        .iter()
        .map(|&((row, col), ref value)| (row, col, value.as_str()))
        .collect()
}

fn find(sheet: &Sheet, text: &str, exact: bool) -> Vec<(u64, usize, String)> {
    let result = sheet.find(&query(text, exact), (0, 0), 1000).unwrap();
    assert_eq!(result.next, None);
    cells(&result)
        .into_iter()
        .map(|(row, col, value)| (row, col, value.to_owned()))
        .collect()
}

fn paged(sheet: &Sheet, query: &FindQuery, expected: &FindResult) {
    for limit in 1..=expected.matches.len() + 1 {
        let (mut from, mut seen) = ((0, 0), Vec::new());
        loop {
            let page = sheet.find(query, from, limit).unwrap();
            assert_eq!(page.next.is_some(), page.matches.len() == limit);
            seen.extend(page.matches);
            match page.next {
                Some(next) => from = next,
                None => break,
            }
        }
        assert_eq!(seen, expected.matches, "limit {limit}");
    }
}

fn qd(source: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_qd"))
        .arg(source)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn workbook_values_formulas_and_blank_rectangle() {
    let dir = tempfile::tempdir().unwrap();
    for path in [xlsx(dir.path()), ods(dir.path())] {
        let mut sheet = open(&path, None);
        let name = sheet.format().to_owned();
        let s = |v: &str| v.to_owned();
        // Cached result present: its value is searched, never the formula text.
        assert_eq!(find(&sheet, "12", true), [(1, 3, s("12"))], "{name}");
        assert!(find(&sheet, "SUM", false).is_empty(), "{name}");
        assert!(find(&sheet, "1+1", false).is_empty(), "{name}");
        // Blanks inside A1:E5 are searched (including the uncached E2), none beyond it.
        let blanks = [
            (0, 4),
            (1, 4),
            (2, 2),
            (2, 3),
            (2, 4),
            (3, 0),
            (3, 1),
            (3, 2),
            (3, 3),
            (3, 4),
            (4, 0),
            (4, 1),
            (4, 3),
            (4, 4),
        ];
        let empty = find(&sheet, "", true);
        assert_eq!(
            empty,
            blanks.map(|(row, col)| (row, col, String::new())),
            "{name}"
        );
        let all = sheet.find(&query("", true), (0, 0), 1000).unwrap();
        paged(&sheet, &query("", true), &all);
        let rest = sheet.find(&query("", true), (4, 2), 1).unwrap();
        assert_eq!((cells(&rest), rest.next), (vec![(4, 3, "")], Some((4, 4))));
        assert_eq!(find(&sheet, "end", true), [(4, 2, s("end"))]);
        // Editing an input leaves the cached result stale, as --read shows it.
        sheet.set(1, 1, "100".into()).unwrap();
        assert_eq!(find(&sheet, "100", false), [(1, 1, s("100"))]);
        assert!(find(&sheet, "10", true).is_empty());
        assert_eq!(find(&sheet, "12", true), [(1, 3, s("12"))]);
        assert!(sheet.set(1, 3, "13".into()).is_err());
        let notes = open(&path, Some("Notes õ"));
        assert_eq!(find(&notes, "Tallinn", false), [(2, 1, s("Tallinn notes"))]);
        assert_eq!(find(&sheet, "Tallinn", false), [(1, 2, s("Tallinn"))]);
        // The original archive is verified too.
        let mut archive = fs::OpenOptions::new().append(true).open(&path).unwrap();
        archive.write_all(b" ").unwrap();
        let error = sheet.find(&query("x", false), (0, 0), 1).unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
    }
}

#[test]
fn xlsx_formula_overlay_and_edits_beyond_the_rectangle() {
    let dir = tempfile::tempdir().unwrap();
    let path = xlsx(dir.path());
    let mut sheet = open(&path, None);
    sheet.set(1, 6, "=SUM(B2:B3)".into()).unwrap();
    sheet.set(1, 16_383, "far".into()).unwrap();
    sheet.set(4, 16_383, "=far".into()).unwrap();
    let far = sheet.find(&query("far", false), (0, 0), 1000).unwrap();
    assert_eq!(cells(&far), [(1, 16_383, "far"), (4, 16_383, "=far")]);
    paged(&sheet, &query("far", false), &far);
    let first = sheet.find(&query("far", false), (0, 0), 1).unwrap();
    assert_eq!(first.next, Some((1, 16_384)));
    let empty = sheet.find(&query("far", true), (4, 16_384), 1).unwrap();
    assert_eq!((empty.matches.len(), empty.next), (0, None));
    assert_eq!(
        cells(&sheet.find(&query("=SUM", false), (0, 0), 9).unwrap()),
        [(1, 6, "=SUM(B2:B3)")]
    );
    // Unedited virtual columns beyond the rectangle are not searched.
    let filtered = FindQuery {
        columns: Some(vec![5, 6, 7]),
        ..query("", true)
    };
    assert!(sheet.find(&filtered, (0, 0), 9).unwrap().matches.is_empty());
    let blank = sheet.find(&query("", true), (1, 5), 9).unwrap();
    assert_eq!(cells(&blank)[0], (2, 2, ""));
}

#[test]
fn workbook_find_cli() {
    let dir = tempfile::tempdir().unwrap();
    let xlsx = xlsx(dir.path());
    let notes = qd(
        &xlsx,
        &["--sheet", "Notes õ", "--find", "tallinn", "--ignore-case"],
    );
    assert_eq!(notes["sheet"], "Notes õ");
    assert_eq!(notes["format"], "XLSX");
    assert_eq!(
        notes["matches"],
        json!([{"cell": "B3", "value": "Tallinn notes"}])
    );
    assert!(notes.get("formula_results").is_none() && notes.get("changes").is_none());
    let far = qd(
        &xlsx,
        &[
            "--set",
            "XFD2",
            "far",
            "--find",
            "far",
            "--limit",
            "1",
            "--dry-run",
        ],
    );
    assert_eq!(far["matches"], json!([{"cell": "XFD2", "value": "far"}]));
    assert_eq!(far["next"], "XFE2");
    assert_eq!(
        far["changes"],
        json!([{"cell": "XFD2", "before": "", "after": "far"}])
    );
    let after = qd(
        &xlsx,
        &[
            "--set",
            "XFD2",
            "far",
            "--find",
            "far",
            "--from",
            "XFE2",
            "--dry-run",
        ],
    );
    assert_eq!(
        (&after["matches"], &after["next"]),
        (&json!([]), &Value::Null)
    );
    let ods = ods(dir.path());
    let blanks = qd(
        &ods,
        &["--find", "", "--exact", "--columns", "E", "--limit", "2"],
    );
    assert_eq!(blanks["format"], "ODS");
    assert_eq!(
        blanks["matches"],
        json!([{"cell": "E1", "value": ""}, {"cell": "E2", "value": ""}])
    );
    assert_eq!(blanks["next"], "F2");
    let sorted = qd(&ods, &["--sort", "B:n", "--find", "Tallinn"]);
    assert_eq!(sorted["coordinates"], "sorted_view");
    assert_eq!(
        sorted["matches"],
        json!([{"cell": "C3", "value": "Tallinn"}])
    );
}
