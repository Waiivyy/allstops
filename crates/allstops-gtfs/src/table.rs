//! Streaming CSV table reading with column lookup by name, UTF-8 checks,
//! byte-order-mark stripping and row limits.

use std::io::Read;

use crate::error::{Error, LimitKind, Result};
use crate::limits::as_limit;

/// One open GTFS table. Columns are looked up once by name; rows are then
/// read field by field without allocating per row.
pub struct Table<'r> {
    file: String,
    reader: csv::Reader<&'r mut dyn Read>,
    headers: Vec<String>,
    record: csv::ByteRecord,
    rows: u64,
    max_rows: u64,
}

impl<'r> Table<'r> {
    pub fn new(file: &str, input: &'r mut dyn Read, max_rows: u64) -> Result<Self> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .flexible(true)
            .from_reader(input);
        let raw = reader
            .byte_headers()
            .map_err(|e| csv_error(file, e))?
            .clone();
        let mut headers = Vec::with_capacity(raw.len());
        for (i, h) in raw.iter().enumerate() {
            let h = std::str::from_utf8(h).map_err(|_| Error::Row {
                file: file.to_string(),
                line: 1,
                message: "header is not valid UTF-8".into(),
            })?;
            let h = if i == 0 {
                h.trim_start_matches('\u{feff}')
            } else {
                h
            };
            headers.push(h.trim().to_string());
        }
        Ok(Table {
            file: file.to_string(),
            reader,
            headers,
            record: csv::ByteRecord::new(),
            rows: 0,
            max_rows,
        })
    }

    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    pub fn column(&self, name: &str) -> Option<usize> {
        self.headers.iter().position(|h| h == name)
    }

    pub fn require(&self, name: &'static str) -> Result<usize> {
        self.column(name).ok_or_else(|| Error::MissingColumn {
            file: self.file.clone(),
            column: name,
        })
    }

    /// Advance to the next row. Returns false at the end of the file.
    pub fn next_row(&mut self) -> Result<bool> {
        match self.reader.read_byte_record(&mut self.record) {
            Ok(false) => Ok(false),
            Ok(true) => {
                self.rows += 1;
                if self.rows > self.max_rows {
                    return Err(Error::Limit(LimitKind::RowCount {
                        file: self.file.clone(),
                        limit: self.max_rows,
                    }));
                }
                Ok(true)
            }
            Err(e) => Err(csv_error(&self.file, e)),
        }
    }

    /// The current row as read, untrimmed.
    pub fn record(&self) -> &csv::ByteRecord {
        &self.record
    }

    /// 1-based physical line of the current row, for error messages.
    pub fn line(&self) -> u64 {
        self.record.position().map(|p| p.line()).unwrap_or(0)
    }

    /// Field of the current row, trimmed. A missing trailing field reads as
    /// empty, which GTFS treats the same as an empty value.
    pub fn get(&self, col: Option<usize>) -> Result<&str> {
        let Some(col) = col else { return Ok("") };
        let raw = self.record.get(col).unwrap_or(b"");
        std::str::from_utf8(raw)
            .map(str::trim)
            .map_err(|_| self.error("field is not valid UTF-8"))
    }

    pub fn error(&self, message: impl Into<String>) -> Error {
        Error::Row {
            file: self.file.clone(),
            line: self.line(),
            message: message.into(),
        }
    }
}

fn csv_error(file: &str, e: csv::Error) -> Error {
    let line = e.position().map(|p| p.line()).unwrap_or(0);
    if let csv::ErrorKind::Io(io) = e.kind() {
        if let Some(kind) = as_limit(io) {
            return Error::Limit(kind);
        }
        return Error::File {
            file: file.to_string(),
            message: format!("could not read: {io}"),
        };
    }
    Error::Row {
        file: file.to_string(),
        line,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bom_and_quotes_from_headers() {
        let data = "\u{feff}\"stop_id\",\"stop_name\"\n\"a\",\"Alpha\"\n";
        let mut input = data.as_bytes();
        let mut t = Table::new("stops.txt", &mut input, 10).unwrap();
        let id = t.require("stop_id").unwrap();
        let name = t.column("stop_name");
        assert!(t.next_row().unwrap());
        assert_eq!(t.get(Some(id)).unwrap(), "a");
        assert_eq!(t.get(name).unwrap(), "Alpha");
        assert!(!t.next_row().unwrap());
    }

    #[test]
    fn missing_column_is_named() {
        let mut input = &b"stop_name\nx\n"[..];
        let t = Table::new("stops.txt", &mut input, 10).unwrap();
        let err = t.require("stop_id").unwrap_err();
        assert_eq!(
            err.to_string(),
            "stops.txt: missing required column stop_id"
        );
    }

    #[test]
    fn invalid_utf8_field_is_an_error_with_line() {
        let mut input = &b"stop_id\nok\n\xff\xfe\n"[..];
        let mut t = Table::new("stops.txt", &mut input, 10).unwrap();
        let c = t.require("stop_id").unwrap();
        assert!(t.next_row().unwrap());
        assert!(t.get(Some(c)).is_ok());
        assert!(t.next_row().unwrap());
        let err = t.get(Some(c)).unwrap_err();
        assert!(err.to_string().contains("line 3"), "{err}");
    }

    #[test]
    fn row_limit_holds() {
        let mut input = &b"a\n1\n2\n3\n"[..];
        let mut t = Table::new("x.txt", &mut input, 2).unwrap();
        assert!(t.next_row().unwrap());
        assert!(t.next_row().unwrap());
        assert!(matches!(
            t.next_row(),
            Err(Error::Limit(LimitKind::RowCount { .. }))
        ));
    }

    #[test]
    fn short_rows_read_as_empty() {
        let mut input = &b"a,b\n1\n"[..];
        let mut t = Table::new("x.txt", &mut input, 10).unwrap();
        let b = t.column("b");
        assert!(t.next_row().unwrap());
        assert_eq!(t.get(b).unwrap(), "");
    }
}
