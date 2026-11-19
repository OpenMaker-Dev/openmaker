//! FileMaker calculation values: the value model the calculation engine and
//! script engine compute with (spec `docs/re/calc-values.md`).
//!
//! - [`number`]: FileMaker's decimal number with display precision and guard
//!   digits, its arithmetic, text forms and transcendental functions;
//! - [`datetime`]: dates, times and timestamps, their display and parsing;
//! - [`text`]: styled text, characters (grapheme clusters), value lists and
//!   collation;
//! - [`locale`]: a file's formats;
//! - [`value`]: the value type and its conversions;
//! - [`ops`]: the operators;
//! - [`functions`]: the function library, by Claris category;
//! - [`error`]: error codes and the first-error rule;
//! - [`eval`]: the evaluator over `om_formula` trees and the [`eval::CalcHost`]
//!   boundary.
//!
//! Functions take values and a [`Ctx`] (locale, today, evaluation
//! precision), convert their arguments with [`Value::to_number`],
//! [`Value::to_text`], [`Value::to_date`] and friends, and return a value or
//! a [`CalcError`].
#![forbid(unsafe_code)]

pub mod datetime;
pub mod error;
pub mod eval;
pub mod functions;
pub mod locale;
pub mod number;
pub mod ops;
pub mod text;
pub mod value;

pub use datetime::{Date, Time, TimeStyle, Timestamp};
pub use error::{CalcError, FirstError};
pub use locale::{Language, Locale};
pub use number::Number;
pub use text::{CharStyle, Text};
pub use value::{Container, Ctx, Interrupt, LibraryRef, StreamType, Value, ValueType, Work};
