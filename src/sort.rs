//! Sorting retains only keys and record locations, never the entire CSV.
use super::*;
use rust_decimal::Decimal;
use std::{
    cmp::Ordering as Cmp,
    io::{Seek, SeekFrom},
};

// ponytail: bounded in-memory keys; add external merge sorting if this budget is
// insufficient. Refuse the operation without changing the current view.
const SORT_BUDGET: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub column: usize,
    pub descending: bool,
    pub numeric: bool,
}

pub fn parse_sort(text: &str) -> Result<Vec<SortKey>> {
    let mut keys = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (descending, part) = if let Some(p) = part.strip_prefix('-') {
            (true, p)
        } else {
            (false, part.strip_prefix('+').unwrap_or(part))
        };
        let (part, numeric) = if let Some(p) = part.strip_suffix(":n") {
            (p, true)
        } else {
            (part, false)
        };
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(
                "Use column letters, commas, - for descending and :n for numbers; e.g. B,-D:n"
                    .into(),
            );
        }
        let mut column = 0usize;
        for b in part.bytes() {
            column = column
                .checked_mul(26)
                .and_then(|c| c.checked_add((b.to_ascii_uppercase() - b'A' + 1) as usize))
                .ok_or("Column is too large")?;
        }
        if keys.iter().any(|k: &SortKey| k.column == column - 1) {
            return Err("A sort column may only appear once".into());
        }
        keys.push(SortKey {
            column: column - 1,
            descending,
            numeric,
        });
    }
    Ok(keys)
}

#[derive(Clone, Copy)]
pub(crate) struct RecordRef {
    pub row: u64,
    pub start: u64,
    end: u64,
}

pub struct SortOrder {
    pub(crate) rows: Vec<RecordRef>,
    pub keys: Vec<SortKey>,
    pub header: bool,
    revision: u64,
}

pub struct SortJob {
    pub rows: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub result: Receiver<std::result::Result<SortOrder, String>>,
}

impl Drop for SortJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
enum Value {
    Text(String),
    Number(Decimal),
    Empty,
}
struct Entry {
    record: RecordRef,
    values: Vec<Value>,
}

impl Sheet {
    pub fn start_sort(&self, keys: Vec<SortKey>, header: bool) -> Result<SortJob> {
        let p = self.progress();
        if let Some(error) = p.error {
            return Err(error.into());
        }
        if !p.done {
            return Err("Wait for indexing to finish before sorting".into());
        }
        if keys.is_empty() {
            return Err("Choose at least one sort column".into());
        }
        self.check_source()?;
        let minimum = size_of::<Entry>()
            .checked_add(
                keys.len()
                    .checked_mul(size_of::<Value>())
                    .ok_or("Too many sort columns")?,
            )
            .ok_or("Too many sort columns")?;
        let count = usize::try_from(p.rows)?;
        if count.checked_mul(minimum).is_none_or(|n| n > SORT_BUDGET) {
            return Err("Sort keys exceed the 512 MiB budget; try fewer columns. Disk-backed sorting is not implemented yet".into());
        }
        let (path, stamp, delimiter, edits, revision) = (
            self.data_path().to_path_buf(),
            self.stamp.clone(),
            self.delimiter,
            self.edits.clone(),
            self.revision,
        );
        let rows = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (progress, stop) = (rows.clone(), cancel.clone());
        let (sender, result) = mpsc::channel();
        let workbook = self.workbook.clone();
        thread::spawn(move || {
            let result = (|| -> Result<SortOrder> {
                if let Some(w) = &workbook { w.check_source()?; }
                stamp.check(&path)?;
                let mut parser = reader(File::open(&path)?, delimiter);
                let mut record = csv::StringRecord::new();
                let mut entries = Vec::with_capacity(count);
                let mut used = count * minimum;
                for row in 0..p.rows {
                    if stop.load(Ordering::Relaxed) { return Err("Sort cancelled".into()); }
                    let start = parser.position().byte();
                    if !parser.read_record(&mut record)? { return Err("Source ended unexpectedly during sort".into()); }
                    let mut values = Vec::with_capacity(keys.len());
                    for key in &keys {
                        let text = edits.get(&(row, key.column)).map(String::as_str).or_else(|| record.get(key.column));
                        // With a header, a typo must not silently sort an absent column.
                        if row == 0 && header && text.is_none() { return Err(format!("Sort column {} is outside the header", key.column + 1).into()); }
                        let value = match text {
                            _ if header && row == 0 => Value::Empty,
                            None | Some("") => Value::Empty,
                            Some(text) if key.numeric => {
                                let number = Decimal::from_str_exact(text.trim()).map_err(|_| format!("Record {}, column {} is not an exact decimal (maximum scale 28); choose text sorting for mixed values", row + 1, key.column + 1))?;
                                Value::Number(number)
                            }
                            Some(text) => {
                                // Include a conservative allocator allowance for each string.
                                used = used.saturating_add(text.len().saturating_add(32));
                                if used > SORT_BUDGET { return Err("Sort keys exceed the 512 MiB budget; current view was kept".into()); }
                                Value::Text(text.to_owned())
                            }
                        };
                        values.push(value);
                    }
                    entries.push(Entry { record: RecordRef { row, start, end: parser.position().byte() }, values });
                    if row % 256 == 0 { progress.store(row, Ordering::Relaxed); }
                }
                progress.store(p.rows, Ordering::Relaxed);
                stamp.check(&path)?;
                let first = usize::from(header && !entries.is_empty());
                entries[first..].sort_unstable_by(|a, b| {
                    for (i, key) in keys.iter().enumerate() {
                        let (a, b) = (&a.values[i], &b.values[i]);
                        let ordering = match (a, b) {
                            (Value::Empty, Value::Empty) => Cmp::Equal,
                            (Value::Empty, _) => Cmp::Greater,
                            (_, Value::Empty) => Cmp::Less,
                            _ if key.descending => b.cmp(a),
                            _ => a.cmp(b),
                        };
                        if ordering != Cmp::Equal { return ordering; }
                    }
                    a.record.row.cmp(&b.record.row)
                });
                if stop.load(Ordering::Relaxed) { return Err("Sort cancelled".into()); }
                // Consume and release the key allocations as the compact order is produced.
                let rows = entries.into_iter().map(|e| e.record).collect();
                stamp.check(&path)?;
                Ok(SortOrder { rows, keys, header, revision })
            })().and_then(|order| {
                if let Some(w) = &workbook { w.check_source()?; }
                Ok(order)
            }).map_err(|e| e.to_string());
            let _ = sender.send(result);
        });
        Ok(SortJob {
            rows,
            cancel,
            result,
        })
    }

