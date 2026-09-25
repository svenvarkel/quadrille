//! Workbook fixtures built in the test; no binary fixtures.
#![allow(dead_code)]
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
use zip::{ZipWriter, write::SimpleFileOptions};

const X: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const P: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const T: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const O: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
const TX: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";

pub fn pack(path: &Path, entries: &[(&str, String)]) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    for (name, text) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

/// Data: A1:E5 with a cached formula (D2), an uncached one (E2) and blank rows/cells.
pub fn xlsx(dir: &Path) -> PathBuf {
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
pub fn ods(dir: &Path) -> PathBuf {
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
