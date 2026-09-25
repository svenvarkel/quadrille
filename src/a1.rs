//! A1 coordinates, zero-based. The one parser and formatter for columns, cells and
//! ranges; callers with their own wording map `A1Error` to it.
use std::{fmt, num::ParseIntError};

/// Zero-based `(row, column)`.
pub type Address = (u64, usize);

/// Inclusive rectangle from `first` (top-left) to `last` (bottom-right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub first: Address,
    pub last: Address,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A1Error {
    /// Column text that is empty or not only ASCII letters.
    Letters(String),
    /// Column letters beyond `usize`.
    ColumnTooLarge,
    /// A cell that is not letters followed by digits.
    Cell,
    /// Row digits beyond `u64`.
    RowTooLarge(ParseIntError),
    /// Row 0.
    RowZero,
    /// A range whose first cell is below or right of its last.
    Reversed,
}

impl fmt::Display for A1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Letters(letters) => write!(f, "Use column letters such as B, not {letters:?}"),
            Self::ColumnTooLarge => f.write_str("Column address is too large"),
            Self::Cell => f.write_str("Use a cell address such as B7"),
            Self::RowTooLarge(error) => error.fmt(f),
            Self::RowZero => f.write_str("Rows start at 1"),
            Self::Reversed => f.write_str("Range must run from top-left to bottom-right"),
        }
    }
}

impl std::error::Error for A1Error {}

/// Zero-based index of column letters, case-insensitive. Unbounded: CSV records may be
/// wider than XLSX's XFD; workbooks enforce their own column limit on edits.
pub fn column_index(letters: &str) -> Result<usize, A1Error> {
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(A1Error::Letters(letters.to_owned()));
    }
    letters
        .bytes()
        .try_fold(0usize, |column, b| {
            column
                .checked_mul(26)?
                .checked_add((b.to_ascii_uppercase() - b'A' + 1) as usize)
        })
        .map(|column| column - 1)
        .ok_or(A1Error::ColumnTooLarge)
}

/// Column letters of a zero-based index: 0 is A, 26 is AA.
pub fn column_name(mut col: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap()
}

/// Comma-separated column letters, sorted and without duplicates.
pub fn parse_columns(text: &str) -> Result<Vec<usize>, A1Error> {
    let mut columns = text
        .split(',')
        .map(column_index)
        .collect::<Result<Vec<_>, _>>()?;
    columns.sort_unstable();
    columns.dedup();
    Ok(columns)
}

/// A cell such as `B7` or `aa65`.
pub fn parse_cell(text: &str) -> Result<Address, A1Error> {
    let split = text
        .find(|c: char| c.is_ascii_digit())
        .ok_or(A1Error::Cell)?;
    let (letters, digits) = text.split_at(split);
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(A1Error::Cell);
    }
    let column = column_index(letters).map_err(|error| match error {
        A1Error::Letters(_) => A1Error::Cell,
        error => error,
    })?;
    let row = digits
        .parse::<u64>()
        .map_err(A1Error::RowTooLarge)?
        .checked_sub(1)
        .ok_or(A1Error::RowZero)?;
    Ok((row, column))
}

pub fn cell_name((row, col): Address) -> String {
    format!("{}{}", column_name(col), row + 1)
}

/// The two ends of `A1:B2`; a single cell is both ends.
pub fn split_range(text: &str) -> (&str, &str) {
    text.split_once(':').unwrap_or((text, text))
}

/// `A1:B2`, or a single cell as a one-cell range.
pub fn parse_range(text: &str) -> Result<Range, A1Error> {
    let (first, last) = split_range(text);
    Range::new(parse_cell(first)?, parse_cell(last)?)
}

impl Range {
    pub fn new(first: Address, last: Address) -> Result<Self, A1Error> {
        if first.0 > last.0 || first.1 > last.1 {
            return Err(A1Error::Reversed);
        }
        Ok(Self { first, last })
    }

    pub fn rows(&self) -> u64 {
        self.last.0 - self.first.0 + 1
    }

    pub fn columns(&self) -> usize {
        self.last.1 - self.first.1 + 1
    }
}

