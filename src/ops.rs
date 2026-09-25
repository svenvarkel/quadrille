//! Headless operations for the CLI and the MCP server: typed arguments in, typed results
//! out. Each result's `to_json` is the one definition of its JSON shape. No argv, stdout
//! or process exit here; which operations to run, and in what order, is the caller's.
use crate::{
    FindQuery, FindResult, Progress, Result, Sheet,
    a1::{self, Address, Range, cell_name, column_name},
    parse_sort,
};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// Largest read, in cells and in records.
pub const READ_CELLS: usize = 100_000;
pub const READ_ROWS: u64 = 10_000;
/// Find matches per call: the default and the maximum.
pub const FIND_LIMIT: usize = 100;
pub const FIND_LIMIT_MAX: usize = 10_000;
/// Rejection of a limit outside `1..=FIND_LIMIT_MAX`; the CLI prefixes its option name.
pub const FIND_LIMIT_ERROR: &str = "limit must be a whole number from 1 to 10,000";

/// The effective find limit: the default when absent, an error when out of range.
pub fn find_limit(limit: Option<usize>) -> Result<usize> {
    let limit = limit.unwrap_or(FIND_LIMIT);
    if (1..=FIND_LIMIT_MAX).contains(&limit) {
        Ok(limit)
    } else {
        Err(FIND_LIMIT_ERROR.into())
    }
}

/// One cell edit: zero-based address and new text.
pub type Edit = (Address, String);

/// How much of the index an operation needs before it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// At least this many records, e.g. `range.last.0 + 1` for a read.
    Rows(u64),
    /// The complete scan, so that an encoding error cannot publish a partial result.
    All,
}

/// Block until `wait` is satisfied; an indexing error is returned as soon as it is seen.
pub fn wait_for_index(sheet: &Sheet, wait: Wait) -> Result<()> {
    loop {
        let p = sheet.progress();
        if let Some(error) = p.error {
            return Err(error.into());
        }
        if p.done || matches!(wait, Wait::Rows(rows) if p.rows >= rows) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// What was opened. Every document names it the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    /// Workbook sheet name and format (`XLSX` or `ODS`); `None` for CSV.
    pub workbook: Option<(String, String)>,
}

impl Source {
    pub fn of(sheet: &Sheet) -> Self {
        Self {
            path: sheet.path.to_string_lossy().into_owned(),
            workbook: sheet
                .sheet_name()
                .map(|name| (name.to_owned(), sheet.format().to_owned())),
        }
    }

    /// `sheet` and `format` for workbooks: the one place these keys are written.
    fn describe(&self, doc: &mut Value) {
        if let Some((name, format)) = &self.workbook {
            doc["sheet"] = json!(name);
            doc["format"] = json!(format);
        }
    }

    pub fn to_json(&self) -> Value {
        let mut doc = json!({"source": self.path});
        self.describe(&mut doc);
        doc
    }
}

/// Workbook sheet names, for `--sheets`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheets {
    /// The path as given, not canonicalized.
    pub source: PathBuf,
    pub names: Vec<String>,
}

pub fn sheets(path: &Path) -> Result<Sheets> {
    Ok(Sheets {
        source: path.to_owned(),
        names: Sheet::sheet_names(path)?,
    })
}

impl Sheets {
    pub fn to_json(&self) -> Value {
        json!({"source": self.source, "sheets": self.names})
    }
}

/// Scan counts. Run after `wait_for_index(sheet, Wait::All)`.
#[derive(Clone, Debug)]
pub struct Check {
    pub progress: Progress,
    pub elapsed: Duration,
    pub source: Source,
    /// Workbook archive size; the counts describe its extracted sheet.
    pub source_bytes: Option<u64>,
}

/// `started` is when opening began, so that the timing includes it.
pub fn check(sheet: &Sheet, started: Instant) -> Result<Check> {
    let (progress, elapsed) = (sheet.progress(), started.elapsed());
    let source_bytes = match sheet.sheet_name() {
        Some(_) => Some(fs::metadata(&sheet.path)?.len()),
        None => None,
    };
    Ok(Check {
        progress,
        elapsed,
        source: Source::of(sheet),
        source_bytes,
    })
}

