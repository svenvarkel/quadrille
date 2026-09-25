//! Find scans with its own reader, so it neither moves the view nor waits for indexing.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindQuery {
    pub text: String,
    /// Zero-based columns to search; `None` searches every stored or edited cell.
    pub columns: Option<Vec<usize>>,
    pub exact: bool,
    pub ignore_case: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindResult {
    /// `(row, column)` in view coordinates, and the value `--read` would return.
    pub matches: Vec<((u64, usize), String)>,
    /// Where the next page starts; `None` when the scan reached the end of the view.
    pub next: Option<(u64, usize)>,
    /// Records read from `from`'s row onwards, including partially searched ones.
    pub records_scanned: u64,
}

struct Matcher {
    text: String,
    columns: Option<Vec<usize>>,
    exact: bool,
    ignore_case: bool,
}

impl Matcher {
    fn matches(&self, value: &str) -> bool {
        let lowered;
        let value = if !self.ignore_case {
            value
        } else if value.is_ascii() {
            // Same result as to_lowercase without allocating: the lowercased text
            // contains no ASCII capitals, and ASCII values lowercase bytewise.
            let (value, text) = (value.as_bytes(), self.text.as_bytes());
            return if self.exact {
                value.eq_ignore_ascii_case(text)
            } else {
                text.is_empty()
                    || value
                        .windows(text.len())
                        .any(|w| w.eq_ignore_ascii_case(text))
            };
        } else {
            lowered = value.to_lowercase();
            &lowered
        };
        if self.exact {
            value == self.text
        } else {
            value.contains(&self.text)
        }
    }
}

impl Sheet {
    pub fn find(&self, query: &FindQuery, from: (u64, usize), limit: usize) -> Result<FindResult> {
        self.scan_find(query, from, limit, &mut || {})
    }

    /// `each_record` runs after every record read; tests use it to change the source mid-scan.
    fn scan_find(
        &self,
        query: &FindQuery,
        from: (u64, usize),
        limit: usize,
        each_record: &mut dyn FnMut(),
    ) -> Result<FindResult> {
        if query.text.is_empty() && !query.exact {
            return Err("Search text must not be empty unless the match is exact".into());
        }
        if limit == 0 {
            return Err("The match limit must be at least 1".into());
        }
        let matcher = Matcher {
            text: if query.ignore_case {
                query.text.to_lowercase()
            } else {
                query.text.clone()
            },
            columns: query.columns.clone().map(|mut columns| {
                columns.sort_unstable();
                columns.dedup();
                columns
            }),
            exact: query.exact,
            ignore_case: query.ignore_case,
        };
        self.check_source()?;
        let mut parser = reader(File::open(self.data_path())?, self.delimiter);
        let mut record = csv::StringRecord::new();
        let mut result = FindResult::default();
        if let Some(order) = &self.order {
            for (row, reference) in order.rows.iter().enumerate().skip(from.0 as usize) {
                let mut position = csv::Position::new();
                position.set_byte(reference.start).set_record(reference.row);
                parser.seek(position)?;
                if !parser.read_record(&mut record)? {
                    return Err("Source ended unexpectedly; reopen it".into());
                }
                each_record();
                let row = row as u64;
                if self.search(
                    &matcher,
                    row,
                    reference.row,
                    from,
                    &record,
                    limit,
                    &mut result,
                ) {
                    break;
                }
            }
        } else {
            let (byte, mut row) = start(&self.index.lock().unwrap().offsets, from.0);
            let mut position = csv::Position::new();
            position.set_byte(byte).set_record(row);
            parser.seek(position)?;
            while parser.read_record(&mut record)? {
                each_record();
                if row >= from.0
                    && self.search(&matcher, row, row, from, &record, limit, &mut result)
                {
                    break;
                }
                row += 1;
            }
        }
        self.check_source()?;
        Ok(result)
    }

