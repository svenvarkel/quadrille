//! Workbook values use the CSV engine; native saves patch only edited XML cells.
use super::*;
use a1::{A1Error, Address, cell_name};
use calamine::Reader;
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::BTreeSet;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

const TABLE: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const OFFICE: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
const TEXT: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
// ponytail: workbook import and XML patches are in memory, unlike large CSVs.
// Add streaming workbook readers/patchers when these explicit limits are too small.
const XML_LIMIT: u64 = 128 * 1024 * 1024;
const PACKAGE_LIMIT: u64 = 256 * 1024 * 1024;
const CELL_LIMIT: u64 = 5_000_000;
const NODE_LIMIT: u32 = 8_000_000;
type CellRange = (Address, Address);

pub(crate) struct Workbook {
    source: PathBuf,
    stamp: Stamp,
    pub cache: tempfile::NamedTempFile,
    pub name: String,
    pub names: Vec<String>,
    pub format: String,
    part: String,
    pub formulas: BTreeMap<Address, String>,
    protected: Vec<CellRange>,
}

pub(crate) fn is_workbook(path: &Path) -> bool {
    matches!(extension(path).as_str(), "xlsx" | "ods")
}

fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}

fn xml(text: &str) -> Result<Document<'_>> {
    Ok(Document::parse_with_options(
        text,
        ParsingOptions {
            nodes_limit: NODE_LIMIT,
            ..ParsingOptions::default()
        },
    )?)
}

fn archive(path: &Path) -> Result<ZipArchive<File>> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let mut size = 0u64;
    let mut names = BTreeSet::new();
    for i in 0..zip.len() {
        let file = zip.by_index(i)?;
        if !names.insert(file.name().to_owned()) {
            return Err("Workbook contains duplicate ZIP members".into());
        }
        size = size
            .checked_add(file.size())
            .ok_or("Workbook is too large")?;
        if size > PACKAGE_LIMIT {
            return Err(
                "Workbook exceeds the 256 MiB uncompressed import limit; use CSV for larger data"
                    .into(),
            );
        }
        if file.name().ends_with(".xml") && file.size() > XML_LIMIT {
            return Err(
                "Workbook XML exceeds the 128 MiB import limit; use CSV for larger data".into(),
            );
        }
    }
    Ok(zip)
}

