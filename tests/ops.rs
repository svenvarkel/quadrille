//! `quadrille::ops` called in-process on CSV, XLSX and ODS files built here.
mod support;

use quadrille::{
    FindQuery, Sheet,
    a1::{self, Range},
    ops::{self, Change, Changes, Edit, Read, Report, Sorted, Source, Wait},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const CSV: &str = "id,name,note\n00123,Tallinn,\"line 1\nline 2\"\n00456,Tartu\n";

fn csv(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    fs::canonicalize(path).unwrap()
}

fn open(path: &Path, sheet: Option<&str>) -> Sheet {
    let sheet = Sheet::open_sheet(path, b',', sheet).unwrap();
    ops::wait_for_index(&sheet, Wait::All).unwrap();
    sheet
}

fn range(text: &str) -> Range {
    a1::parse_range(text).unwrap()
}

fn error<T: std::fmt::Debug>(result: quadrille::Result<T>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn waits_for_rows_or_the_whole_scan_and_reports_index_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::new();
    for i in 0..100_000 {
        text.push_str(&format!("{i},row {i}\n"));
    }
    let many = csv(dir.path(), "many.csv", &text);
    let sheet = Sheet::open(&many, b',').unwrap();
    ops::wait_for_index(&sheet, Wait::Rows(10)).unwrap();
    assert!(sheet.progress().rows >= 10);
    // More rows than exist: waits for the complete scan, then the range is refused.
    ops::wait_for_index(&sheet, Wait::Rows(1_000_000)).unwrap();
    assert!(sheet.progress().done);
    assert_eq!(sheet.progress().rows, 100_000);
    assert_eq!(
        error(ops::in_bounds(&sheet, &range("A100001"))),
        "Requested range extends beyond the last row"
    );
    ops::in_bounds(&sheet, &range("A100000")).unwrap();
    ops::wait_for_index(&sheet, Wait::All).unwrap();
    let bad = dir.path().join("bad.csv");
    fs::write(&bad, b"a,b\n\xff,c\n").unwrap();
    let sheet = Sheet::open(&bad, b',').unwrap();
    for wait in [Wait::All, Wait::Rows(5)] {
        assert!(
            error(ops::wait_for_index(&sheet, wait)).contains("invalid UTF-8"),
            "{wait:?}"
        );
    }
}

#[test]
fn sources_sheets_and_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = csv(dir.path(), "data.csv", CSV);
    let sheet = open(&path, None);
    let source = Source::of(&sheet);
    assert_eq!(
        source,
        Source {
            path: path.to_string_lossy().into_owned(),
            workbook: None
        }
    );
    assert_eq!(source.to_json(), json!({"source": path}));
    let check = ops::check(&sheet, std::time::Instant::now()).unwrap();
    assert_eq!((check.progress.rows, check.source_bytes), (3, None));
    let mut doc = check.to_json();
    assert!(
        doc.as_object_mut()
            .unwrap()
            .remove("elapsed_seconds")
            .unwrap()
            .as_f64()
            .unwrap()
            >= 0.0
    );
    assert_eq!(
        doc,
        json!({"records": 3, "bytes": CSV.len(), "index_bytes": 8})
    );
    assert_eq!(
        error(ops::sheets(&path)),
        "--sheets requires an XLSX or ODS workbook"
    );
    let xlsx = support::xlsx(dir.path());
    let sheets = ops::sheets(&xlsx).unwrap();
    assert_eq!(sheets.names, ["Data", "Notes õ"]);
    assert_eq!(
        sheets.to_json(),
        json!({"source": xlsx, "sheets": ["Data", "Notes õ"]})
    );
    for (path, format) in [(xlsx, "XLSX"), (support::ods(dir.path()), "ODS")] {
        let notes = open(&path, Some("Notes õ"));
        let source = Source::of(&notes);
        assert_eq!(
            source.to_json(),
            json!({"source": fs::canonicalize(&path).unwrap(), "sheet": "Notes õ", "format": format})
        );
        let check = ops::check(&notes, std::time::Instant::now()).unwrap();
        let mut doc = check.to_json();
        doc.as_object_mut().unwrap().remove("elapsed_seconds");
        assert_eq!(
            doc,
            json!({
                "records": 3, "bytes": check.progress.total_bytes, "index_bytes": 8,
                "sheet": "Notes õ", "format": format,
                "source_bytes": fs::metadata(&path).unwrap().len()
            })
        );
    }
}