/// Always `first:last`, also for a single cell.
impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", cell_name(self.first), cell_name(self.last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn too_large() -> ParseIntError {
        "18446744073709551616".parse::<u64>().unwrap_err()
    }

    #[test]
    fn columns_round_trip_at_every_boundary() {
        for (letters, index) in [
            ("A", 0),
            ("z", 25),
            ("AA", 26),
            ("aB", 27),
            ("AZ", 51),
            ("BA", 52),
            ("ZZ", 701),
            ("AAA", 702),
            ("XFD", 16_383),
            ("XFE", 16_384),
        ] {
            assert_eq!(column_index(letters), Ok(index), "{letters}");
            assert_eq!(column_name(index), letters.to_ascii_uppercase());
        }
        for index in (0..20_000).chain([usize::MAX / 26, usize::MAX - 1]) {
            assert_eq!(column_index(&column_name(index)), Ok(index), "{index}");
        }
        // The one-based value must fit, so usize::MAX itself has a name but no index.
        assert_eq!(
            column_index(&column_name(usize::MAX)),
            Err(A1Error::ColumnTooLarge)
        );
        assert_eq!(
            column_index("ZZZZZZZZZZZZZZZZZZZZZZZZ"),
            Err(A1Error::ColumnTooLarge)
        );
        for bad in ["", " A", "A ", "A1", "Õ", "$A", "-"] {
            assert_eq!(
                column_index(bad),
                Err(A1Error::Letters(bad.into())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn column_lists_are_sorted_and_unique() {
        assert_eq!(parse_columns("c,b,B"), Ok(vec![1, 2]));
        assert_eq!(parse_columns("XFD"), Ok(vec![16_383]));
        for (bad, letters) in [
            ("", ""),
            ("A,,B", ""),
            ("A, B", " B"),
            ("A,", ""),
            ("B1", "B1"),
        ] {
            assert_eq!(parse_columns(bad), Err(A1Error::Letters(letters.into())));
        }
        assert_eq!(
            parse_columns("A,ZZZZZZZZZZZZZZZZZZZZZZZZ"),
            Err(A1Error::ColumnTooLarge)
        );
    }

    #[test]
    fn cells_parse_and_format() {
        for (text, address, name) in [
            ("A1", (0, 0), "A1"),
            ("aa65", (64, 26), "AA65"),
            ("XFD1048576", (1_048_575, 16_383), "XFD1048576"),
            ("XFE1048577", (1_048_576, 16_384), "XFE1048577"),
            ("B007", (6, 1), "B7"),
            (
                "A18446744073709551615",
                (u64::MAX - 1, 0),
                "A18446744073709551615",
            ),
        ] {
            assert_eq!(parse_cell(text), Ok(address), "{text}");
            assert_eq!(cell_name(address), name);
        }
        for bad in [
            "", "A", "1", "12", "A-1", "A1x", "A1 ", " A1", "🦀1", "$A$1", "A$1", "1A",
        ] {
            assert_eq!(parse_cell(bad), Err(A1Error::Cell), "{bad:?}");
        }
        // Syntax errors win over an oversized column.
        assert_eq!(parse_cell("ZZZZZZZZZZZZZZZZZZZZZZZZ1x"), Err(A1Error::Cell));
        assert_eq!(
            parse_cell("ZZZZZZZZZZZZZZZZZZZZZZZZ1"),
            Err(A1Error::ColumnTooLarge)
        );
        // An oversized column wins over an oversized or zero row.
        assert_eq!(
            parse_cell("ZZZZZZZZZZZZZZZZZZZZZZZZ0"),
            Err(A1Error::ColumnTooLarge)
        );
        assert_eq!(
            parse_cell("A18446744073709551616"),
            Err(A1Error::RowTooLarge(too_large()))
        );
        assert_eq!(parse_cell("A0"), Err(A1Error::RowZero));
        assert_eq!(parse_cell("A00"), Err(A1Error::RowZero));
    }

    #[test]
    fn ranges_parse_measure_and_format() {
        let range = parse_range("B2:D11").unwrap();
        assert_eq!((range.first, range.last), ((1, 1), (10, 3)));
        assert_eq!((range.rows(), range.columns()), (10, 3));
        assert_eq!(range.to_string(), "B2:D11");
        let single = parse_range("c3").unwrap();
        assert_eq!((single.first, single.last), ((2, 2), (2, 2)));
        assert_eq!((single.rows(), single.columns()), (1, 1));
        assert_eq!(single.to_string(), "C3:C3");
        assert_eq!(split_range("A1:B2"), ("A1", "B2"));
        assert_eq!(split_range("A1"), ("A1", "A1"));
        assert_eq!(split_range("A1:B2:C3"), ("A1", "B2:C3"));
        for (bad, error) in [
            ("B2:A1", A1Error::Reversed),
            ("B1:A2", A1Error::Reversed),
            ("A2:B1", A1Error::Reversed),
            ("A1:B2:C3", A1Error::Cell),
            ("A1:", A1Error::Cell),
            (":A1", A1Error::Cell),
            ("A0:A1", A1Error::RowZero),
            ("A1:A0", A1Error::RowZero),
        ] {
            assert_eq!(parse_range(bad), Err(error), "{bad}");
        }
        assert_eq!(Range::new((0, 0), (0, 0)).unwrap().to_string(), "A1:A1");
    }

    #[test]
    fn errors_read_as_the_cli_reports_them() {
        for (error, text) in [
            (
                A1Error::Letters(" B".into()),
                r#"Use column letters such as B, not " B""#,
            ),
            (A1Error::ColumnTooLarge, "Column address is too large"),
            (A1Error::Cell, "Use a cell address such as B7"),
            (
                A1Error::RowTooLarge(too_large()),
                "number too large to fit in target type",
            ),
            (A1Error::RowZero, "Rows start at 1"),
            (
                A1Error::Reversed,
                "Range must run from top-left to bottom-right",
            ),
        ] {
            assert_eq!(error.to_string(), text);
            let boxed: Box<dyn std::error::Error + Send + Sync> = error.into();
            assert_eq!(boxed.to_string(), text);
        }
    }
}
