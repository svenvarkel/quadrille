//! A bounded-memory CSV view with sparse indexing and copy-on-save edits.
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::SystemTime,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
mod sort;
pub use sort::{SortJob, SortKey, SortOrder, parse_sort};

const STRIDE: u64 = 64;
type Edits = BTreeMap<(u64, usize), String>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Stamp {
    fn new(meta: Metadata) -> io::Result<Self> {
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        })
    }

    fn check(&self, path: &Path) -> Result<()> {
        if *self != Self::new(fs::metadata(path)?)? {
            return Err(
                "Source file changed outside Quadrille; reopen it before continuing".into(),
            );
        }
        Ok(())
    }
}

#[derive(Default)]
struct Index {
    offsets: Vec<u64>,
    rows: u64,
    bytes: u64,
    done: bool,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub rows: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub index_bytes: usize,
    pub done: bool,
    pub error: Option<String>,
}

pub struct Sheet {
    pub path: PathBuf,
    delimiter: u8,
    stamp: Stamp,
    reader: csv::Reader<File>,
    index: Arc<Mutex<Index>>,
    stop: Arc<AtomicBool>,
    edits: Edits,
    undo: Vec<((u64, usize), Option<String>)>,
    order: Option<Arc<SortOrder>>,
    revision: u64,
}

pub struct SaveJob {
    pub bytes: Arc<AtomicU64>,
    pub result: Receiver<std::result::Result<PathBuf, String>>,
}

fn reader(file: File, delimiter: u8) -> csv::Reader<File> {
    csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .buffer_capacity(256 * 1024)
        .from_reader(file)
}

