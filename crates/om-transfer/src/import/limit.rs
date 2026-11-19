//! OpenMaker's limits on an import source. FileMaker's own limits for
//! hostile sources are not observed; these bound what a source can make
//! a reader allocate or loop over, whoever runs the import (a server
//! script, the desktop). A source past one is refused with its format's
//! import error, as a malformed one is: 708 for text and dBase files, 718
//! for FMPXMLRESULT, 730 for an Excel workbook (script-steps.md,
//! "Implementation notes (import/export)", Import limits).

use std::mem::{size_of, size_of_val};

use crate::{Result, TransferError};

/// The largest source read, in bytes. A host reads at most one byte more,
/// so that a larger file is refused without being read whole. Its text
/// takes up to three times as much (a byte that is not UTF-8 is U+FFFD),
/// so an import peaks at about 2 GiB with [`PARSED_BYTES`].
pub const SOURCE_BYTES: usize = 256 << 20;

/// What a source's parsed rows may take: each value costs its slot and its
/// repetitions' strings and text, each row its slot. About 20 million
/// short values. Text is counted as it is read, so no one value grows
/// past it.
pub const PARSED_BYTES: usize = 1 << 30;

/// Columns of a row in every format: Excel's 16,384 (`A` to `XFD`).
pub const COLUMNS: usize = 16_384;

/// Rows of a worksheet: Excel's 1,048,576.
pub const SHEET_ROWS: usize = 1_048_576;

/// Parts of a workbook's zip package (a workbook has tens).
pub const ZIP_ENTRIES: usize = 10_000;

/// The workbook parts read, uncompressed, together (a full sheet of
/// 1,048,576 rows of ten short cells is about 350 MB of XML).
pub const ZIP_BYTES: u64 = 512 << 20;

/// The largest compression ratio of a workbook part past
/// [`ZIP_RATIO_FLOOR`] (Apache POI's default for the same check).
pub const ZIP_RATIO: u64 = 100;

/// A part may always inflate to this many bytes.
pub const ZIP_RATIO_FLOOR: u64 = 1 << 20;

/// What a parse may still allocate ([`PARSED_BYTES`]), and the error that
/// refuses more.
#[derive(Debug)]
pub(crate) struct Budget {
    left: usize,
    code: i32,
}

impl Budget {
    /// The whole budget, refused with `code`.
    pub(crate) fn new(code: i32) -> Budget {
        Budget::with(code, PARSED_BYTES)
    }

    pub(crate) fn with(code: i32, left: usize) -> Budget {
        Budget { left, code }
    }

    /// The format's error, for a source past any limit.
    pub(crate) fn error(&self, message: &str) -> TransferError {
        TransferError::new(self.code, message)
    }

    pub(crate) fn charge(&mut self, bytes: usize) -> Result<()> {
        match self.left.checked_sub(bytes) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => Err(self.error("The source is larger than OpenMaker imports.")),
        }
    }

    /// A value with these repetitions.
    pub(crate) fn value(&mut self, value: &[String]) -> Result<()> {
        let text: usize = value.iter().map(String::len).sum();
        self.charge(text)?;
        self.slots(value)
    }

    /// A value whose text was charged as it was read.
    pub(crate) fn slots(&mut self, value: &[String]) -> Result<()> {
        self.charge(size_of::<Vec<String>>() + size_of_val(value))
    }

    /// `n` values of no repetitions (the gap before a cell).
    pub(crate) fn empty_values(&mut self, n: usize) -> Result<()> {
        self.charge(n.saturating_mul(size_of::<Vec<String>>()))
    }

    pub(crate) fn row(&mut self) -> Result<()> {
        self.charge(size_of::<Vec<Vec<String>>>())
    }

    /// Refuse a row of more than [`COLUMNS`] values.
    pub(crate) fn columns(&self, n: usize) -> Result<()> {
        if n > COLUMNS {
            Err(self.error("A row has more than 16,384 columns."))
        } else {
            Ok(())
        }
    }
}