    /// Search one record; true once `limit` matches are held.
    #[allow(clippy::too_many_arguments)]
    fn search(
        &self,
        matcher: &Matcher,
        row: u64,
        source: u64,
        from: (u64, usize),
        record: &csv::StringRecord,
        limit: usize,
        result: &mut FindResult,
    ) -> bool {
        result.records_scanned += 1;
        let first = if row == from.0 { from.1 } else { 0 };
        let value = |col| {
            self.edits
                .get(&(source, col))
                .map(String::as_str)
                .or_else(|| record.get(col))
        };
        let (mut filtered, mut all);
        let columns: &mut dyn Iterator<Item = usize> = match &matcher.columns {
            Some(columns) => {
                filtered = columns.iter().copied().filter(|&col| col >= first);
                &mut filtered
            }
            None => {
                let edited = self
                    .edits
                    .range((source, first.max(record.len()))..=(source, usize::MAX));
                all = (first..record.len()).chain(edited.map(|(&(_, col), _)| col));
                &mut all
            }
        };
        for col in columns {
            let Some(value) = value(col) else { continue };
            if matcher.matches(value) {
                result.matches.push(((row, col), value.to_owned()));
                if result.matches.len() == limit {
                    result.next = Some((row, col + 1));
                    return true;
                }
            }
        }
        false
    }
}