#[test]
fn patch_entries_parse_or_fail_in_order() {
    let edits = |value| ops::parse_edits(&value);
    assert_eq!(
        edits(json!([{"cell": "B2", "value": "x"}, {"value": "", "cell": "aa65"}])).unwrap(),
        Vec::<Edit>::from([((1, 1), "x".into()), ((64, 26), String::new())])
    );
    assert!(edits(json!([])).unwrap().is_empty());
    for (patch, message) in [
        (json!({}), "Patch must be an array of {cell, value} objects"),
        (
            json!("B2"),
            "Patch must be an array of {cell, value} objects",
        ),
        (json!([1]), "Each patch entry must be an object"),
        (
            json!([{"cell": "A1"}]),
            "Each patch entry must contain only cell and value",
        ),
        (
            json!([{"cell": "A1", "value": "x", "note": "y"}]),
            "Each patch entry must contain only cell and value",
        ),
        (
            json!([{"cell": 1, "value": "x"}]),
            "Patch cell must be an A1-style string",
        ),
        (
            json!([{"a": "A1", "b": "x"}]),
            "Patch cell must be an A1-style string",
        ),
        (
            json!([{"cell": "A1", "value": 1}]),
            "Patch value must be a string; no automatic type conversion",
        ),
        (
            json!([{"cell": "A1", "value": null}]),
            "Patch value must be a string; no automatic type conversion",
        ),
        (json!([{"cell": "A0", "value": "x"}]), "Rows start at 1"),
        (
            json!([{"cell": "$A$1", "value": "x"}]),
            "Use a cell address such as B7",
        ),
        // The first bad entry wins; within an entry the value is checked before the cell.
        (
            json!([{"cell": "A0", "value": 1}, 1]),
            "Patch value must be a string; no automatic type conversion",
        ),
        (json!([{"cell": "A0", "value": "x"}, 1]), "Rows start at 1"),
    ] {
        assert_eq!(error(edits(patch.clone())), message, "{patch}");
    }
}

#[test]
fn edits_report_net_changes_by_cell() {
    let dir = tempfile::tempdir().unwrap();
    let mut sheet = open(&csv(dir.path(), "data.csv", CSV), None);
    let changes = ops::apply_edits(
        &mut sheet,
        vec![
            ((2, 0), "x".into()),
            ((1, 2), "c".into()),
            ((2, 0), "y".into()),
            ((1, 0), "00123".into()),
            ((1, 1), "gone".into()),
            ((1, 1), "Tallinn".into()),
        ],
    )
    .unwrap();
    let change = |cell, before: &str, after: &str| Change {
        cell,
        before: before.into(),
        after: after.into(),
    };
    assert_eq!(
        changes,
        Changes(vec![
            change((1, 2), "line 1\nline 2", "c"),
            change((2, 0), "00456", "y")
        ])
    );
    assert_eq!(
        changes.to_json(),
        json!([
            {"cell": "C2", "before": "line 1\nline 2", "after": "c"},
            {"cell": "A3", "before": "00456", "after": "y"}
        ])
    );
    assert_eq!(sheet.edit_count(), 2);
    assert_eq!(
        ops::apply_edits(&mut sheet, vec![]).unwrap(),
        Changes::default()
    );
    assert_eq!(Changes::default().to_json(), json!([]));
    // A ragged record has no field to edit; earlier edits of the batch stay applied.
    assert_eq!(
        error(ops::apply_edits(
            &mut sheet,
            vec![((0, 0), "ID".into()), ((2, 2), "x".into())]
        )),
        "No cell at this position"
    );
    assert_eq!(sheet.edit_count(), 3);
}

