//! Errors: malformed stored streams, and FileMaker's parse errors.

use std::fmt;
use std::ops::Range;

/// A stored stream that does not decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Byte offset in the stream where decoding failed.
    pub offset: usize,
    pub kind: DecodeErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeErrorKind {
    /// The stream ended inside a token.
    Truncated,
    /// An opcode no stored stream of this kind may hold.
    InvalidOpcode(u16),
    /// A count above FileMaker's limit of 1000 (calc-encoding.md 3.1).
    CountTooLarge(u16),
    /// An operand with an impossible layout.
    Malformed(&'static str),
    /// Text that is not valid SCSU.
    Text,
    /// Bytes after a complete Polish expression.
    TrailingBytes,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DecodeErrorKind::Truncated => write!(f, "formula truncated at byte {}", self.offset),
            DecodeErrorKind::InvalidOpcode(op) => {
                write!(f, "invalid opcode {op} at byte {}", self.offset)
            }
            DecodeErrorKind::CountTooLarge(n) => {
                write!(f, "argument count {n} too large at byte {}", self.offset)
            }
            DecodeErrorKind::Malformed(what) => {
                write!(f, "malformed {what} at byte {}", self.offset)
            }
            DecodeErrorKind::Text => write!(f, "invalid text at byte {}", self.offset),
            DecodeErrorKind::TrailingBytes => {
                write!(
                    f,
                    "unexpected bytes after the formula at byte {}",
                    self.offset
                )
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A formula FileMaker would reject, with FileMaker's error number
/// (catalogs/error-codes.csv; calc-encoding.md 6.4) and the source range the
/// error points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub code: ErrorCode,
    /// The range FileMaker selects, in UTF-16 code units of the source text
    /// (empty at the end of the text). For [`crate::compile_tokens`], token
    /// indexes instead.
    pub span: Range<usize>,
}

/// FileMaker error numbers a formula's syntax can raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// 1200, FileMaker's generic calculation error: the formula nests past
    /// what OpenMaker reads ([`crate::MAX_NESTING`]).
    NestingTooDeep = 1200,
    /// 102: a name that is no field of the context table occurrence.
    FieldMissing = 102,
    /// 106: a field name with no table context, or an unknown occurrence.
    TableMissing = 106,
    TooFewParameters = 1201,
    TooManyParameters = 1202,
    UnexpectedEnd = 1203,
    /// "Number, text constant, field name or `(` expected".
    OperandExpected = 1204,
    CommentNotTerminated = 1205,
    TextNotTerminated = 1206,
    UnbalancedParenthesis = 1207,
    /// "Operator missing, function not found or `(` not expected".
    FunctionNotFound = 1208,
    NameMissing = 1209,
    ListNotAllowed = 1211,
    OperatorExpected = 1212,
    FieldRequired = 1214,
    InvalidGetParameter = 1215,
    /// `GetSummary`'s first argument is not a summary field.
    SummaryFieldRequired = 1216,
    /// "Cannot evaluate the number": a numeric literal out of range.
    InvalidNumber = 1218,
    /// `Self` where the formula does not support it.
    NotSupported = 1225,
}

impl ErrorCode {
    pub fn number(self) -> u16 {
        self as u16
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FileMaker error {} at {}..{}",
            self.code.number(),
            self.span.start,
            self.span.end
        )
    }
}

impl std::error::Error for ParseError {}
