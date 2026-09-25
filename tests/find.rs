//! Find over XLSX and ODS packages built here; no binary fixtures.
use quadrille::{FindQuery, FindResult, Sheet};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use zip::{ZipWriter, write::SimpleFileOptions};

const X: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const P: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const T: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const O: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
const TX: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";

fn pack(path: &Path, entries: &[(&str, String)]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, text) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

/// Data: A1:E5 with a cached formula (D2), an uncached one (E2) and blank rows/cells.
fn xlsx(dir: &Path) -> PathBuf {
    let path = dir.join("book.xlsx");
    let inline = |r: &str, t: &str| format!(r#"<c r="{r}" t="inlineStr"><is><t>{t}</t></is></c>"#);
    let data = format!(
        r#"<worksheet xmlns="{X}"><sheetData><row r="1">{}{}{}{}</row><row r="2">{}<c r="B2"><v>10</v></c>{}<c r="D2"><f>SUM(B2:B3)</f><v>12</v></c><c r="E2"><f>1+1</f></c></row><row r="3">{}<c r="B3"><v>2</v></c></row><row r="5">{}</row></sheetData></worksheet>"#,
        inline("A1", "id"),
        inline("B1", "amount"),
        inline("C1", "note"),
        inline("D1", "formula"),
        inline("A2", "00123"),
        inline("C2", "Tallinn"),
        inline("A3", "00456"),
        inline("C5", "end"),
    );
    pack(
        &path,
        &[
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.into()),
            ("_rels/.rels", format!(r#"<Relationships xmlns="{P}"><Relationship Id="b" Type="{R}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#)),
            ("xl/workbook.xml", format!(r#"<workbook xmlns="{X}" xmlns:r="{R}"><sheets><sheet name="Data" sheetId="1" r:id="r1"/><sheet name="Notes õ" sheetId="2" r:id="r2"/></sheets></workbook>"#)),
            ("xl/_rels/workbook.xml.rels", format!(r#"<Relationships xmlns="{P}"><Relationship Id="r1" Type="{R}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="r2" Type="{R}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#)),
            ("xl/worksheets/sheet1.xml", data),
            ("xl/worksheets/sheet2.xml", format!(r#"<worksheet xmlns="{X}"><sheetData><row r="3">{}</row></sheetData></worksheet>"#, inline("B3", "Tallinn notes"))),
        ],
    );
    path
}

/// The same Data layout as `xlsx`, with ODS formulas in D2 (cached) and E2 (uncached).
fn ods(dir: &Path) -> PathBuf {
    let path = dir.join("book.ods");
    let text = |t: &str| {
        format!(
            r#"<table:table-cell office:value-type="string"><text:p>{t}</text:p></table:table-cell>"#
        )
    };
    let float = |v: &str| {
        format!(
            r#"<table:table-cell office:value-type="float" office:value="{v}"><text:p>{v}</text:p></table:table-cell>"#
        )
    };
    let content = format!(
        r#"<office:document-content xmlns:office="{O}" xmlns:table="{T}" xmlns:text="{TX}" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2" office:version="1.2"><office:body><office:spreadsheet><table:table table:name="Data"><table:table-row>{}{}{}{}</table:table-row><table:table-row>{}{}{}<table:table-cell table:formula="of:=SUM([.B2:.B3])" office:value-type="float" office:value="12"><text:p>12</text:p></table:table-cell><table:table-cell table:formula="of:=1+1"/></table:table-row><table:table-row>{}{}</table:table-row><table:table-row/><table:table-row><table:table-cell table:number-columns-repeated="2"/>{}</table:table-row></table:table><table:table table:name="Notes õ"><table:table-row table:number-rows-repeated="2"/><table:table-row><table:table-cell/>{}</table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
        text("id"),
        text("amount"),
        text("note"),
        text("formula"),
        text("00123"),
        float("10"),
        text("Tallinn"),
        text("00456"),
        float("2"),
        text("end"),
        text("Tallinn notes"),
    );
    pack(
        &path,
        &[
            ("mimetype", "application/vnd.oasis.opendocument.spreadsheet".into()),
            ("META-INF/manifest.xml", r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#.into()),
            ("content.xml", content),
        ],
    );
    path
}

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
