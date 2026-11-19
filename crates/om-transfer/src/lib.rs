//! FileMaker's record transfer steps without a host: Export Records (36),
//! Save Records as Excel (143) and Import Records (35).
//!
//! - [`spec`]: the stored import and export specs (script-steps.md
//!   11.3-11.5), byte for byte.
//! - [`charset`]: the character sets, numbered as the specs number them.
//! - [`export`]: a found set out to tab- and comma-separated text, merge,
//!   HTML table, FMPXMLRESULT XML, Excel workbook and dBase files, through
//!   an [`export::ExportSource`] the host implements over its records.
//! - [`import`]: those formats in, mapped by the import order, matched,
//!   added or updated through an [`import::ImportTarget`] the host
//!   implements over its writes (which apply the file's auto-enter and
//!   validation rules as the import options say).
//!
//! The formats are FileMaker Server 26.0.3's own output byte for byte, as
//! its script engine wrote them for the batteries in
//! `tools/fms-transfer-oracle` (script-steps.md, "Implementation notes
//! (import/export)"). Nothing here touches files: hosts resolve paths,
//! read the source bytes and write the result.

#![forbid(unsafe_code)]

pub mod charset;
pub mod export;
pub mod import;
pub mod spec;
pub mod text;
mod xml;

pub use charset::{ExportCharset, ImportCharset};
pub use spec::{
    ExportOptions, FieldFlags, FieldSpec, FileType, ImportAction, ImportOptions, Key, Mapping,
    Profile, ProfileEx,
};

/// A record of the host's file.
pub type RecordId = u64;

/// A field's data type as transfers see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldType {
    Text,
    Number,
    Date,
    Time,
    Timestamp,
    Container,
}

/// What a field is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Normal,
    /// A stored or unstored calculation.
    Calculation,
    Summary,
    /// Global storage.
    Global,
}

/// FileMaker errors of the transfer steps (catalogs/error-codes.csv).
pub mod code {
    pub const FILE_MISSING: i32 = 100;
    pub const FIELD_MISSING: i32 = 102;
    /// "Export order is invalid": no field to export.
    pub const EXPORT_ORDER_INVALID: i32 = 410;
    /// "Import order is invalid": nothing to import (an empty source).
    pub const IMPORT_ORDER_INVALID: i32 = 409;
    /// "Error in parsing XML file".
    pub const XML_PARSE: i32 = 718;
    /// "Error when exporting; intended format does not support repeating
    /// fields" (Excel and dBase).
    pub const REPEATING_FIELDS: i32 = 720;
    /// "Errors occurred during import; records could not be imported".
    pub const IMPORT_ERRORS: i32 = 729;
    /// "File you are importing from contains no data".
    pub const NO_DATA: i32 = 731;
    /// "Unable to create file on disk".
    pub const CANNOT_CREATE: i32 = 800;
    /// "Command is unavailable".
    pub const UNAVAILABLE: i32 = 3;
    /// "User canceled action": the host's script was aborted or ran out of
    /// time ([`crate::import::ImportTarget::interrupted`]).
    pub const CANCELED: i32 = 1;
}

/// The error of a transfer the host interrupted (the script was aborted or
/// ran out of time).
pub(crate) fn interrupted() -> TransferError {
    TransferError::new(code::CANCELED, "The script was aborted.")
}

/// A transfer ended with a FileMaker error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferError {
    pub code: i32,
    pub message: String,
}

impl TransferError {
    pub fn new(code: i32, message: impl Into<String>) -> TransferError {
        TransferError {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for TransferError {}

pub type Result<T, E = TransferError> = std::result::Result<T, E>;
