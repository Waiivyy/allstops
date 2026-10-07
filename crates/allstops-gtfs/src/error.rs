use std::fmt;

/// Every way loading a feed can fail. Malformed input always ends up here,
/// never in a panic.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a readable zip archive: {0}")]
    Zip(String),

    #[error("input limit exceeded: {0}")]
    Limit(LimitKind),

    #[error("missing required file {0}")]
    MissingFile(&'static str),

    #[error("{file}: missing required column {column}")]
    MissingColumn { file: String, column: &'static str },

    #[error("{file} line {line}: {message}")]
    Row {
        file: String,
        line: u64,
        message: String,
    },

    #[error("{file}: {message}")]
    File { file: String, message: String },

    #[error("date {date} is outside the feed's service range {start} to {end}")]
    DateOutOfRange {
        date: chrono::NaiveDate,
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
    },

    #[error("unknown time zone {0:?} in agency.txt")]
    TimeZone(String),

    #[error("date {date} does not exist in time zone {tz}")]
    DateNotInTimeZone { date: chrono::NaiveDate, tz: String },
}

/// Which input limit a feed broke. See [`crate::Limits`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitKind {
    CompressedSize { limit: u64 },
    UncompressedSize { limit: u64 },
    EntryCount { count: usize, limit: usize },
    RowCount { file: String, limit: u64 },
    LineLength { file: String, limit: usize },
}

impl fmt::Display for LimitKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitKind::CompressedSize { limit } => {
                write!(f, "archive is larger than {limit} bytes")
            }
            LimitKind::UncompressedSize { limit } => {
                write!(f, "archive expands to more than {limit} bytes")
            }
            LimitKind::EntryCount { count, limit } => {
                write!(f, "archive has {count} entries, the limit is {limit}")
            }
            LimitKind::RowCount { file, limit } => {
                write!(f, "{file} has more than {limit} rows")
            }
            LimitKind::LineLength { file, limit } => {
                write!(f, "{file} has a line longer than {limit} bytes")
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