impl Check {
    pub fn to_json(&self) -> Value {
        let p = &self.progress;
        let mut doc = json!({"records": p.rows, "bytes": p.total_bytes, "index_bytes": p.index_bytes, "elapsed_seconds": self.elapsed.as_secs_f64()});
        self.source.describe(&mut doc);
        if let Some(bytes) = self.source_bytes {
            doc["source_bytes"] = json!(bytes);
        }
        doc
    }
}

/// `[{"cell": "B7", "value": "text"}, ...]`: an `--apply` patch or MCP `set` edits.
pub fn parse_edits(patch: &Value) -> Result<Vec<Edit>> {
    let mut edits = Vec::new();
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
        edits.push((a1::parse_cell(cell)?, value.to_owned()));
    }
    Ok(edits)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub cell: Address,
    pub before: String,
    pub after: String,
}

/// Net changes by cell; edits that restore the stored value are omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes(pub Vec<Change>);

/// Apply edits in order, stopping at the first refused one. `before` is the stored value.
pub fn apply_edits(sheet: &mut Sheet, edits: Vec<Edit>) -> Result<Changes> {
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
    Ok(Changes(
        changes
            .into_iter()
            .filter(|(_, (before, after))| before != after)
            .map(|(cell, (before, after))| Change {
                cell,
                before,
                after,
            })
            .collect(),
    ))
}

