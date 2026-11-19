//! Errors for HBAM container and slot decoding.

use std::fmt;

/// Error opening or decoding an HBAM container or a packed slot buffer.
#[derive(Debug)]
pub enum Error {
    /// Reading the file failed.
    Io(std::io::Error),
    /// The `HBAM7` magic at superblock offset 0x0f is missing.
    NotHbam,
    /// The superblock page size is unusable (smaller than a page header).
    BadPageSize(u32),
    /// The file length is not a whole number of pages.
    PartialPage { len: usize, page_size: u32 },
    /// A page header is internally inconsistent (free space larger than the body).
    BadPageHeader { page: u32, reason: &'static str },
    /// The level-0 leaf chain is broken: no head, a cycle, a bad link, or a non-leaf page.
    BrokenChain { page: u32, reason: &'static str },
    /// A chunk runs past the end of its buffer.
    Truncated { offset: usize },
    /// A chunk code byte outside the grammar in file-format.md 4.2.
    UnsupportedCode { offset: usize, code: u8 },
    /// A chunk after the root list was closed (a pop with an empty stack).
    PopUnderflow { offset: usize },
    /// More nested key segments than the 17 levels the format allows.
    TooDeep { offset: usize },
    /// Sibling keys are not in ascending order, or a value is stored twice.
    KeyOrder { path: String },
    /// Text that is not valid masked SCSU.
    Text(om_scsu::DecodeError),
    /// A key holds a value of the wrong shape for its known meaning.
    BadValue { path: String, reason: &'static str },
    /// A key tree cannot be written as a file (hbam-writer.md limits).
    Write(&'static str),
    /// A tree read from slots would take more than `limit` bytes of memory
    /// (`tree::read_owned`).
    TooLarge { limit: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::NotHbam => write!(f, "not an HBAM7 file (magic missing at 0x0f)"),
            Error::BadPageSize(n) => write!(f, "unusable page size {n}"),
            Error::PartialPage { len, page_size } => {
                write!(
                    f,
                    "file length {len} is not a multiple of page size {page_size}"
                )
            }
            Error::BadPageHeader { page, reason } => write!(f, "page {page}: {reason}"),
            Error::BrokenChain { page, reason } => write!(f, "leaf chain at page {page}: {reason}"),
            Error::Truncated { offset } => write!(f, "chunk at offset {offset} is truncated"),
            Error::UnsupportedCode { offset, code } => {
                write!(f, "unsupported chunk code {code:#04x} at offset {offset}")
            }
            Error::PopUnderflow { offset } => {
                write!(f, "chunk after the root list closed at offset {offset}")
            }
            Error::TooDeep { offset } => write!(f, "key nesting too deep at offset {offset}"),
            Error::KeyOrder { path } => write!(f, "key out of order or duplicated at {path}"),
            Error::Text(e) => write!(f, "bad text value: {e:?}"),
            Error::BadValue { path, reason } => write!(f, "{path}: {reason}"),
            Error::Write(reason) => write!(f, "cannot write the file: {reason}"),
            Error::TooLarge { limit } => {
                write!(f, "the tree would take more than {limit} bytes of memory")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<om_scsu::DecodeError> for Error {
    fn from(e: om_scsu::DecodeError) -> Self {
        Error::Text(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