fn read_xml(zip: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let mut text = String::new();
    zip.by_name(name)?
        .take(XML_LIMIT + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > XML_LIMIT {
        return Err("Workbook XML exceeds the 128 MiB limit".into());
    }
    Ok(text)
}

fn target(base: &str, relative: &str) -> Result<String> {
    let joined = if relative.starts_with('/') {
        relative.trim_start_matches('/').to_owned()
    } else {
        format!("{base}{relative}")
    };
    let mut parts = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Invalid workbook relationship path")?;
            }
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

fn parts(zip: &mut ZipArchive<File>, format: &str) -> Result<Vec<(String, String)>> {
    if format == "ods" {
        let text = read_xml(zip, "content.xml")?;
        let doc = xml(&text)?;
        return doc
            .descendants()
            .filter(|n| n.has_tag_name((TABLE, "table")))
            .map(|n| {
                Ok((
                    n.attribute((TABLE, "name"))
                        .ok_or("Unnamed ODS sheet")?
                        .to_owned(),
                    "content.xml".into(),
                ))
            })
            .collect();
    }
    let text = read_xml(zip, "_rels/.rels")?;
    let doc = xml(&text)?;
    let office = doc
        .descendants()
        .find(|n| {
            n.attribute("Type")
                .is_some_and(|s| s.ends_with("/officeDocument"))
        })
        .ok_or("Missing workbook relationship")?;
    if office.attribute("TargetMode") == Some("External") {
        return Err("External workbook relationships are unsupported".into());
    }
    let book = target(
        "",
        office
            .attribute("Target")
            .ok_or("Missing workbook target")?,
    )?;
    let (base, filename) = book
        .rsplit_once('/')
        .map_or((String::new(), book.as_str()), |(p, f)| {
            (format!("{p}/"), f)
        });
    let rels = read_xml(zip, &format!("{base}_rels/{filename}.rels"))?;
    let rels = xml(&rels)?;
    let text = read_xml(zip, &book)?;
    let doc = xml(&text)?;
    doc.descendants()
        .filter(|n| n.tag_name().name() == "sheet")
        .map(|sheet| {
            let name = sheet.attribute("name").ok_or("Unnamed XLSX sheet")?;
            let id = sheet
                .attributes()
                .find(|a| a.name() == "id")
                .ok_or("Missing sheet relationship")?
                .value();
            let rel = rels
                .descendants()
                .find(|n| n.attribute("Id") == Some(id))
                .ok_or("Missing worksheet relationship")?;
            if rel.attribute("TargetMode") == Some("External") {
                return Err("External worksheet relationships are unsupported".into());
            }
            Ok((
                name.to_owned(),
                target(
                    &base,
                    rel.attribute("Target").ok_or("Missing worksheet target")?,
                )?,
            ))
        })
        .collect()
}

pub(crate) fn sheet_names(path: &Path) -> Result<Vec<String>> {
    if !is_workbook(path) {
        return Err("--sheets requires an XLSX or ODS workbook".into());
    }
    let mut zip = archive(path)?;
    Ok(parts(&mut zip, &extension(path))?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// A1 cell with optional `$` anchors, within 1,048,576 rows and 16,384 columns.
fn address(text: &str) -> Result<Address> {
    let (row, col) = a1::parse_cell(&text.replace('$', "")).map_err(|error| match error {
        A1Error::ColumnTooLarge => "Workbook column overflow".into(),
        A1Error::RowTooLarge(error) => error.to_string(),
        A1Error::RowZero => "Workbook rows start at 1".into(),
        _ => "Invalid workbook cell address".to_owned(),
    })?;
    if row >= 1_048_576 || col >= 16_384 {
        return Err("Workbook cell exceeds supported sheet dimensions".into());
    }
    Ok((row, col))
}

fn cell_range(text: &str) -> Result<CellRange> {
    let (first, last) = a1::split_range(text);
    let range = a1::Range::new(address(first)?, address(last)?)
        .map_err(|_| "Invalid workbook cell range")?;
    Ok((range.first, range.last))
}

fn repeat(node: Node<'_, '_>, name: &str) -> Result<u64> {
    let count = node.attribute((TABLE, name)).unwrap_or("1").parse()?;
    if count == 0 || count > 1_048_576 {
        return Err("Invalid ODS repetition count".into());
    }
    Ok(count)
}

fn check_size(rows: u64, cols: u64) -> Result<()> {
    if rows > 1_048_576 || cols > 16_384 || rows.checked_mul(cols).is_none_or(|n| n > CELL_LIMIT) {
        return Err("Workbook sheet exceeds the 5 million cell import limit (including leading empty cells); use CSV for larger data".into());
    }
    Ok(())
}

fn ods_text(node: Node<'_, '_>) -> Result<String> {
    if node.is_text() {
        return Ok(node.text().unwrap_or_default().to_owned());
    }
    if node.has_tag_name((TEXT, "tab")) {
        return Ok("\t".into());
    }
    if node.has_tag_name((TEXT, "line-break")) {
        return Ok("\n".into());
    }
    if node.has_tag_name((TEXT, "s")) {
        let count = node
            .attribute((TEXT, "c"))
            .unwrap_or("1")
            .parse::<usize>()?;
        if count > XML_LIMIT as usize {
            return Err("ODS text repetition exceeds the import limit".into());
        }
        return Ok(" ".repeat(count));
    }
    if node.has_tag_name((OFFICE, "annotation")) {
        return Ok(String::new());
    }
    node.children()
        .map(ods_text)
        .collect::<Result<Vec<_>>>()
        .map(|s| s.concat())
}

// Validate sparse/repeated dimensions before the reader allocates dense ranges.
fn preflight(doc: &Document<'_>, format: &str) -> Result<()> {
    if format == "xlsx" {
        let (mut rows, mut cols) = (0, 0);
        for cell in doc.descendants().filter(|n| n.tag_name().name() == "c") {
            let (row, col) = address(
                cell.attribute("r")
                    .ok_or("XLSX cells without addresses are unsupported")?,
            )?;
            rows = rows.max(row + 1);
            cols = cols.max(col as u64 + 1);
        }
        check_size(rows, cols)?;
    } else {
        for table in doc
            .descendants()
            .filter(|n| n.has_tag_name((TABLE, "table")))
        {
            let (mut row, mut max_row, mut max_col) = (0u64, 0u64, 0u64);
            for r in table
                .descendants()
                .filter(|n| n.has_tag_name((TABLE, "table-row")))
            {
                row = row
                    .checked_add(repeat(r, "number-rows-repeated")?)
                    .ok_or("ODS row overflow")?;
                let mut col = 0u64;
                for cell in r.children().filter(|n| {
                    n.has_tag_name((TABLE, "table-cell"))
                        || n.has_tag_name((TABLE, "covered-table-cell"))
                }) {
                    col = col
                        .checked_add(repeat(cell, "number-columns-repeated")?)
                        .ok_or("ODS column overflow")?;
                    if cell.attribute((OFFICE, "value-type")).is_some()
                        || cell.attribute((TABLE, "formula")).is_some()
                        || cell.children().any(|n| n.is_element())
                    {
                        max_row = max_row.max(row);
                        max_col = max_col.max(col);
                        check_size(max_row, max_col)?;
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn open(path: &Path, name: Option<&str>) -> Result<Sheet> {
    let source = fs::canonicalize(path)?;
    let stamp = Stamp::new(fs::metadata(&source)?)?;
    let format = extension(path);
    let mut zip = archive(&source)?;
    let parts = parts(&mut zip, &format)?;
    let selected = match name {
        Some(name) => parts
            .iter()
            .find(|(n, _)| n == name)
            .ok_or_else(|| format!("No sheet named {name:?}; use --sheets"))?,
        None => parts.first().ok_or("Workbook has no sheets")?,
    };
    // ODS is loaded eagerly by calamine, so preflight its entire content first.
    let text = read_xml(&mut zip, &selected.1)?;
    let doc = xml(&text)?;
    preflight(&doc, &format)?;
    let mut protected = Vec::new();
    let mut text_overrides = BTreeMap::new();
    let mut has_formulas = format == "ods";
    if format == "xlsx" {
        for node in doc.descendants() {
            if node.tag_name().name() == "f" {
                has_formulas = true;
            }
            if node.tag_name().name() == "mergeCell"
                || (node.tag_name().name() == "f" && node.attribute("t") == Some("array"))
            {
                if let Some(r) = node.attribute("ref") {
                    protected.push(cell_range(r)?);
                }
            }
        }
    } else {
        let table = doc
            .descendants()
            .find(|n| {
                n.has_tag_name((TABLE, "table"))
                    && n.attribute((TABLE, "name")) == Some(selected.0.as_str())
            })
            .ok_or("Missing ODS sheet")?;
        let mut r = 0;
        for row in table
            .descendants()
            .filter(|n| n.has_tag_name((TABLE, "table-row")))
        {
            let row_count = repeat(row, "number-rows-repeated")?;
            let mut c = 0usize;
            for cell in row.children().filter(|n| {
                n.has_tag_name((TABLE, "table-cell"))
                    || n.has_tag_name((TABLE, "covered-table-cell"))
            }) {
                let col_count = repeat(cell, "number-columns-repeated")? as usize;
                // calamine 0.31 omits ODS text:tab and text:line-break. Preserve
                // those values in the view/export as well as in the native file.
                if cell.attribute((OFFICE, "value-type")) == Some("string")
                    && cell.attribute((OFFICE, "string-value")).is_none()
                    && cell.descendants().any(|n| {
                        n.has_tag_name((TEXT, "tab")) || n.has_tag_name((TEXT, "line-break"))
                    })
                {
                    let value = cell
                        .children()
                        .filter(|n| n.has_tag_name((TEXT, "p")))
                        .map(ods_text)
                        .collect::<Result<Vec<_>>>()?
                        .join("\n");
                    for rr in r..r + row_count {
                        for cc in c..c + col_count {
                            text_overrides.insert((rr, cc), value.clone());
                        }
                    }
                }
                if cell.has_tag_name((TABLE, "covered-table-cell"))
                    || [
                        "number-columns-spanned",
                        "number-rows-spanned",
                        "number-matrix-columns-spanned",
                        "number-matrix-rows-spanned",
                    ]
                    .iter()
                    .any(|name| cell.attribute((TABLE, *name)).is_some())
                {
                    let rows = repeat(cell, "number-rows-spanned")?
                        .max(repeat(cell, "number-matrix-rows-spanned")?);
                    let cols = repeat(cell, "number-columns-spanned")?
                        .max(repeat(cell, "number-matrix-columns-spanned")?)
                        as usize;
                    protected.push(((r, c), (r + row_count + rows - 2, c + col_count + cols - 2)));
                }
                c = c.checked_add(col_count).ok_or("ODS column overflow")?;
            }
            r = r.checked_add(row_count).ok_or("ODS row overflow")?;
        }
    }
    drop(doc);
    drop(text);
    drop(zip);
    let mut book = calamine::open_workbook_auto(&source)?;
    let values = book.worksheet_range(&selected.0)?;
    let mut formulas = BTreeMap::new();
    let formula_end = if has_formulas {
        let formula_range = book.worksheet_formula(&selected.0)?;
        if let Some((r, c)) = formula_range.start() {
            for (row, col, formula) in formula_range.used_cells() {
                formulas.insert(
                    (r as u64 + row as u64, c as usize + col),
                    formula.to_owned(),
                );
            }
        }
        formula_range.end()
    } else {
        None
    };
    let end = values
        .end()
        .into_iter()
        .chain(formula_end)
        .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)));
    let mut cache = tempfile::NamedTempFile::new()?;
    let mut writer = csv::Writer::from_writer(cache.as_file_mut());
    if let Some((last_row, last_col)) = end {
        check_size(last_row as u64 + 1, last_col as u64 + 1)?;
        for row in 0..=last_row {
            writer.write_record((0..=last_col).map(|col| {
                text_overrides
                    .get(&(row as u64, col as usize))
                    .cloned()
                    .unwrap_or_else(|| {
                        values
                            .get_value((row, col))
                            .map(ToString::to_string)
                            .unwrap_or_default()
                    })
            }))?;
        }
    }
    writer.flush()?;
    drop(writer);
    drop(book);
    stamp.check(&source)?;
    let mut sheet = Sheet::open_csv(cache.path(), b',')?;
    sheet.path = source.clone();
    sheet.workbook = Some(Arc::new(Workbook {
        source,
        stamp,
        cache,
        name: selected.0.clone(),
        names: parts.iter().map(|(n, _)| n.clone()).collect(),
        format: format.to_ascii_uppercase(),
        part: selected.1.clone(),
        formulas,
        protected,
    }));
    Ok(sheet)
}

impl Workbook {
    pub fn check_source(&self) -> Result<()> {
        self.stamp.check(&self.source)
    }

    pub fn validate_edit(&self, row: u64, col: usize, value: &str) -> Result<()> {
        if self.format == "XLSX" {
            check_size(row + 1, col as u64 + 1)?;
        }
        if self.formulas.contains_key(&(row, col))
            || self
                .protected
                .iter()
                .any(|&(a, b)| row >= a.0 && row <= b.0 && col >= a.1 && col <= b.1)
        {
            return Err("Formula and merged/array cells are read-only in this version".into());
        }
        if value.chars().any(|c| !matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
            return Err("Workbook text contains a character forbidden by XML 1.0".into());
        }
        if self.format == "XLSX" && value == "=" {
            return Err("Enter a formula after =".into());
        }
        if self.format == "XLSX"
            && value
                .strip_prefix('=')
                .is_some_and(|formula| formula.encode_utf16().count() > 8_192)
        {
            return Err("XLSX formulas are limited to 8,192 UTF-16 code units".into());
        }
        if self.format == "XLSX" && value.encode_utf16().count() > 32_767 {
            return Err("XLSX cell text is limited to 32,767 UTF-16 code units".into());
        }
        Ok(())
    }

    pub fn validate_destination(&self, path: &Path, sorted: bool) -> Result<()> {
        let ext = extension(path);
        if ext == "csv" {
            return Ok(());
        }
        if ext != self.format.to_ascii_lowercase() {
            return Err(format!(
                "Save as .{} to preserve the workbook, or .csv to export the selected sheet",
                self.format.to_ascii_lowercase()
            )
            .into());
        }
        if sorted {
            return Err("Native workbook saves require source row order: clear the sort, or export the sorted view as .csv".into());
        }
        Ok(())
    }

    pub fn save(
        &self,
        edits: &Edits,
        destination: &Path,
        mut temp: tempfile::NamedTempFile,
        progress: &AtomicU64,
    ) -> Result<()> {
        self.check_source()?;
        if edits.is_empty() {
            copy_bytes(
                &mut File::open(&self.source)?,
                temp.as_file_mut(),
                self.stamp.len,
                progress,
            )?;
        } else {
            let mut input = archive(&self.source)?;
            if input.file_names().any(|n| {
                n.starts_with("_xmlsignatures/")
                    || n.ends_with("documentsignatures.xml")
                    || n.ends_with("macrosignatures.xml")
            }) {
                return Err("Digitally signed workbooks cannot be edited without invalidating their signatures".into());
            }
            let text = read_xml(&mut input, &self.part)?;
            let doc = xml(&text)?;
            let replacement = if self.format == "XLSX" {
                patch_xlsx(&doc, &text, edits)?
            } else {
                patch_ods(&doc, &text, &self.name, edits)?
            };
            xml(&replacement)?;
            let mut output = ZipWriter::new(temp.as_file_mut());
            output.set_raw_comment(input.comment().into());
            for i in 0..input.len() {
                let file = input.by_index(i)?;
                let size = file.compressed_size();
                if file.name() == self.part {
                    let mut options =
                        SimpleFileOptions::default().compression_method(file.compression());
                    if let Some(time) = file.last_modified() {
                        options = options.last_modified_time(time);
                    }
                    if let Some(mode) = file.unix_mode() {
                        options = options.unix_permissions(mode);
                    }
                    output.start_file(file.name(), options)?;
                    output.write_all(replacement.as_bytes())?;
                } else {
                    output.raw_copy_file(file)?;
                }
                progress.fetch_add(size, Ordering::Relaxed);
            }
            output.finish()?;
        }
        self.check_source()?;
        temp.as_file().sync_all()?;
        if destination == self.source {
            temp.persist(destination)?;
        } else {
            temp.persist_noclobber(destination)?;
        }
        Ok(())
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
}

// Preserve all original markup except the exact spans being replaced.
fn splice(text: &str, mut changes: Vec<(std::ops::Range<usize>, String)>) -> Result<String> {
    changes.sort_by_key(|(range, _)| range.start);
    let mut result = String::new();
    let mut cursor = 0;
    for (range, replacement) in changes {
        if range.start < cursor {
            return Err("Overlapping workbook XML edits".into());
        }
        result.push_str(&text[cursor..range.start]);
        result.push_str(&replacement);
        cursor = range.end;
    }
    result.push_str(&text[cursor..]);
    Ok(result)
}

fn qualified(node: Node<'_, '_>, local: &str) -> String {
    node.tag_name()
        .namespace()
        .and_then(|ns| node.lookup_prefix(ns))
        .map_or_else(|| local.to_owned(), |p| format!("{p}:{local}"))
}

fn start_tag(
    node: Node<'_, '_>,
    text: &str,
    remove: &[(&str, &str)],
    extra: &str,
) -> Result<String> {
    let start = node.range().start;
    // Attribute values may contain '>'; use the parser's last attribute boundary.
    let after_attrs = node
        .attributes()
        .next_back()
        .map_or(start, |a| a.range().end);
    let end = after_attrs + text[after_attrs..].find('>').ok_or("Missing XML tag end")?;
    let changes = node
        .attributes()
        .filter(|a| {
            remove
                .iter()
                .any(|&(ns, name)| a.name() == name && a.namespace().unwrap_or("") == ns)
        })
        .map(|a| {
            let r = a.range();
            (r.start - start..r.end - start, String::new())
        })
        .collect();
    let tag = splice(&text[start..end], changes)?;
    Ok(format!("{}{extra}>", tag.trim_end().trim_end_matches('/')))
}

fn xlsx_cell(
    node: Option<Node<'_, '_>>,
    parent: Node<'_, '_>,
    text: &str,
    at: Address,
    value: &str,
) -> Result<String> {
    let c = qualified(node.unwrap_or(parent), "c");
    let f = qualified(parent, "f");
    let is = qualified(parent, "is");
    let t = qualified(parent, "t");
    let tail: String = node
        .into_iter()
        .flat_map(|n| n.children())
        .filter(|n| n.is_element() && !matches!(n.tag_name().name(), "v" | "is" | "f"))
        .map(|n| &text[n.range()])
        .collect();
    if let Some(formula) = value.strip_prefix('=') {
        let start = match node {
            Some(n) => start_tag(n, text, &[("", "t")], "")?,
            None => format!("<{c} r=\"{}\">", cell_name(at)),
        };
        return Ok(format!("{start}<{f}>{}</{f}>{tail}</{c}>", escape(formula)));
    }
    // OOXML escape-looking literals must be escaped before writing inline text.
    let mut literal = String::new();
    for (i, ch) in value.char_indices() {
        if ch == '_'
            && value.get(i..i + 7).is_some_and(|s| {
                s.starts_with("_x")
                    && s.ends_with('_')
                    && s[2..6].bytes().all(|b| b.is_ascii_hexdigit())
            })
        {
            literal.push_str("_x005F_");
        } else {
            literal.push(ch);
        }
    }
    let start = match node {
        Some(n) => start_tag(n, text, &[("", "t")], " t=\"inlineStr\"")?,
        None => format!("<{c} r=\"{}\" t=\"inlineStr\">", cell_name(at)),
    };
    // Keep non-value children such as extension metadata intact.
    Ok(format!(
        "{start}<{is}><{t} xml:space=\"preserve\">{}</{t}></{is}>{tail}</{c}>",
        escape(&literal)
    ))
}

fn patch_xlsx(doc: &Document<'_>, text: &str, edits: &Edits) -> Result<String> {
    let data = doc
        .descendants()
        .find(|n| n.tag_name().name() == "sheetData")
        .ok_or("Missing XLSX sheetData")?;
    let mut changes = Vec::new();
    let mut remaining = edits.clone();
    if let Some(dimension) = doc
        .descendants()
        .find(|node| node.tag_name().name() == "dimension")
    {
        if let Some(reference) = dimension.attribute_node("ref") {
            let (first, mut last) = cell_range(reference.value())?;
            let original = last;
            for &(row, col) in edits.keys() {
                last = (last.0.max(row), last.1.max(col));
            }
            if last != original {
                changes.push((
                    reference.range(),
                    format!("ref=\"{}\"", a1::Range { first, last }),
                ));
            }
        }
    }
    for row in data.children().filter(|n| n.tag_name().name() == "row") {
        let number = row
            .attribute("r")
            .ok_or("XLSX rows without numbers are unsupported")?
            .parse::<u64>()?
            .checked_sub(1)
            .ok_or("Invalid XLSX row number")?;
        let row_edits: Vec<_> = remaining
            .range((number, 0)..=(number, usize::MAX))
            .map(|(&at, v)| (at, v.clone()))
            .collect();
        if row_edits.is_empty() {
            continue;
        }
        let cells: Vec<_> = row
            .children()
            .filter(|n| n.tag_name().name() == "c")
            .map(|n| Ok((address(n.attribute("r").ok_or("Missing cell address")?)?, n)))
            .collect::<Result<_>>()?;
        for (at, value) in row_edits {
            if let Some((_, cell)) = cells.iter().find(|(pos, _)| *pos == at) {
                changes.push((cell.range(), xlsx_cell(Some(*cell), row, text, at, &value)?));
            } else {
                // Empty row elements need to be expanded to insert a new cell.
                if text[row.range()].trim_end().ends_with("/>") {
                    let body = remaining
                        .range((number, 0)..=(number, usize::MAX))
                        .map(|(&at, v)| xlsx_cell(None, row, text, at, v))
                        .collect::<Result<Vec<_>>>()?
                        .join("");
                    changes.push((
                        row.range(),
                        format!(
                            "{}{body}</{}>",
                            start_tag(row, text, &[], "")?,
                            qualified(row, "row")
                        ),
                    ));
                    remaining.retain(|&(r, _), _| r != number);
                    break;
                }
                let insert = cells.iter().find(|(pos, _)| pos.1 > at.1).map_or_else(
                    || row.range().start + text[row.range()].rfind("</").unwrap(),
                    |(_, n)| n.range().start,
                );
                changes.push((insert..insert, xlsx_cell(None, row, text, at, &value)?));
            }
            remaining.remove(&at);
        }
    }
    let mut missing_rows: BTreeMap<u64, String> = BTreeMap::new();
    for (&at, value) in &remaining {
        missing_rows
            .entry(at.0)
            .or_default()
            .push_str(&xlsx_cell(None, data, text, at, value)?);
    }
    for (row, body) in missing_rows {
        let tag = qualified(data, "row");
        let insert = data
            .children()
            .find(|n| {
                n.tag_name().name() == "row"
                    && n.attribute("r")
                        .and_then(|s| s.parse::<u64>().ok())
                        .is_some_and(|r| r > row + 1)
            })
            .map_or_else(
                || data.range().start + text[data.range()].rfind("</").unwrap(),
                |n| n.range().start,
            );
        changes.push((
            insert..insert,
            format!("<{tag} r=\"{}\">{body}</{tag}>", row + 1),
        ));
    }
    splice(text, changes)
}

fn ods_cell(cell: Node<'_, '_>, text: &str, value: &str) -> Result<String> {
    if cell.has_tag_name((TABLE, "covered-table-cell"))
        || cell.attribute((TABLE, "number-columns-spanned")).is_some()
        || cell.attribute((TABLE, "number-rows-spanned")).is_some()
        || cell.attribute((TABLE, "formula")).is_some()
    {
        return Err("Formula and merged cells are read-only in this version".into());
    }
    let remove = [
        (OFFICE, "value-type"),
        (OFFICE, "value"),
        (OFFICE, "string-value"),
        (OFFICE, "boolean-value"),
        (OFFICE, "date-value"),
        (OFFICE, "time-value"),
        (OFFICE, "currency"),
        (TABLE, "number-columns-repeated"),
        (
            "urn:org:documentfoundation:names:experimental:calc:xmlns:calcext:1.0",
            "value-type",
        ),
    ];
    // Local namespace declarations avoid depending on a particular input prefix.
    let namespace = if cell.lookup_namespace_uri(Some("office")) == Some(OFFICE) {
        String::new()
    } else {
        format!(" xmlns:office=\"{OFFICE}\"")
    };
    let start = start_tag(
        cell,
        text,
        &remove,
        &format!(
            "{namespace} office:value-type=\"string\" office:string-value=\"{}\"",
            escape(value).replace('\n', "&#10;").replace('\t', "&#9;")
        ),
    )?;
    let tail: String = cell
        .children()
        .filter(|n| n.is_element() && !n.has_tag_name((TEXT, "p")))
        .map(|n| &text[n.range()])
        .collect();
    let paragraphs = value
        .split('\n')
        .map(|line| {
            let content: String = line
                .chars()
                .map(|c| match c {
                    ' ' => "<text:s/>".into(),
                    '\t' => "<text:tab/>".into(),
                    c => escape(&c.to_string()),
                })
                .collect();
            format!("<text:p xmlns:text=\"{TEXT}\">{content}</text:p>")
        })
        .collect::<String>();
    Ok(format!(
        "{start}{paragraphs}{tail}</{}>",
        qualified(cell, "table-cell")
    ))
}

fn repeated(
    node: Node<'_, '_>,
    text: &str,
    attribute: &str,
    count: u64,
    body: Option<String>,
) -> Result<String> {
    if count == 0 {
        return Ok(String::new());
    }
    let prefix = node
        .lookup_prefix(TABLE)
        .ok_or("ODS table namespace needs a prefix")?;
    let extra = if count == 1 {
        String::new()
    } else {
        format!(" {prefix}:{attribute}=\"{count}\"")
    };
    let start = start_tag(node, text, &[(TABLE, attribute)], &extra)?;
    let body = match body {
        Some(s) => s,
        None => {
            let raw = &text[node.range()];
            if raw.trim_end().ends_with("/>") {
                String::new()
            } else {
                let last = node
                    .attributes()
                    .next_back()
                    .map_or(node.range().start, |a| a.range().end);
                let from = last + text[last..].find('>').unwrap() + 1;
                text[from..node.range().start + raw.rfind("</").unwrap()].to_owned()
            }
        }
    };
    Ok(format!(
        "{start}{body}</{}>",
        qualified(node, node.tag_name().name())
    ))
}

fn patch_ods(doc: &Document<'_>, text: &str, name: &str, edits: &Edits) -> Result<String> {
    let table = doc
        .descendants()
        .find(|n| n.has_tag_name((TABLE, "table")) && n.attribute((TABLE, "name")) == Some(name))
        .ok_or("Missing ODS sheet")?;
    let mut changes = Vec::new();
    let mut first_row = 0u64;
    let mut applied = 0;
    for row in table
        .descendants()
        .filter(|n| n.has_tag_name((TABLE, "table-row")))
    {
        let next_row = first_row
            .checked_add(repeat(row, "number-rows-repeated")?)
            .ok_or("ODS row overflow")?;
        let touched: BTreeSet<_> = edits
            .range((first_row, 0)..(next_row, 0))
            .map(|(&(r, _), _)| r)
            .collect();
        let mut replacement = String::new();
        let mut cursor = first_row;
        for r in touched {
            replacement.push_str(&repeated(
                row,
                text,
                "number-rows-repeated",
                r - cursor,
                None,
            )?);
            let mut cell_changes = Vec::new();
            let mut first_col = 0usize;
            for cell in row.children().filter(|n| {
                n.has_tag_name((TABLE, "table-cell"))
                    || n.has_tag_name((TABLE, "covered-table-cell"))
            }) {
                let next_col = first_col
                    .checked_add(repeat(cell, "number-columns-repeated")? as usize)
                    .ok_or("ODS column overflow")?;
                let touched: Vec<_> = edits.range((r, first_col)..(r, next_col)).collect();
                if !touched.is_empty() {
                    let mut cells = String::new();
                    let mut cursor = first_col;
                    for (&(_, col), value) in touched {
                        cells.push_str(&repeated(
                            cell,
                            text,
                            "number-columns-repeated",
                            (col - cursor) as u64,
                            None,
                        )?);
                        cells.push_str(&ods_cell(cell, text, value)?);
                        cursor = col + 1;
                        applied += 1;
                    }
                    cells.push_str(&repeated(
                        cell,
                        text,
                        "number-columns-repeated",
                        (next_col - cursor) as u64,
                        None,
                    )?);
                    let range = cell.range();
                    cell_changes.push((
                        range.start - row.range().start..range.end - row.range().start,
                        cells,
                    ));
                }
                first_col = next_col;
            }
            // ODS permits omitted trailing empty cells. Extend only inside the imported rectangle.
            let trailing: Vec<_> = edits.range((r, first_col)..=(r, usize::MAX)).collect();
            let mut extra = String::new();
            let template = format!("<table:table-cell xmlns:table=\"{TABLE}\"/>");
            let template_doc = xml(&template)?;
            for (&(_, col), value) in trailing {
                if col > first_col {
                    extra.push_str(&format!("<table:table-cell xmlns:table=\"{TABLE}\" table:number-columns-repeated=\"{}\"/>", col - first_col));
                }
                extra.push_str(&ods_cell(template_doc.root_element(), &template, value)?);
                first_col = col + 1;
                applied += 1;
            }
            let mut row_text = splice(&text[row.range()], cell_changes)?;
            if !extra.is_empty() {
                if row_text.trim_end().ends_with("/>") {
                    row_text = repeated(row, text, "number-rows-repeated", 1, Some(extra))?;
                    replacement.push_str(&row_text);
                    cursor = r + 1;
                    continue;
                }
                let insert = row_text.rfind("</").ok_or("Missing ODS row end")?;
                row_text.insert_str(insert, &extra);
            }
            // Only the row repetition attribute changes; namespaces stay in their original scope.
            let count_attr = row.attribute_node((TABLE, "number-rows-repeated"));
            let row_text = if let Some(a) = count_attr {
                let range = a.range();
                splice(
                    &row_text,
                    vec![(
                        range.start - row.range().start..range.end - row.range().start,
                        String::new(),
                    )],
                )?
            } else {
                row_text
            };
            replacement.push_str(&row_text);
            cursor = r + 1;
        }
        if cursor != first_row {
            replacement.push_str(&repeated(
                row,
                text,
                "number-rows-repeated",
                next_row - cursor,
                None,
            )?);
            changes.push((row.range(), replacement));
        }
        first_row = next_row;
    }
    if applied != edits.len() {
        return Err("Some ODS target cells were not found; output was not published".into());
    }
    splice(text, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error<T: std::fmt::Debug>(result: Result<T>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn addresses_strip_anchors_and_keep_workbook_bounds_and_wording() {
        for (text, at) in [
            ("A1", (0, 0)),
            ("$A$1", (0, 0)),
            ("a$65", (64, 0)),
            ("$$B$2$", (1, 1)),
            ("XFD1048576", (1_048_575, 16_383)),
        ] {
            assert_eq!(address(text).unwrap(), at, "{text}");
        }
        for (text, message) in [
            ("", "Invalid workbook cell address"),
            ("A", "Invalid workbook cell address"),
            ("1", "Invalid workbook cell address"),
            ("A1x", "Invalid workbook cell address"),
            ("Õ1", "Invalid workbook cell address"),
            ("ZZZZZZZZZZZZZZZZZZZZZZZZ1", "Workbook column overflow"),
            (
                "A18446744073709551616",
                "number too large to fit in target type",
            ),
            ("A0", "Workbook rows start at 1"),
            ("$A$0", "Workbook rows start at 1"),
            ("XFE1", "Workbook cell exceeds supported sheet dimensions"),
            (
                "A1048577",
                "Workbook cell exceeds supported sheet dimensions",
            ),
        ] {
            assert_eq!(error(address(text)), message, "{text}");
        }
    }

    #[test]
    fn cell_ranges_use_workbook_wording() {
        assert_eq!(cell_range("A1:$D$5").unwrap(), ((0, 0), (4, 3)));
        assert_eq!(cell_range("B2").unwrap(), ((1, 1), (1, 1)));
        assert_eq!(error(cell_range("B2:A1")), "Invalid workbook cell range");
        assert_eq!(error(cell_range("A1:B")), "Invalid workbook cell address");
        assert_eq!(error(cell_range("A0:B1")), "Workbook rows start at 1");
        assert_eq!(
            error(cell_range("A1:XFE1")),
            "Workbook cell exceeds supported sheet dimensions"
        );
    }
}