impl Changes {
    pub fn to_json(&self) -> Value {
        self.0
            .iter()
            .map(|c| json!({"cell": cell_name(c.cell), "before": c.before, "after": c.after}))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sorted {
    /// The specification as given, such as `B,-D:n`.
    pub columns: String,
    pub header: bool,
}

/// Sort the view; later reads and finds use sorted-view coordinates, edits source ones.
pub fn sort(sheet: &mut Sheet, spec: &str, header: bool) -> Result<Sorted> {
    let job = sheet.start_sort(parse_sort(spec)?, header)?;
    let order = job.result.recv()??;
    sheet.apply_sort(order)?;
    Ok(Sorted {
        columns: spec.to_owned(),
        header,
    })
}

impl Sorted {
    pub fn to_json(&self) -> Value {
        json!({"columns": self.columns, "header": self.header, "changes_coordinates": "source", "read_coordinates": "sorted_view"})
    }
}

/// A read rectangle within the read limits.
pub fn read_range(text: &str) -> Result<Range> {
    let range = a1::parse_range(text)?;
    if range.rows() > READ_ROWS
        || (range.rows() as usize)
            .checked_mul(range.columns())
            .is_none_or(|n| n > READ_CELLS)
    {
        return Err("Read at most 100,000 cells and 10,000 rows at a time; request large datasets in chunks".into());
    }
    Ok(range)
}

/// Refuse a range below the indexed records. Run after `wait_for_index`.
pub fn in_bounds(sheet: &Sheet, range: &Range) -> Result<()> {
    if range.last.0 >= sheet.progress().rows {
        return Err("Requested range extends beyond the last row".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub range: Range,
    /// `None` where a ragged CSV record has no such field.
    pub rows: Vec<Vec<Option<String>>>,
    /// Formula text by cell, for workbooks only.
    pub formulas: Option<BTreeMap<Address, String>>,
}

/// Values as shown: edits applied, cached formula results. Run after `wait_for_index`.
pub fn read(sheet: &mut Sheet, range: Range) -> Result<Read> {
    in_bounds(sheet, &range)?;
    let records = sheet.window(range.first.0, range.rows() as usize)?;
    let rows = records
        .iter()
        .zip(range.first.0..)
        .map(|(record, row)| {
            (range.first.1..=range.last.1)
                .map(|col| sheet.cell_value(row, col, record).map(str::to_owned))
                .collect()
        })
        .collect();
    let formulas = sheet.sheet_name().map(|_| {
        (range.first.0..=range.last.0)
            .flat_map(|row| (range.first.1..=range.last.1).map(move |col| (row, col)))
            .filter_map(|at| Some((at, sheet.formula(at.0, at.1)?.to_owned())))
            .collect()
    });
    Ok(Read {
        range,
        rows,
        formulas,
    })
}

impl Read {
    /// `rows`, `range` and, for workbooks, `formulas`.
    pub fn to_json(&self) -> Value {
        let mut doc = json!({"rows": self.rows, "range": self.range.to_string()});
        if let Some(formulas) = &self.formulas {
            doc["formulas"] = formulas
                .iter()
                .map(|(&at, formula)| (cell_name(at), json!(formula)))
                .collect::<Map<_, _>>()
                .into();
        }
        doc
    }
}

/// A find page with its query, ready to render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub source: Source,
    pub query: FindQuery,
    pub from: Address,
    pub limit: usize,
    pub result: FindResult,
    /// The sort in effect: matches are then sorted-view coordinates.
    pub sort: Option<Sorted>,
    /// Edits previewed with `--dry-run`.
    pub preview: Option<Changes>,
}

/// Find from `from` (default A1), at most `limit` (default 100) matches. Needs no index.
pub fn find(
    sheet: &Sheet,
    query: FindQuery,
    from: Option<Address>,
    limit: Option<usize>,
) -> Result<Found> {
    let (from, limit) = (from.unwrap_or((0, 0)), find_limit(limit)?);
    let result = sheet.find(&query, from, limit)?;
    Ok(Found {
        source: Source::of(sheet),
        query,
        from,
        limit,
        result,
        sort: None,
        preview: None,
    })
}

impl Found {
    pub fn to_json(&self) -> Value {
        let q = &self.query;
        let columns = q
            .columns
            .as_ref()
            .map(|columns| columns.iter().copied().map(column_name).collect::<Vec<_>>());
        let matches: Vec<Value> = self
            .result
            .matches
            .iter()
            .map(|&(at, ref value)| json!({"cell": cell_name(at), "value": value}))
            .collect();
        let mut doc = self.source.to_json();
        doc["find"] = json!({"text": q.text, "columns": columns, "exact": q.exact, "ignore_case": q.ignore_case, "from": cell_name(self.from), "limit": self.limit});
        doc["coordinates"] = json!(if self.sort.is_some() {
            "sorted_view"
        } else {
            "source"
        });
        doc["matches"] = json!(matches);
        doc["next"] = json!(self.result.next.map(cell_name));
        doc["records_scanned"] = json!(self.result.records_scanned);
        if let Some(sort) = &self.sort {
            doc["sort"] = sort.to_json();
        }
        if let Some(changes) = &self.preview {
            doc["changes"] = changes.to_json();
            doc["dry_run"] = json!(true);
        }
        doc
    }
}

pub fn save(sheet: &Sheet, destination: &Path) -> Result<PathBuf> {
    Ok(sheet.save_as(destination)?.result.recv()??)
}

/// The edit / read / save document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub source: Source,
    pub changes: Changes,
    pub dry_run: bool,
    pub sort: Option<Sorted>,
    pub read: Option<Read>,
    pub output: Option<PathBuf>,
}

impl Report {
    pub fn to_json(&self) -> Value {
        let mut doc = self.source.to_json();
        doc["changes"] = self.changes.to_json();
        doc["dry_run"] = json!(self.dry_run);
        if self.source.workbook.is_some() {
            doc["formula_results"] = json!(
                "existing results are cached; new formulas are calculated when the saved file opens"
            );
            doc["edit_type"] = json!("text; XLSX values beginning with = are formulas");
        }
        if let Some(sort) = &self.sort {
            doc["sort"] = sort.to_json();
        }
        if let Some(Value::Object(fields)) = self.read.as_ref().map(Read::to_json) {
            doc.as_object_mut().unwrap().extend(fields);
        }
        if let Some(output) = &self.output {
            doc["output"] = json!(output.to_string_lossy());
        }
        doc
    }
}
