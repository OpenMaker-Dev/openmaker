//! FileMaker calculation formula syntax: the stored codec, the source
//! tokenizer, parser and compiler, and the printer.
//!
//! Spec: `docs/re/calc-encoding.md`. A stored formula has a context table
//! occurrence (child 4), a formatted stream (child 5: the source as tokens
//! with whitespace and comments) and a Polish stream (child 6: compiled
//! prefix code, the form FileMaker evaluates):
//!
//! ```text
//! child 5 --formatted::decode--> [Token] --print----------> text
//! child 6 --polish::decode-----> Expr    (evaluated by om-calc)
//! text    --compile-----> [Token] (child 5) + Expr --polish::encode--> child 6
//! [Token] --compile_tokens--> Expr
//! ```
//!
//! The codec is lossless: decoding a stored stream and encoding it again
//! gives the same bytes ([`Formula`] holds all three children). Names are
//! stored as ids; a [`Names`] implementation ([`NameTable`] in memory)
//! supplies a file's catalogs to the compiler and printer. The function,
//! operator and Get selector catalog is embedded ([`catalog`]). This crate
//! knows no values: number literals stay in their stored decimal form
//! ([`Decimal`]) for `om-calc` to convert.

pub mod catalog;
mod codec;
mod error;
mod expr;
pub mod formatted;
mod lex;
mod names;
mod number;
mod operand;
mod parse;
pub mod polish;
mod print;

pub use catalog::Opcode;
pub use error::{DecodeError, DecodeErrorKind, ErrorCode, ParseError};
pub use expr::Expr;
pub use formatted::Token;
pub use names::{Function, NameTable, Names, NoNames};
pub use number::{Decimal, Number, FORMAT_FLOAT, FORMAT_SCIENTIFIC};
pub use operand::{Field, Key, PluginFunctionId, Style, Symbol, Text};
pub use parse::{compile, compile_tokens, Compiled, Context, MAX_NESTING};
pub use print::{print, FIELD_MISSING, FUNCTION_MISSING, TABLE_MISSING};

/// Whether a field or table occurrence name must be written `${name}` in a
/// formula to read back as one name (`DBCalcScan::QuoteName`): it is
/// empty, holds a delimiter, starts with a digit, a period or `$`, has
/// whitespace at either end, holds `and`, `or`, `xor` or `not` as a word or
/// `::`, or names a function that takes no arguments.
pub fn name_needs_quotes(name: &str) -> bool {
    print::needs_quotes(name)
}

/// A stored formula (calc-encoding.md 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formula {
    /// Child 4: the table occurrence the formula is evaluated from; occurrence
    /// 0 means none. Custom function bodies have no child 4.
    pub context: Option<Key>,
    /// Child 5; `None` when the formula text was stripped.
    pub formatted: Option<Vec<Token>>,
    /// Child 6.
    pub polish: Expr,
}

impl Formula {
    /// Decode the three children of a formula node.
    pub fn decode(
        context: Option<&[u8]>,
        formatted: Option<&[u8]>,
        polish: &[u8],
    ) -> Result<Self, DecodeError> {
        Ok(Self {
            context: context.map(|c| Key(c.to_vec())),
            formatted: formatted.map(formatted::decode).transpose()?,
            polish: polish::decode(polish)?,
        })
    }

    /// The context table occurrence id, `None` for occurrence 0 or none.
    pub fn occurrence(&self) -> Option<u64> {
        self.context
            .as_ref()
            .and_then(Key::id)
            .filter(|&id| id != 0)
    }

    /// Child 4 as stored.
    pub fn encode_context(&self) -> Option<Vec<u8>> {
        self.context.as_ref().map(|k| k.0.clone())
    }

    /// Child 5 as stored.
    pub fn encode_formatted(&self) -> Option<Vec<u8>> {
        self.formatted.as_deref().map(formatted::encode)
    }

    /// Child 6 as stored.
    pub fn encode_polish(&self) -> Vec<u8> {
        polish::encode(&self.polish)
    }
}