#[test]
fn reads_are_bounded_and_show_edits_formulas_and_sorting() {
    let widest = format!("A1:{}1", a1::column_name(ops::READ_CELLS - 1));
    let too_wide = format!("A1:{}1", a1::column_name(ops::READ_CELLS));
    for (text, ok) in [
        ("A1:A10000", true),
        ("A1:A10001", false),
        ("B2:B10001", true),
        ("A1:J10000", true),
        ("A1:K10000", false),
        (&widest, true),
        (&too_wide, false),
        ("A1:ZZZ100", false),
    ] {
        assert_eq!(ops::read_range(text).is_ok(), ok, "{text}");
    }
    // Rows × columns beyond usize is refused, not wrapped.
    let huge = format!("A1:{}2", a1::column_name(usize::MAX - 1));
    assert_eq!(
        error(ops::read_range(&huge)),
        "Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks"
    );
    assert_eq!(
        error(ops::read_range("B2:A1")),
        "Range must run from top-left to bottom-right"
    );
    assert_eq!(error(ops::read_range("A0")), "Rows start at 1");

    let dir = tempfile::tempdir().unwrap();
    let mut sheet = open(&csv(dir.path(), "data.csv", CSV), None);
    ops::apply_edits(&mut sheet, vec![((1, 1), "changed".into())]).unwrap();
    let read = ops::read(&mut sheet, ops::read_range("A2:D3").unwrap()).unwrap();
    let text = |v: &str| Some(v.to_owned());
    assert_eq!(
        read,
        Read {
            range: range("A2:D3"),
            rows: vec![
                vec![text("00123"), text("changed"), text("line 1\nline 2"), None],
                vec![text("00456"), text("Tartu"), None, None]
            ],
            formulas: None
        }
    );
    assert_eq!(
        read.to_json(),
        json!({
            "range": "A2:D3",
            "rows": [["00123", "changed", "line 1\nline 2", null], ["00456", "Tartu", null, null]]
        })
    );
    assert_eq!(
        error(ops::read(&mut sheet, range("A4"))),
        "Requested range extends beyond the last row"
    );
    let sorted = ops::sort(&mut sheet, "-A", true).unwrap();
    assert_eq!(
        sorted,
        Sorted {
            columns: "-A".into(),
            header: true
        }
    );
    assert_eq!(
        sorted.to_json(),
        json!({"columns": "-A", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"})
    );
    assert_eq!(
        ops::read(&mut sheet, range("A1:A3")).unwrap().rows,
        [[text("id")], [text("00456")], [text("00123")]]
    );
    for (spec, message) in [
        ("A,A", "A sort column may only appear once"),
        ("D", "Sort column 4 is outside the header"),
    ] {
        assert_eq!(error(ops::sort(&mut sheet, spec, true)), message);
    }
    assert_eq!(
        ops::sort(&mut sheet, "B", false).unwrap().to_json()["header"],
        false
    );

    let book = support::xlsx(dir.path());
    let mut sheet = open(&book, None);
    ops::apply_edits(&mut sheet, vec![((1, 6), "=1+1".into())]).unwrap();
    let read = ops::read(&mut sheet, range("C2:G3")).unwrap();
    assert_eq!(
        read.formulas,
        Some(BTreeMap::from([
            ((1, 3), "SUM(B2:B3)".to_owned()),
            ((1, 4), "1+1".to_owned()),
            ((1, 6), "1+1".to_owned())
        ]))
    );
    assert_eq!(
        read.to_json(),
        json!({
            "range": "C2:G3",
            "rows": [["Tallinn", "12", "", "", "=1+1"], ["", "", "", "", ""]],
            "formulas": {"D2": "SUM(B2:B3)", "E2": "1+1", "G2": "1+1"}
        })
    );
    let ods = support::ods(dir.path());
    let read = ops::read(&mut open(&ods, None), range("A1")).unwrap();
    assert_eq!(
        read.to_json(),
        json!({"range": "A1:A1", "rows": [["id"]], "formulas": {}})
    );
}

#[test]
fn finds_render_their_query_sort_and_preview() {
    let dir = tempfile::tempdir().unwrap();
    let path = csv(dir.path(), "data.csv", CSV);
    let mut sheet = Sheet::open(&path, b',').unwrap();
    let query = |text: &str| FindQuery {
        text: text.into(),
        ..FindQuery::default()
    };
    // Find needs no index; defaults are A1 and 100 matches.
    let found = ops::find(&sheet, query("0"), None, None).unwrap();
    assert_eq!((found.from, found.limit), ((0, 0), ops::FIND_LIMIT));
    assert_eq!(
        found.to_json(),
        json!({
            "source": path,
            "find": {"text": "0", "columns": null, "exact": false, "ignore_case": false, "from": "A1", "limit": 100},
            "coordinates": "source",
            "matches": [{"cell": "A2", "value": "00123"}, {"cell": "A3", "value": "00456"}],
            "next": null,
            "records_scanned": 3
        })
    );
    ops::wait_for_index(&sheet, Wait::All).unwrap();
    let changes = ops::apply_edits(&mut sheet, vec![((2, 1), "Tallinn".into())]).unwrap();
    let sorted = ops::sort(&mut sheet, "B", true).unwrap();
    let mut found = ops::find(
        &sheet,
        FindQuery {
            columns: Some(vec![2, 1]),
            ignore_case: true,
            exact: true,
            ..query("TALLINN")
        },
        Some((1, 1)),
        Some(1),
    )
    .unwrap();
    found.sort = Some(sorted);
    found.preview = Some(changes);
    assert_eq!(
        found.to_json(),
        json!({
            "source": path,
            "find": {"text": "TALLINN", "columns": ["C", "B"], "exact": true, "ignore_case": true, "from": "B2", "limit": 1},
            "coordinates": "sorted_view",
            "matches": [{"cell": "B2", "value": "Tallinn"}],
            "next": "C2",
            "records_scanned": 1,
            "sort": {"columns": "B", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"},
            "changes": [{"cell": "B3", "before": "Tartu", "after": "Tallinn"}],
            "dry_run": true
        })
    );
    assert_eq!(
        error(ops::find(&sheet, query(""), None, None)),
        "Search text must not be empty unless the match is exact"
    );
    for bad in [0, ops::FIND_LIMIT_MAX + 1] {
        assert_eq!(
            error(ops::find(&sheet, query("a"), None, Some(bad))),
            ops::FIND_LIMIT_ERROR
        );
    }
    assert_eq!(ops::find_limit(None).unwrap(), ops::FIND_LIMIT);
    assert_eq!(
        ops::find_limit(Some(ops::FIND_LIMIT_MAX)).unwrap(),
        ops::FIND_LIMIT_MAX
    );
    let book = support::xlsx(dir.path());
    let notes = open(&book, Some("Notes õ"));
    let doc = ops::find(&notes, query("notes"), None, Some(5))
        .unwrap()
        .to_json();
    assert_eq!(
        (&doc["sheet"], &doc["format"]),
        (&json!("Notes õ"), &json!("XLSX"))
    );
    assert_eq!(
        doc["matches"],
        json!([{"cell": "B3", "value": "Tallinn notes"}])
    );
}

#[test]
fn saves_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let path = csv(dir.path(), "data.csv", CSV);
    let mut sheet = open(&path, None);
    let changes = ops::apply_edits(&mut sheet, vec![((2, 1), "Narva".into())]).unwrap();
    let output = dir.path().join("out.csv");
    assert_eq!(ops::save(&sheet, &output).unwrap(), output);
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        CSV.replace("Tartu", "Narva")
    );
    assert_eq!(
        error(ops::save(&sheet, &output)),
        "Destination already exists; choose a new filename"
    );
    let report = Report {
        source: Source::of(&sheet),
        changes,
        dry_run: false,
        sort: None,
        read: None,
        output: Some(output.clone()),
    };
    assert_eq!(
        report.to_json(),
        json!({
            "source": path, "dry_run": false, "output": output,
            "changes": [{"cell": "B3", "before": "Tartu", "after": "Narva"}]
        })
    );
    let sort = ops::sort(&mut sheet, "B", true).unwrap();
    let read = ops::read(&mut sheet, range("B2:B3")).unwrap();
    assert_eq!(
        Report {
            source: Source::of(&sheet),
            changes: Changes::default(),
            dry_run: true,
            sort: Some(sort),
            read: Some(read),
            output: None,
        }
        .to_json(),
        json!({
            "source": path, "changes": [], "dry_run": true, "range": "B2:B3",
            "rows": [["Narva"], ["Tallinn"]],
            "sort": {"columns": "B", "header": true, "changes_coordinates": "source", "read_coordinates": "sorted_view"}
        })
    );
    let book = support::ods(dir.path());
    let mut sheet = open(&book, None);
    let read = ops::read(&mut sheet, range("D5")).unwrap();
    assert_eq!(
        Report {
            source: Source::of(&sheet),
            changes: Changes::default(),
            dry_run: false,
            sort: None,
            read: Some(read),
            output: None,
        }
        .to_json(),
        json!({
            "source": fs::canonicalize(&book).unwrap(), "sheet": "Data", "format": "ODS",
            "changes": [], "dry_run": false, "range": "D5:D5", "rows": [[""]], "formulas": {},
            "formula_results": "existing results are cached; new formulas are calculated when the saved file opens",
            "edit_type": "text; XLSX values beginning with = are formulas"
        })
    );
}