/// Where to start parsing for `row`: the nearest published stride at or before it,
/// else byte 0. Offsets are only published once their record has been read.
fn start(offsets: &[u64], row: u64) -> (u64, u64) {
    let stride = ((row / STRIDE) as usize).min(offsets.len().saturating_sub(1));
    offsets
        .get(stride)
        .map_or((0, 0), |&byte| (byte, stride as u64 * STRIDE))
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

    fn open(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> Sheet {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        let sheet = Sheet::open(&path, b',').unwrap();
        ready(&sheet);
        sheet
    }

    fn query(text: &str) -> FindQuery {
        FindQuery {
            text: text.into(),
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

    /// Every page size yields the complete result, in order, without read-ahead.
    fn assert_pages(sheet: &Sheet, query: &FindQuery) -> FindResult {
        let all = sheet.find(query, (0, 0), usize::MAX).unwrap();
        assert_eq!(all.next, None);
        for limit in 1..=all.matches.len() + 1 {
            let (mut from, mut pages, mut seen) = ((0, 0), 0, Vec::new());
            loop {
                let page = sheet.find(query, from, limit).unwrap();
                assert!(page.matches.len() <= limit);
                assert_eq!(page.next.is_some(), page.matches.len() == limit, "{limit}");
                seen.extend(page.matches);
                pages += 1;
                assert!(pages <= all.matches.len() + 2, "no progress at {from:?}");
                match page.next {
                    Some(next) => from = next,
                    None => break,
                }
            }
            assert_eq!(seen, all.matches, "{query:?} limit {limit}");
        }
        all
    }

    const SAMPLE: &[u8] = b"\xef\xbb\xbfcity,name,note\nTallinn,\xc3\x95ie,\"multi\nline Tallinn\"\ntartu,,x\nTALLINN-N\xc3\xb5mme\n,\xc3\xb5ie,\n";

    #[test]
    fn modes_columns_and_ragged_records() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = open(&dir, "sample.csv", SAMPLE);
        let run = |q: FindQuery| sheet.find(&q, (0, 0), 100).unwrap();
        let tallinn = run(query("Tallinn"));
        assert_eq!(
            cells(&tallinn),
            [(1, 0, "Tallinn"), (1, 2, "multi\nline Tallinn")]
        );
        assert_eq!(tallinn.next, None);
        assert_eq!(tallinn.records_scanned, 5);
        let ignore = |text: &str| FindQuery {
            ignore_case: true,
            ..query(text)
        };
        assert_eq!(
            cells(&run(ignore("tallinn"))),
            [
                (1, 0, "Tallinn"),
                (1, 2, "multi\nline Tallinn"),
                (3, 0, "TALLINN-Nõmme")
            ]
        );
        assert_eq!(
            cells(&run(ignore("õ"))),
            [(1, 1, "Õie"), (3, 0, "TALLINN-Nõmme"), (4, 1, "õie")]
        );
        assert_eq!(
            cells(&run(query("õ"))),
            [(3, 0, "TALLINN-Nõmme"), (4, 1, "õie")]
        );
        // The BOM is not part of the first value.
        let exact = |text: &str| FindQuery {
            exact: true,
            ..query(text)
        };
        assert_eq!(cells(&run(exact("city"))), [(0, 0, "city")]);
        assert!(run(exact("Tallin")).matches.is_empty());
        assert_eq!(
            cells(&run(FindQuery {
                ignore_case: true,
                ..exact("õIE")
            })),
            [(1, 1, "Õie"), (4, 1, "õie")]
        );
        // Empty exact finds stored empty fields only, not the missing fields of row 4.
        assert_eq!(cells(&run(exact(""))), [(2, 1, ""), (4, 0, ""), (4, 2, "")]);
        let columns = |text: &str, columns: Vec<usize>| FindQuery {
            columns: Some(columns),
            ..ignore(text)
        };
        assert_eq!(
            cells(&run(columns("tallinn", vec![2, 2]))),
            [(1, 2, "multi\nline Tallinn")]
        );
        assert_eq!(
            cells(&run(columns("i", vec![1, 0]))),
            [
                (0, 0, "city"),
                (1, 0, "Tallinn"),
                (1, 1, "Õie"),
                (3, 0, "TALLINN-Nõmme"),
                (4, 1, "õie")
            ]
        );
        assert!(
            run(FindQuery {
                exact: true,
                ..columns("", vec![5])
            })
            .matches
            .is_empty()
        );
        assert_eq!(run(columns("x", vec![5])).records_scanned, 5);
        assert!(sheet.find(&query(""), (0, 0), 1).is_err());
        assert!(sheet.find(&query("a"), (0, 0), 0).is_err());
    }

    #[test]
    fn ignore_case_agrees_with_unicode_lowercase() {
        let values = [
            "",
            "k",
            "K",
            "kelvin",
            "KELVIN",
            "Õie",
            "õie",
            "ÕIE",
            "İx",
            "i\u{307}x",
            "Tallinn-Nõmme",
            "a,b",
        ];
        let texts = [
            "", "k", "\u{212a}", "KEL", "õ", "Õie", "İ", "i\u{307}", "NÕMME", "tallinn", "a,b",
            "zz",
        ];
        for text in texts {
            for exact in [false, true] {
                let matcher = Matcher {
                    text: text.to_lowercase(),
                    columns: None,
                    exact,
                    ignore_case: true,
                };
                for value in values {
                    let (value_lower, text_lower) = (value.to_lowercase(), text.to_lowercase());
                    let expected = if exact {
                        value_lower == text_lower
                    } else {
                        value_lower.contains(&text_lower)
                    };
                    assert_eq!(
                        matcher.matches(value),
                        expected,
                        "{value:?} {text:?} {exact}"
                    );
                }
            }
        }
    }

    #[test]
    fn paging_resumes_inside_records_and_after_the_last_cell() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = open(&dir, "pages.csv", b"a,xa,x\nx\n,x,,x\nb\nx,x\n");
        let q = query("x");
        let first = sheet.find(&q, (0, 0), 1).unwrap();
        assert_eq!(
            (cells(&first), first.next, first.records_scanned),
            (vec![(0, 1, "xa")], Some((0, 2)), 1)
        );
        let second = sheet.find(&q, (0, 2), 2).unwrap();
        assert_eq!(
            (cells(&second), second.next, second.records_scanned),
            (vec![(0, 2, "x"), (1, 0, "x")], Some((1, 1)), 2)
        );
        // The final cell still yields a next page, which is then empty.
        let last = sheet.find(&q, (4, 1), 1).unwrap();
        assert_eq!((cells(&last), last.next), (vec![(4, 1, "x")], Some((4, 2))));
        let empty = sheet.find(&q, (4, 2), 1).unwrap();
        assert_eq!(
            (empty.matches.len(), empty.next, empty.records_scanned),
            (0, None, 1)
        );
        let beyond = sheet.find(&q, (99, 0), 1).unwrap();
        assert_eq!(
            (beyond.matches.len(), beyond.next, beyond.records_scanned),
            (0, None, 0)
        );
        // A `from` column past the last filtered column resumes on the next row.
        let filtered = FindQuery {
            columns: Some(vec![0, 1]),
            ..q.clone()
        };
        let resumed = sheet.find(&filtered, (2, 3), 1).unwrap();
        assert_eq!(cells(&resumed), [(4, 0, "x")]);
        assert_eq!(resumed.records_scanned, 3);
        let mut sheet = sheet;
        // Workbook-style edits past the stored fields are searched and page like any cell.
        sheet.edits.insert((0, 7), "x".into());
        sheet.edits.insert((2, 5), "x".into());
        let past = sheet.find(&q, (0, 3), 1).unwrap();
        assert_eq!((cells(&past), past.next), (vec![(0, 7, "x")], Some((0, 8))));
        assert_eq!(cells(&sheet.find(&q, (0, 8), 1).unwrap()), [(1, 0, "x")]);
        for q in [
            q.clone(),
            filtered,
            FindQuery {
                columns: Some(vec![3, 9]),
                ..q.clone()
            },
            FindQuery {
                exact: true,
                ..query("")
            },
        ] {
            assert_pages(&sheet, &q);
        }
    }

    #[test]
    fn edits_and_sorted_views_are_searched_as_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut sheet = open(
            &dir,
            "sorted.csv",
            b"name,city\nc,Tartu\na,Tallinn\nb,Tallinn\nd\n",
        );
        sheet.set(3, 1, "Narva".into()).unwrap();
        sheet.set(1, 1, "Tallinn".into()).unwrap();
        let tallinn = query("Tallinn");
        assert_eq!(
            cells(&sheet.find(&tallinn, (0, 0), 9).unwrap()),
            [(1, 1, "Tallinn"), (2, 1, "Tallinn")]
        );
        assert!(
            sheet
                .find(&query("Tartu"), (0, 0), 9)
                .unwrap()
                .matches
                .is_empty()
        );
        let job = sheet.start_sort(parse_sort("A").unwrap(), true).unwrap();
        sheet
            .apply_sort(job.result.recv().unwrap().unwrap())
            .unwrap();
        let sorted = assert_pages(&sheet, &tallinn);
        assert_eq!(cells(&sorted), [(1, 1, "Tallinn"), (3, 1, "Tallinn")]);
        for &((row, col), ref value) in &sorted.matches {
            let record = &sheet.window(row, 1).unwrap()[0];
            assert_eq!(sheet.cell_value(row, col, record), Some(value.as_str()));
        }
        assert_eq!(
            cells(&sheet.find(&query("Narva"), (0, 0), 9).unwrap()),
            [(2, 1, "Narva")]
        );
        let from_view = sheet.find(&tallinn, (2, 0), 1).unwrap();
        assert_eq!(
            (cells(&from_view), from_view.next, from_view.records_scanned),
            (vec![(3, 1, "Tallinn")], Some((3, 2)), 2)
        );
        let end = sheet.find(&query("d"), (4, 0), 9).unwrap();
        assert_eq!((cells(&end), end.records_scanned), (vec![(4, 0, "d")], 1));
        assert_eq!(
            sheet.find(&tallinn, (5, 0), 9).unwrap(),
            FindResult::default()
        );
        assert_pages(
            &sheet,
            &FindQuery {
                exact: true,
                ..query("")
            },
        );
        assert_pages(
            &sheet,
            &FindQuery {
                columns: Some(vec![0]),
                ignore_case: true,
                ..query("A")
            },
        );
    }

    #[test]
    fn start_position_uses_only_published_strides() {
        assert_eq!(start(&[], 0), (0, 0));
        assert_eq!(start(&[], 500), (0, 0));
        assert_eq!(start(&[0], 63), (0, 0));
        assert_eq!(start(&[0], 64), (0, 0));
        assert_eq!(start(&[3, 700], 64), (700, 64));
        assert_eq!(start(&[3, 700], 127), (700, 64));
        assert_eq!(start(&[3, 700, 1400], 128), (1400, 128));
        assert_eq!(start(&[3, 700], 1000), (700, 64));
    }

    fn many(dir: &tempfile::TempDir, records: u64) -> PathBuf {
        let path = dir.path().join("many.csv");
        let mut writer = csv::Writer::from_path(&path).unwrap();
        for i in 0..records {
            // Multiline records straddle every stride boundary.
            writer
                .write_record([i.to_string(), format!("row {i}\nnext"), (i % 7).to_string()])
                .unwrap();
        }
        writer.flush().unwrap();
        path
    }

    #[test]
    fn partial_and_missing_indexes_give_the_same_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = many(&dir, 300);
        let mut sheet = Sheet::open(&path, b',').unwrap();
        ready(&sheet);
        let q = FindQuery {
            exact: true,
            columns: Some(vec![2]),
            ..query("3")
        };
        let expected = sheet.find(&q, (130, 1), 5).unwrap();
        assert_eq!(expected.matches[0].0, (136, 2));
        let offsets = sheet.index.lock().unwrap().offsets.clone();
        for published in [0, 1, 2, 3, offsets.len()] {
            sheet.index = Arc::new(Mutex::new(Index {
                offsets: offsets[..published].to_vec(),
                ..Index::default()
            }));
            assert_eq!(
                sheet.find(&q, (130, 1), 5).unwrap(),
                expected,
                "{published}"
            );
            assert!(!sheet.progress().done);
        }
    }

    #[test]
    fn finds_before_indexing_finishes_and_stops_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = many(&dir, 200_000);
        let sheet = Sheet::open(&path, b',').unwrap();
        let early = sheet.find(&query("row 5"), (0, 0), 1).unwrap();
        assert_eq!(
            (cells(&early), early.next, early.records_scanned),
            (vec![(5, 1, "row 5\nnext")], Some((5, 2)), 6)
        );
        // An index that never completes must not delay find.
        let mut stalled = Sheet::open(&path, b',').unwrap();
        stalled.index = Arc::new(Mutex::new(Index::default()));
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            let result = stalled.find(&query("row 199999"), (199_990, 0), 1);
            send.send((result.unwrap(), stalled.progress().done))
                .unwrap();
        });
        let (late, done) = receive.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(!done);
        assert_eq!(
            (cells(&late), late.records_scanned),
            (vec![(199_999, 1, "row 199999\nnext")], 10)
        );
    }

    #[test]
    fn invalid_utf8_is_an_error_only_inside_the_scanned_part() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = open(&dir, "bad.csv", b"a,b\nneedle\n\xff\nneedle\n");
        assert!(sheet.find(&query("needle"), (0, 0), 5).is_err());
        let early = sheet.find(&query("needle"), (0, 0), 1).unwrap();
        assert_eq!(cells(&early), [(1, 0, "needle")]);
        assert!(sheet.progress().error.is_some());
    }

    #[test]
    fn source_changes_before_or_during_the_scan_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = open(&dir, "changed.csv", b"x\ny\nx\n");
        let path = sheet.path.clone();
        let append = || {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(b"x\n").unwrap();
        };
        let mut calls = 0;
        let error = sheet
            .scan_find(&query("x"), (0, 0), 1, &mut || {
                calls += 1;
                if calls == 1 {
                    append();
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(calls, 1);
        assert!(sheet.find(&query("x"), (0, 0), 1).is_err());

        let mut sheet = open(&dir, "sorted.csv", b"b,1\na,2\nc,3\n");
        let job = sheet.start_sort(parse_sort("A").unwrap(), false).unwrap();
        sheet
            .apply_sort(job.result.recv().unwrap().unwrap())
            .unwrap();
        let path = sheet.path.clone();
        let error = sheet
            .scan_find(&query("zzz"), (0, 0), 1, &mut || {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(2)
                    .unwrap();
            })
            .unwrap_err();
        assert!(error.to_string().contains("ended unexpectedly"), "{error}");
        // Bytes rewritten in place (same length) are read as found: invalid UTF-8 errors.
        fs::write(&path, b"b,1\na,2\nc,3\n").unwrap();
        let mut sheet = Sheet::open(&path, b',').unwrap();
        ready(&sheet);
        let job = sheet.start_sort(parse_sort("A").unwrap(), false).unwrap();
        sheet
            .apply_sort(job.result.recv().unwrap().unwrap())
            .unwrap();
        let error = sheet
            .scan_find(&query("zzz"), (0, 0), 1, &mut || {
                let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.write_all(b"\xff").unwrap();
            })
            .unwrap_err();
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn workbook_archive_changes_during_the_scan_are_errors() {
        use zip::{ZipWriter, write::SimpleFileOptions};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.xlsx");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        let main = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let package = "http://schemas.openxmlformats.org/package/2006/relationships";
        for (name, text) in [
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.to_owned()),
            ("_rels/.rels", format!(r#"<Relationships xmlns="{package}"><Relationship Id="b" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#)),
            ("xl/workbook.xml", format!(r#"<workbook xmlns="{main}" xmlns:r="{rel}"><sheets><sheet name="S" sheetId="1" r:id="r1"/></sheets></workbook>"#)),
            ("xl/_rels/workbook.xml.rels", format!(r#"<Relationships xmlns="{package}"><Relationship Id="r1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#)),
            ("xl/worksheets/sheet1.xml", format!(r#"<worksheet xmlns="{main}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row><row r="2"><c r="B2" t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#)),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let sheet = Sheet::open(&path, b',').unwrap();
        ready(&sheet);
        assert_eq!(
            cells(&sheet.find(&query("x"), (0, 0), 9).unwrap()),
            [(0, 0, "x"), (1, 1, "x")]
        );
        let error = sheet
            .scan_find(&query("x"), (0, 0), 9, &mut || {
                let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
                file.write_all(b" ").unwrap();
            })
            .unwrap_err();
        assert!(error.to_string().contains("changed"), "{error}");
    }
}
