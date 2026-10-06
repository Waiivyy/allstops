use std::cell::Cell;
use std::io::{self, Read};
use std::rc::Rc;

use crate::error::LimitKind;

/// Hard limits applied to every feed, trusted or not. They exist to stop zip
/// bombs and memory blow-ups from user-supplied archives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Size of the zip file itself.
    pub max_compressed_bytes: u64,
    /// Total bytes produced by decompressing every entry that is read.
    pub max_uncompressed_bytes: u64,
    /// Number of entries in the archive, files and folders together.
    pub max_entries: usize,
    /// Data rows in any single file.
    pub max_rows_per_file: u64,
    /// Bytes in any single physical line.
    pub max_line_bytes: usize,
}

impl Default for Limits {
    /// Generous enough for a large regional feed (the full MVV feed is about
    /// 18 MB compressed, 291 MB uncompressed and 2.2 million stop_times rows).
    fn default() -> Self {
        Limits {
            max_compressed_bytes: 256 << 20,
            max_uncompressed_bytes: 2 << 30,
            max_entries: 64,
            max_rows_per_file: 40_000_000,
            max_line_bytes: 64 << 10,
        }
    }
}

/// Shared budget of uncompressed bytes across all entries of one archive.
#[derive(Debug, Clone)]
pub(crate) struct ByteBudget {
    remaining: Rc<Cell<u64>>,
    limit: u64,
}

impl ByteBudget {
    pub(crate) fn new(limit: u64) -> Self {
        ByteBudget {
            remaining: Rc::new(Cell::new(limit)),
            limit,
        }
    }
}

/// A reader that enforces the uncompressed byte budget and the line length
/// limit while bytes stream through it. Breaking a limit surfaces as an
/// `io::Error` wrapping a [`LimitKind`], which callers convert back.
pub(crate) struct LimitedReader<R> {
    inner: R,
    budget: ByteBudget,
    file: String,
    max_line: usize,
    line_len: usize,
}

impl<R: Read> LimitedReader<R> {
    pub(crate) fn new(inner: R, budget: ByteBudget, file: &str, max_line: usize) -> Self {
        LimitedReader {
            inner,
            budget,
            file: file.to_string(),
            max_line,
            line_len: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct LimitError(pub LimitKind);

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for LimitError {}

fn limit_err(kind: LimitKind) -> io::Error {
    io::Error::other(LimitError(kind))
}

/// Recover a limit violation that travelled through an `io::Error`.
pub(crate) fn as_limit(err: &io::Error) -> Option<LimitKind> {
    err.get_ref()
        .and_then(|e| e.downcast_ref::<LimitError>())
        .map(|e| e.0.clone())
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.budget.remaining.get();
        // Ask for at most one byte past the budget so an overrun is detected
        // without reading the whole oversized entry.
        let cap = buf
            .len()
            .min(remaining.saturating_add(1).min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..cap])?;
        if n as u64 > remaining {
            return Err(limit_err(LimitKind::UncompressedSize {
                limit: self.budget.limit,
            }));
        }
        self.budget.remaining.set(remaining - n as u64);

        let chunk = &buf[..n];
        let mut start = 0;
        for pos in memchr::memchr_iter(b'\n', chunk) {
            self.line_len += pos - start;
            if self.line_len > self.max_line {
                return Err(self.line_error());
            }
            self.line_len = 0;
            start = pos + 1;
        }
        self.line_len += n - start;
        if self.line_len > self.max_line {
            return Err(self.line_error());
        }
        Ok(n)
    }
}

impl<R> LimitedReader<R> {
    fn line_error(&self) -> io::Error {
        limit_err(LimitKind::LineLength {
            file: self.file.clone(),
            limit: self.max_line,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(data: &[u8], budget: u64, max_line: usize) -> io::Result<Vec<u8>> {
        let mut r = LimitedReader::new(data, ByteBudget::new(budget), "t.txt", max_line);
        let mut out = Vec::new();
        r.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn passes_data_within_limits() {
        assert_eq!(drain(b"ab\ncd\n", 6, 2).unwrap(), b"ab\ncd\n");
    }

    #[test]
    fn rejects_overlong_line() {
        let err = drain(b"ab\nabc\n", 100, 2).unwrap_err();
        assert!(matches!(as_limit(&err), Some(LimitKind::LineLength { .. })));
    }

    #[test]
    fn rejects_overlong_final_line_without_newline() {
        let err = drain(b"abc", 100, 2).unwrap_err();
        assert!(matches!(as_limit(&err), Some(LimitKind::LineLength { .. })));
    }

    #[test]
    fn rejects_bytes_past_budget() {
        let err = drain(b"0123456789", 5, 100).unwrap_err();
        assert!(matches!(
            as_limit(&err),
            Some(LimitKind::UncompressedSize { limit: 5 })
        ));
    }

    #[test]
    fn budget_is_shared_between_readers() {
        let budget = ByteBudget::new(8);
        let mut out = Vec::new();
        LimitedReader::new(&b"12345"[..], budget.clone(), "a", 100)
            .read_to_end(&mut out)
            .unwrap();
        let err = LimitedReader::new(&b"12345"[..], budget, "b", 100)
            .read_to_end(&mut out)
            .unwrap_err();
        assert!(as_limit(&err).is_some());
    }
}
