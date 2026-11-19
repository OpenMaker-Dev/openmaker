//! What editing formula text needs, shared by the desktop's Specify
//! Calculation and the authoring service: the scanner that classes the
//! text ([`scan`]), the functions a file offers ([`functions`]), how a
//! function or field is written into the text ([`Insertion`],
//! [`field_reference`]) and name completion at the cursor
//! ([`completion`]).
//!
//! Positions are characters (Unicode scalar values) of the text, as an
//! editor's cursor counts them; no range ever splits a character.

pub mod completion;
pub mod functions;
pub mod scan;

use std::ops::Range;

use crate::formula::{Context, Field, Token};
use crate::host::CalcSchema;

pub use completion::{complete, Candidate, CandidateKind, Completion, Refusal, Scope, Unavailable};

/// The byte offset of character `chars` of `text` (its length past the
/// end).
pub fn byte_index(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(b, _)| b)
}

/// How a field is written in a formula, as FileMaker prints it
/// (calc-encoding.md 4.2): `Table::Field` when qualified, and `${...}`
/// around a name that needs it.
pub fn field_reference(
    schema: &CalcSchema,
    occurrence: u64,
    field: u64,
    qualified: bool,
) -> String {
    crate::formula::print(
        &[Token::Field(Field::new(occurrence, field, qualified))],
        &Context::new(schema.names()),
    )
}

/// Text that replaces a range of the formula, and what is selected
/// afterwards (characters from the start of `text`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insertion {
    pub text: String,
    pub select: Range<usize>,
}

impl Insertion {
    /// `text` with the cursor after it.
    pub fn caret_after(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.chars().count();
        Self {
            text,
            select: end..end,
        }
    }

    /// A function's signature, with the first parameter selected so typing
    /// replaces it (`Abs ( number )` selects `number`). A signature without
    /// parameters, or with `select_parameter` false (Get functions), leaves
    /// the cursor after it.
    pub fn function(signature: &str, select_parameter: bool) -> Self {
        let parameter = signature.find('(').and_then(|open| {
            let rest = &signature[open + 1..];
            let start = rest.find(|c: char| !matches!(c, ' ' | '{' | '[' | ']' | '}'))?;
            let len = rest[start..]
                .find([' ', ';', '(', ')', '{', '}', '[', ']'])
                .unwrap_or(rest.len() - start);
            (len > 0).then(|| open + 1 + start..open + 1 + start + len)
        });
        match parameter.filter(|_| select_parameter) {
            Some(range) => {
                let start = signature[..range.start].chars().count();
                let end = start + signature[range].chars().count();
                Self {
                    text: signature.to_owned(),
                    select: start..end,
                }
            }
            None => Self::caret_after(signature),
        }
    }

    /// Replace `range` (characters) of `text` with this; the new
    /// selection. The range is clamped to the text.
    pub fn apply(&self, text: &mut String, range: Range<usize>) -> Range<usize> {
        let count = text.chars().count();
        let start = range.start.min(count);
        let end = range.end.clamp(start, count);
        let (from, to) = (byte_index(text, start), byte_index(text, end));
        text.replace_range(from..to, &self.text);
        start + self.select.start..start + self.select.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn functions_insert_their_signature_with_the_first_parameter_selected() {
        let selected = |s: &str| {
            let i = Insertion::function(s, true);
            i.text
                .chars()
                .skip(i.select.start)
                .take(i.select.len())
                .collect::<String>()
        };
        assert_eq!(selected("Abs ( number )"), "number");
        assert_eq!(
            selected("Let ( {[} var1 = expression1 {; var2 = expression2...]} ; calculation )"),
            "var1"
        );
        assert_eq!(
            selected("While ( [ initialVariable ] ; condition ; [ logic ] ; result )"),
            "initialVariable"
        );
        assert_eq!(selected("WindowNames {( fileName )}"), "fileName");
        assert_eq!(
            Insertion::function("Random", true),
            Insertion::caret_after("Random")
        );
        assert_eq!(
            Insertion::function("Get ( AccountName )", false),
            Insertion::caret_after("Get ( AccountName )")
        );
        // Inserted at the cursor, replacing the selection.
        let mut text = "1 + x".to_owned();
        let selection = Insertion::function("Abs ( number )", true).apply(&mut text, 4..5);
        assert_eq!(text, "1 + Abs ( number )");
        assert_eq!(selection, 10..16);
    }
}