impl Sheet {
    pub fn open(path: &Path, delimiter: u8) -> Result<Self> {
        if !delimiter.is_ascii() || matches!(delimiter, b'\r' | b'\n' | b'"' | 0) {
            return Err(
                "Delimiter must be a single ASCII byte other than a quote, CR, LF or NUL".into(),
            );
        }
        let path = fs::canonicalize(path)?;
        let file = File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err("Open a regular CSV file (pipes are not supported)".into());
        }
        let stamp = Stamp::new(file.metadata()?)?;
        let scan_file = File::open(&path)?;
        stamp.check(&path)?;
        let index = Arc::new(Mutex::new(Index::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (scan_index, scan_stop, scan_path, scan_stamp) =
            (index.clone(), stop.clone(), path.clone(), stamp.clone());
        thread::spawn(move || {
            let result = scan(scan_file, delimiter, &scan_index, &scan_stop)
                .and_then(|()| scan_stamp.check(&scan_path));
            let mut index = scan_index.lock().unwrap();
            if let Err(error) = result {
                index.error = Some(error.to_string());
            }
            index.done = true;
        });
        Ok(Self {
            path,
            delimiter,
            stamp,
            reader: reader(file, delimiter),
            index,
            stop,
            edits: BTreeMap::new(),
            undo: Vec::new(),
            order: None,
            revision: 0,
        })
    }

    pub fn progress(&self) -> Progress {
        let index = self.index.lock().unwrap();
        Progress {
            rows: index.rows,
            bytes: index.bytes,
            total_bytes: self.stamp.len,
            index_bytes: index.offsets.len() * size_of::<u64>(),
            done: index.done,
            error: index.error.clone(),
        }
    }

    /// Zero-based CSV records, including any header. Quoted newlines are not rows.
    pub fn window(&mut self, start: u64, count: usize) -> Result<Vec<csv::StringRecord>> {
        self.stamp.check(&self.path)?;
        if let Some(order) = &self.order {
            let mut rows = Vec::new();
            for reference in order.rows.iter().skip(start as usize).take(count) {
                let mut position = csv::Position::new();
                position.set_byte(reference.start).set_record(reference.row);
                self.reader.seek(position)?;
                let mut row = csv::StringRecord::new();
                if !self.reader.read_record(&mut row)? {
                    return Err("Source ended unexpectedly; reopen it".into());
                }
                rows.push(row);
            }
            return Ok(rows);
        }
        let (offset, available) = {
            let index = self.index.lock().unwrap();
            if start >= index.rows {
                return Ok(Vec::new());
            }
            (index.offsets[(start / STRIDE) as usize], index.rows - start)
        };
        seek_row(&mut self.reader, start, offset)?;
        let mut rows = Vec::new();
        for _ in 0..count.min(available as usize) {
            let mut row = csv::StringRecord::new();
            if !self.reader.read_record(&mut row)? {
                return Err("Source ended unexpectedly; reopen it".into());
            }
            rows.push(row);
        }
        Ok(rows)
    }

    pub fn value<'a>(&'a self, row: u64, col: usize, original: &'a str) -> &'a str {
        self.edits
            .get(&(self.source_row(row), col))
            .map_or(original, String::as_str)
    }

    pub fn is_edited(&self, row: u64, col: usize) -> bool {
        self.edits.contains_key(&(self.source_row(row), col))
    }

    pub fn edit_count(&self) -> usize {
        self.edits.len()
    }

    pub fn set(&mut self, row: u64, col: usize, value: String) -> Result<()> {
        if let Some(error) = self.progress().error {
            return Err(error.into());
        }
        let rows = self.window(row, 1)?;
        let original = rows
            .first()
            .and_then(|r| r.get(col))
            .ok_or("No cell at this position")?;
        if self.value(row, col, original) == value {
            return Ok(());
        }
        let key = (self.source_row(row), col);
        self.revision += 1;
        self.undo.push((key, self.edits.get(&key).cloned()));
        if value == original {
            self.edits.remove(&key);
        } else {
            self.edits.insert(key, value);
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some((key, previous)) = self.undo.pop() else {
            return false;
        };
        self.revision += 1;
        if let Some(value) = previous {
            self.edits.insert(key, value);
        } else {
            self.edits.remove(&key);
        }
        true
    }

    /// Save a snapshot to a new destination. Existing paths are never replaced.
    pub fn save_as(&self, destination: &Path) -> Result<SaveJob> {
        let index = self.index.lock().unwrap();
        if let Some(error) = &index.error {
            return Err(error.clone().into());
        }
        if !index.done {
            return Err("Wait for indexing to finish before saving".into());
        }
        self.stamp.check(&self.path)?;
        if destination.try_exists()? {
            return Err("Destination already exists; choose a new filename".into());
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temp = tempfile::NamedTempFile::new_in(parent)?;
        let (path, stamp, delimiter, offsets, edits, destination) = (
            self.path.clone(),
            self.stamp.clone(),
            self.delimiter,
            index.offsets.clone(),
            self.edits.clone(),
            destination.to_path_buf(),
        );
        let bytes = Arc::new(AtomicU64::new(0));
        let progress = bytes.clone();
        let order = self.order.clone();
        let (sender, result) = mpsc::channel();
        thread::spawn(move || {
            let result = save(
                &path,
                &stamp,
                delimiter,
                &offsets,
                &edits,
                &destination,
                temp,
                &progress,
                order.as_deref(),
            )
            .map(|()| destination)
            .map_err(|e| e.to_string());
            let _ = sender.send(result);
        });
        Ok(SaveJob { bytes, result })
    }
}

impl Drop for Sheet {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn scan(file: File, delimiter: u8, index: &Mutex<Index>, stop: &AtomicBool) -> Result<()> {
    let mut reader = reader(file, delimiter);
    let mut record = csv::StringRecord::new();
    let mut rows = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let offset = reader.position().byte();
        if !reader.read_record(&mut record)? {
            let mut index = index.lock().unwrap();
            index.rows = rows;
            index.bytes = reader.position().byte();
            return Ok(());
        }
        if rows % STRIDE == 0 {
            index.lock().unwrap().offsets.push(offset);
        }
        rows += 1;
        if rows % STRIDE == 0 {
            let mut index = index.lock().unwrap();
            index.rows = rows;
            index.bytes = reader.position().byte();
        }
    }
}

fn seek_row(reader: &mut csv::Reader<File>, row: u64, offset: u64) -> Result<()> {
    let mut position = csv::Position::new();
    position.set_byte(offset).set_record(row / STRIDE * STRIDE);
    reader.seek(position)?;
    let mut record = csv::StringRecord::new();
    for _ in 0..row % STRIDE {
        if !reader.read_record(&mut record)? {
            return Err("Source ended unexpectedly; reopen it".into());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn save(
    path: &Path,
    stamp: &Stamp,
    delimiter: u8,
    offsets: &[u64],
    edits: &Edits,
    destination: &Path,
    mut temp: tempfile::NamedTempFile,
    progress: &AtomicU64,
    order: Option<&SortOrder>,
) -> Result<()> {
    stamp.check(path)?;
    let mut input = File::open(path)?;
    let mut parser = reader(File::open(path)?, delimiter);
    let mut output = BufWriter::with_capacity(1024 * 1024, temp.as_file_mut());
    if let Some(order) = order {
        sort::write_sorted(&mut input, &mut output, order, edits, delimiter, progress)?;
    } else {
        let mut cursor = 0;
        let mut edits = edits.iter().peekable();
        while let Some((&(row, _), _)) = edits.peek().copied() {
            seek_row(&mut parser, row, offsets[(row / STRIDE) as usize])?;
            let start = parser.position().byte();
            let mut record = csv::StringRecord::new();
            if !parser.read_record(&mut record)? {
                return Err("Source ended unexpectedly during save".into());
            }
            let end = parser.position().byte();
            let mut values: Vec<String> = record.iter().map(str::to_owned).collect();
            while let Some((&(next_row, col), value)) = edits.peek().copied() {
                if next_row != row {
                    break;
                }
                *values
                    .get_mut(col)
                    .ok_or("Edited column no longer exists")? = value.clone();
                edits.next();
            }
            copy_bytes(&mut input, &mut output, start - cursor, progress)?;
            let mut raw = vec![0; (end - start).try_into()?];
            input.read_exact(&mut raw)?;
            // csv may include a BOM or skipped blank lines before this record, and
            // may stop between CR and LF. Keep those bytes exactly where they were.
            let bom = if start == 0 && raw.starts_with(b"\xef\xbb\xbf") {
                3
            } else {
                0
            };
            let prefix = bom
                + raw[bom..]
                    .iter()
                    .take_while(|&&b| b == b'\r' || b == b'\n')
                    .count();
            let suffix = raw[prefix..]
                .iter()
                .rev()
                .take_while(|&&b| b == b'\r' || b == b'\n')
                .count();
            output.write_all(&raw[..prefix])?;
            // ponytail: reserialize only edited records; raw field spans are needed
            // if byte-exact preservation inside an edited record becomes required.
            let mut writer = csv::WriterBuilder::new()
                .delimiter(delimiter)
                .from_writer(Vec::new());
            writer.write_record(&values)?;
            let mut encoded = writer.into_inner()?;
            encoded.pop(); // The writer's default LF; preserve the source terminator below.
            output.write_all(&encoded)?;
            output.write_all(&raw[raw.len() - suffix..])?;
            progress.fetch_add(end - start, Ordering::Relaxed);
            cursor = end;
        }
        copy_bytes(&mut input, &mut output, stamp.len - cursor, progress)?;
    }
    output.flush()?;
    drop(output);
    stamp.check(path)?;
    if Stamp::new(input.metadata()?)? != *stamp {
        return Err("Source changed during save; output was not published".into());
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(destination)?;
    Ok(())
}

fn copy_bytes(
    input: &mut File,
    output: &mut impl Write,
    mut remaining: u64,
    progress: &AtomicU64,
) -> io::Result<()> {
    let mut buffer = vec![0; 1024 * 1024];
    while remaining > 0 {
        let size = remaining.min(buffer.len() as u64) as usize;
        let n = input.read(&mut buffer[..size])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Source was truncated",
            ));
        }
        output.write_all(&buffer[..n])?;
        remaining -= n as u64;
        progress.fetch_add(n as u64, Ordering::Relaxed);
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
        assert!(sheet.progress().error.is_none(), "{:?}", sheet.progress());
    }

    fn saved(sheet: &Sheet, path: &Path) -> Vec<u8> {
        sheet
            .save_as(path)
            .unwrap()
            .result
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        fs::read(path).unwrap()
    }

    #[test]
    fn csv_edit_save_and_undo_preserve_data_and_untouched_bytes() {
        let dir = tempfile::tempdir().unwrap();
        for (case, bytes, delimiter) in [
            ("lf", b"id,name,note\n00123,\"Tallinn\",\"line 1\nline 2\"\n00456,\"Tartu\",\"a \"\"quote\"\"\"".as_slice(), b','),
            ("crlf", b"\xef\xbb\xbfid;name;note\r\n\r\n00123;\"Tallinn\";\"line 1\r\nline 2\"\r\n00456;\"Tartu\";\"a \"\"quote\"\"\"\r\n".as_slice(), b';'),
            ("blank", b"\n\nid,name,note\n\n00123,Tallinn,note\n\n00456,Tartu,last\n\n".as_slice(), b','),
        ] {
            let source = dir.path().join(case);
            fs::write(&source, bytes).unwrap();
            let mut sheet = Sheet::open(&source, delimiter).unwrap();
            ready(&sheet);
            assert_eq!(sheet.progress().rows, 3);
            assert_eq!(&sheet.window(1, 1).unwrap()[0][0], "00123");
            assert_eq!(saved(&sheet, &dir.path().join(format!("{case}-copy"))), bytes);
            sheet.set(1, 1, "New, \"quoted\"\ncity 🦀".into()).unwrap();
            sheet.set(1, 0, "00000".into()).unwrap();
            sheet.set(2, 2, String::new()).unwrap();
            let out = dir.path().join(format!("{case}-edited"));
            let written = saved(&sheet, &out);
            let mut result = reader(File::open(&out).unwrap(), delimiter);
            let records: Vec<_> = result.records().map(|r| r.unwrap()).collect();
            assert_eq!(records.len(), 3);
            assert_eq!(&records[1][0], "00000");
            assert_eq!(&records[1][1], "New, \"quoted\"\ncity 🦀");
            assert_eq!(&records[2][2], "");
            assert!(written.starts_with(&bytes[..bytes.iter().position(|&b| b == b'\n').unwrap() + 1]));
            assert_eq!(fs::read(&source).unwrap(), bytes);
            assert!(sheet.save_as(&source).is_err());
            assert!(sheet.save_as(&out).is_err());
            assert!(sheet.undo()); assert!(sheet.undo()); assert!(sheet.undo());
            assert_eq!(sheet.edit_count(), 0);
            assert_eq!(saved(&sheet, &dir.path().join(format!("{case}-undo"))), bytes);
        }
    }

    #[test]
    fn sparse_seeks_ragged_rows_and_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("many.csv");
        let mut writer = csv::WriterBuilder::new()
            .flexible(true)
            .from_path(&source)
            .unwrap();
        for i in 0..1000 {
            writer
                .write_record([i.to_string(), format!("line {i}\nsecond line")])
                .unwrap();
        }
        writer.write_record(["ragged"]).unwrap();
        writer.flush().unwrap();
        let mut sheet = Sheet::open(&source, b',').unwrap();
        ready(&sheet);
        assert_eq!(sheet.progress().rows, 1001);
        for row in [0, 1, 63, 64, 65, 127, 128, 999] {
            assert_eq!(&sheet.window(row, 1).unwrap()[0][0], row.to_string());
        }
        sheet.set(64, 0, "changed".into()).unwrap();
        sheet.set(999, 1, "last".into()).unwrap();
        let out = dir.path().join("out.csv");
        saved(&sheet, &out);
        let mut copied = Sheet::open(&out, b',').unwrap();
        ready(&copied);
        assert_eq!(&copied.window(64, 1).unwrap()[0][0], "changed");
        assert_eq!(&copied.window(999, 1).unwrap()[0][1], "last");
        assert!(sheet.set(1000, 1, "missing".into()).is_err());
        fs::write(&source, "replaced\n").unwrap();
        assert!(sheet.window(0, 1).is_err());
        assert!(sheet.save_as(&dir.path().join("bad.csv")).is_err());
    }

    #[test]
    fn empty_invalid_utf8_and_destination_races() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.csv");
        fs::write(&source, "").unwrap();
        let mut empty = Sheet::open(&source, b',').unwrap();
        ready(&empty);
        assert_eq!(empty.progress().rows, 0);
        assert!(empty.window(0, 50).unwrap().is_empty());
        assert!(saved(&empty, &dir.path().join("empty.csv")).is_empty());
        fs::write(&source, b"a,b\n\xff,c\n").unwrap();
        let invalid = Sheet::open(&source, b',').unwrap();
        while !invalid.progress().done {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(invalid.progress().error.is_some());
        assert!(invalid.save_as(&dir.path().join("invalid.csv")).is_err());
        // The final no-clobber publication also protects paths created during a save.
        let destination = dir.path().join("existing.csv");
        fs::write(&destination, "keep me").unwrap();
        let temp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        assert!(temp.persist_noclobber(&destination).is_err());
        assert_eq!(fs::read_to_string(destination).unwrap(), "keep me");
    }
}