    pub fn apply_sort(&mut self, order: SortOrder) -> Result<()> {
        self.check_source()?;
        if order.revision != self.revision {
            return Err("Edits changed while sorting; run the sort again".into());
        }
        self.order = Some(Arc::new(order));
        Ok(())
    }

    pub fn clear_sort(&mut self) {
        self.order = None;
    }
    pub fn sort_order(&self) -> Option<&SortOrder> {
        self.order.as_deref()
    }

    pub fn source_row(&self, view_row: u64) -> u64 {
        self.order
            .as_ref()
            .and_then(|o| o.rows.get(view_row as usize))
            .map_or(view_row, |r| r.row)
    }
}

pub(crate) fn write_sorted(
    input: &mut File,
    output: &mut impl Write,
    order: &SortOrder,
    edits: &Edits,
    delimiter: u8,
    progress: &AtomicU64,
) -> Result<()> {
    let mut bom = [0; 3];
    if input.read(&mut bom)? == 3 && bom == [0xef, 0xbb, 0xbf] {
        output.write_all(&bom)?;
    }
    let mut raw = Vec::new();
    for reference in &order.rows {
        input.seek(SeekFrom::Start(reference.start))?;
        raw.resize((reference.end - reference.start).try_into()?, 0);
        input.read_exact(&mut raw)?;
        let changed: Vec<_> = edits
            .range((reference.row, 0)..=(reference.row, usize::MAX))
            .collect();
        if changed.is_empty() {
            let bom = usize::from(reference.start == 0 && raw.starts_with(b"\xef\xbb\xbf")) * 3;
            let body = &raw[bom..];
            let start = body
                .iter()
                .take_while(|&&b| b == b'\r' || b == b'\n')
                .count();
            let end = body.len()
                - body
                    .iter()
                    .rev()
                    .take_while(|&&b| b == b'\r' || b == b'\n')
                    .count();
            output.write_all(&body[start..end])?;
            output.write_all(b"\n")?;
        } else {
            let mut parser = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .delimiter(delimiter)
                .from_reader(raw.as_slice());
            let mut record = csv::StringRecord::new();
            if !parser.read_record(&mut record)? {
                return Err("Missing record during sorted save".into());
            }
            let mut values: Vec<_> = record.iter().map(str::to_owned).collect();
            for (&(_, column), value) in changed {
                *values
                    .get_mut(column)
                    .ok_or("Missing edited column during save")? = value.clone();
            }
            let mut writer = csv::WriterBuilder::new()
                .delimiter(delimiter)
                .from_writer(&mut *output);
            writer.write_record(&values)?;
            writer.flush()?;
        }
        progress.fetch_add(reference.end - reference.start, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn ready(sheet: &Sheet) {
        let start = Instant::now();
        while !sheet.progress().done {
            assert!(start.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn sorted(sheet: &mut Sheet, spec: &str, header: bool) {
        let job = sheet.start_sort(parse_sort(spec).unwrap(), header).unwrap();
        let order = job
            .result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        sheet.apply_sort(order).unwrap();
    }

    #[test]
    fn multi_column_numeric_sort_keeps_edits_attached_and_saves_visible_order() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.csv");
        let original = "\u{feff}group,amount,note\r\nb,2,\"line 1\nline 2\"\r\na,10,ten\r\na,2,two\r\na,2,second two\r\na,,empty\r\nb,9007199254740993,big\r\nb,9007199254740992,smaller";
        fs::write(&source, original).unwrap();
        let mut sheet = Sheet::open(&source, b',').unwrap();
        ready(&sheet);
        sheet.set(3, 2, "before sort".into()).unwrap();
        sorted(&mut sheet, "A,B:n", true);
        let rows = sheet.window(0, 8).unwrap();
        assert_eq!(
            rows.iter().map(|r| &r[1]).collect::<Vec<_>>(),
            [
                "amount",
                "2",
                "2",
                "10",
                "",
                "2",
                "9007199254740992",
                "9007199254740993"
            ]
        );
        assert_eq!(sheet.value(1, 2, &rows[1][2]), "before sort");
        sheet.set(1, 2, "after sort".into()).unwrap();
        let output = dir.path().join("sorted.csv");
        sheet
            .save_as(&output)
            .unwrap()
            .result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let mut saved = reader(File::open(&output).unwrap(), b',');
        let values: Vec<_> = saved.records().map(|r| r.unwrap()).collect();
        assert_eq!(values.len(), 8);
        assert_eq!(&values[1][2], "after sort");
        assert_eq!(&values[5][2], "line 1\nline 2");
        assert!(fs::read(&output).unwrap().starts_with(b"\xef\xbb\xbf"));
        assert!(sheet.undo());
        assert_eq!(sheet.value(1, 2, &rows[1][2]), "before sort");
        sheet.clear_sort();
        assert_eq!(sheet.value(3, 2, "two"), "before sort");
        assert!(sheet.undo());
        let copy = dir.path().join("copy.csv");
        sheet
            .save_as(&copy)
            .unwrap()
            .result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(fs::read_to_string(copy).unwrap(), original);
        assert_eq!(fs::read_to_string(source).unwrap(), original);
    }

    #[test]
    fn sort_failures_keep_order_and_do_not_round_large_numbers() {
        for invalid in [
            "",
            "A1",
            "A,,B",
            "A,A",
            "A:x",
            "-",
            "AAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(parse_sort(invalid).is_err(), "{invalid}");
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.csv");
        fs::write(&source, "3,x\n2,y\n1,z").unwrap();
        let mut sheet = Sheet::open(&source, b',').unwrap();
        ready(&sheet);
        sorted(&mut sheet, "A:n", false);
        assert_eq!(&sheet.window(0, 1).unwrap()[0][0], "1");
        sorted(&mut sheet, "-A:n", false);
        assert_eq!(&sheet.window(0, 1).unwrap()[0][0], "3");
        let bad = sheet.start_sort(parse_sort("B:n").unwrap(), false).unwrap();
        assert!(
            bad.result
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .is_err()
        );
        assert_eq!(&sheet.window(0, 1).unwrap()[0][0], "3");
        let job = sheet.start_sort(parse_sort("A").unwrap(), false).unwrap();
        let stale = job
            .result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        sheet.set(0, 0, "4".into()).unwrap();
        assert!(sheet.apply_sort(stale).is_err());
        sheet
            .set(1, 0, "0.00000000000000000000000000001".into())
            .unwrap();
        let exact = sheet.start_sort(parse_sort("A:n").unwrap(), false).unwrap();
        assert!(
            exact
                .result
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .is_err()
        );
    }
}
