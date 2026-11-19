//! Name completion at the cursor: the name being typed, the characters
//! choosing a candidate replaces, and the functions, Get selectors, custom
//! functions and fields whose names start with it.
//!
//! The name is the one the scanner ([`super::scan`]) reads ending at the
//! cursor, so a text literal, a comment, a keyword or a `${...}` name is
//! never completed. Fields and custom functions come only from the file's
//! [`CalcSchema`]; without one they are reported unavailable, never
//! guessed. FileMaker's own completion (its trigger, list, ranking and
//! keys) is unmeasured (re/client-ui.md, Unknowns); the matching and
//! order here are OpenMaker's choices (docs/desktop-client.md).

use std::ops::Range;

use super::functions::{self, Entry};
use super::scan::{self, Class, Piece};
use super::{field_reference, Insertion};
use crate::formula::{Names, NoNames};
use crate::host::CalcSchema;

/// Where unqualified field names are read, and how fields are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// The context occurrence (Specify Calculation's context popup):
    /// unqualified names are its fields.
    pub occurrence: Option<u64>,
    /// A calculation field of the occurrence's table: its fields are
    /// written without their table, as the fields pane inserts them.
    pub unqualified: bool,
}

/// What a candidate is, in the order candidates with the same name are
/// listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateKind {
    Field,
    CustomFunction,
    Function,
    /// `Get ( AccountName )` as a whole.
    GetFunction,
    /// `AccountName` inside `Get ( ... )`.
    Selector,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateKind::Field => "field",
            CandidateKind::CustomFunction => "custom_function",
            CandidateKind::Function => "function",
            CandidateKind::GetFunction => "get_function",
            CandidateKind::Selector => "get_selector",
        }
    }
}

/// A name that completes the prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: CandidateKind,
    /// The name the prefix matched: `Left`, `First Name`,
    /// `Get ( AccountName )`, `AccountName`.
    pub name: String,
    /// What a list shows: a function's signature, a field's reference.
    pub label: String,
    /// What choosing it writes over [`Completion::replace`], and the
    /// selection afterwards.
    pub insertion: Insertion,
    /// A field's occurrence and id.
    pub field: Option<(u64, u64)>,
    /// A function's category name (resource 135), if it has one.
    pub category: Option<&'static str>,
}

/// Why nothing is completed at the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The cursor is inside a text literal.
    InText,
    /// The cursor is inside a comment.
    InComment,
    /// The cursor is inside a word of a name, not at its end.
    InsideName,
    /// No name ends at the cursor (after whitespace, an operator, a
    /// number, a keyword or a `${...}` name).
    NoName,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::InText => "in_text",
            Refusal::InComment => "in_comment",
            Refusal::InsideName => "inside_name",
            Refusal::NoName => "no_name",
        }
    }
}

/// Why the file's fields or custom functions are not offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The file's names (its schema) are not available.
    NoNames,
    /// No context occurrence, or one the file does not have: unqualified
    /// field names cannot be read (`Occurrence::` names still can).
    NoContext,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unavailable::NoNames => "no_names",
            Unavailable::NoContext => "no_context",
        }
    }
}

/// The completion at a cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The characters choosing a candidate replaces: the typed prefix,
    /// which ends at the cursor. Text after the cursor is never replaced.
    pub replace: Range<usize>,
    pub prefix: String,
    /// Best first: an exact name, then alphabetically, then by kind.
    pub candidates: Vec<Candidate>,
    pub refused: Option<Refusal>,
    /// Whether the file's fields and custom functions could be offered.
    pub fields: Option<Unavailable>,
    pub custom_functions: Option<Unavailable>,
}

impl Completion {
    /// Write `candidate` over the prefix in `text`; the selection after it.
    pub fn apply(&self, candidate: &Candidate, text: &mut String) -> Range<usize> {
        candidate.insertion.apply(text, self.replace.clone())
    }
}

/// A cursor past the end of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorOutOfRange {
    pub cursor: usize,
    /// The text's length in characters.
    pub length: usize,
}

/// The completion of `text` at character `cursor`, for the file `schema`
/// (`None`: FileMaker's functions only).
pub fn complete(
    text: &str,
    cursor: usize,
    scope: Scope,
    schema: Option<&CalcSchema>,
) -> Result<Completion, CursorOutOfRange> {
    let length = text.chars().count();
    if cursor > length {
        return Err(CursorOutOfRange { cursor, length });
    }
    let at = super::byte_index(text, cursor);
    let context = scope
        .occurrence
        .filter(|o| schema.is_some_and(|s| s.occurrence(*o).is_some()));
    let mut out = Completion {
        replace: cursor..cursor,
        prefix: String::new(),
        candidates: Vec::new(),
        refused: None,
        fields: match schema {
            None => Some(Unavailable::NoNames),
            Some(_) if context.is_none() => Some(Unavailable::NoContext),
            Some(_) => None,
        },
        custom_functions: schema.is_none().then_some(Unavailable::NoNames),
    };
    // The piece the cursor ends or is inside of.
    let names: &dyn Names = match schema {
        Some(s) => s.names(),
        None => &NoNames,
    };
    let comma = schema.is_some_and(|s| s.locale.decimal_separator == ',');
    let mut found: Option<(Range<usize>, Class, Piece)> = None;
    scan::scan(text, comma, names, |range, class, piece| {
        if found.is_none() && range.start < at && at <= range.end {
            found = Some((range, class, piece));
        }
    });
    let Some((range, class, piece)) = found else {
        out.refused = Some(Refusal::NoName);
        return Ok(out);
    };
    let inside = at < range.end || piece == Piece::Open;
    let selector = match (class, piece) {
        (Class::Text, _) if inside => {
            out.refused = Some(Refusal::InText);
            return Ok(out);
        }
        (Class::Comment, _) if inside => {
            out.refused = Some(Refusal::InComment);
            return Ok(out);
        }
        (_, Piece::Name { selector }) => selector,
        _ => {
            out.refused = Some(Refusal::NoName);
            return Ok(out);
        }
    };
    if text[at..].chars().next().is_some_and(scan::is_name_char) {
        out.refused = Some(Refusal::InsideName);
        return Ok(out);
    }
    // A name runs over the spaces between its words (`First Na`): the
    // whole run first, then from each later word.
    let run = &text[range.start..at];
    let starts = std::iter::once(0).chain(run.char_indices().filter_map(|(i, c)| {
        let before = run[..i].chars().next_back();
        (scan::is_name_char(c) && before.is_some_and(scan::is_whitespace)).then_some(i)
    }));
    let mut chosen = (0, Vec::new());
    for start in starts {
        let found = candidates(&run[start..], selector, scope, context, schema);
        if !found.is_empty() {
            chosen = (start, found);
            break;
        }
    }
    let (start, candidates) = chosen;
    let start = range.start + start;
    out.replace = text[..start].chars().count()..cursor;
    out.prefix = text[start..at].to_owned();
    out.candidates = candidates;
    Ok(out)
}

/// Whether `name` starts with `prefix` (lowercased), ignoring case.
fn matches(name: &str, prefix: &str) -> bool {
    name.to_lowercase().starts_with(prefix)
}

fn function(entry: &Entry, kind: CandidateKind) -> Candidate {
    Candidate {
        kind,
        name: entry.name.clone(),
        label: entry.signature.clone(),
        insertion: entry.insertion(),
        field: None,
        category: functions::categories()
            .get(entry.category)
            .map(String::as_str)
            .filter(|c| !c.is_empty()),
    }
}

fn field(schema: &CalcSchema, occurrence: u64, id: u64, name: &str, qualified: bool) -> Candidate {
    let reference = field_reference(schema, occurrence, id, qualified);
    Candidate {
        kind: CandidateKind::Field,
        name: name.to_owned(),
        label: reference.clone(),
        insertion: Insertion::caret_after(reference),
        field: Some((occurrence, id)),
        category: None,
    }
}

/// The candidates for `prefix`, best first.
fn candidates(
    prefix: &str,
    selector: bool,
    scope: Scope,
    context: Option<u64>,
    schema: Option<&CalcSchema>,
) -> Vec<Candidate> {
    let lower = prefix.to_lowercase();
    let mut out: Vec<Candidate> = Vec::new();
    if selector {
        // `Get ( Acc`: the selectors, inserted bare.
        for entry in functions::catalog() {
            if let Some(name) = entry.selector().filter(|s| matches(s, &lower)) {
                out.push(Candidate {
                    name: name.to_owned(),
                    label: name.to_owned(),
                    insertion: Insertion::caret_after(name),
                    ..function(entry, CandidateKind::Selector)
                });
            }
        }
    } else if let Some((occurrence, field_prefix)) = prefix.rsplit_once("::") {
        // `Contacts::Fi`: the fields of that occurrence, qualified.
        let field_prefix = field_prefix.to_lowercase();
        if let Some(schema) = schema {
            if let Some(o) = schema.occurrence_named(occurrence.trim()) {
                for f in schema
                    .table_of(o.id)
                    .into_iter()
                    .flat_map(|t| t.fields.values())
                {
                    if matches(&f.name, &field_prefix) {
                        out.push(field(schema, o.id, f.id, &f.name, true));
                    }
                }
            }
        }
    } else {
        if let (Some(schema), Some(occurrence)) = (schema, context) {
            for f in schema
                .table_of(occurrence)
                .into_iter()
                .flat_map(|t| t.fields.values())
            {
                if matches(&f.name, &lower) {
                    out.push(field(schema, occurrence, f.id, &f.name, !scope.unqualified));
                }
            }
        }
        for entry in functions::custom_functions(schema) {
            if entry.key.starts_with(&lower) {
                out.push(function(&entry, CandidateKind::CustomFunction));
            }
        }
        for entry in functions::catalog() {
            if entry.key.starts_with(&lower) {
                let kind = if entry.is_get() {
                    CandidateKind::GetFunction
                } else {
                    CandidateKind::Function
                };
                out.push(function(entry, kind));
            }
        }
    }
    out.sort_by_cached_key(|c| {
        let key = c.name.to_lowercase();
        (key != lower, key, c.kind)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::{Locale, ValueType};
    use crate::host::FieldDef;

    /// Completion reads the name the scanner reads before the cursor,
    /// replaces only that prefix (whatever characters come before or
    /// after it), and offers the file's names only from its schema.
    #[test]
    fn completion_replaces_only_the_typed_name_and_keeps_the_rest() {
        let mut schema = CalcSchema::new("Contacts", Locale::en_us());
        schema.add_table(0x8002, "Contacts");
        schema.add_field(0x8002, FieldDef::data(1, "First Name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(2, "Lead Source", ValueType::Text));
        schema.add_occurrence(0xd00002, "Contacts", 0x8002);
        let scope = Scope {
            occurrence: Some(0xd00002),
            unqualified: false,
        };
        let at = |text: &str, cursor: usize| complete(text, cursor, scope, Some(&schema)).unwrap();

        // Characters outside the BMP before the name, text after the cursor.
        let mut text = "\"😀é\" & Le ; x".to_owned();
        let completion = at(&text, 9);
        assert_eq!(
            (completion.replace.clone(), completion.prefix.as_str()),
            (7..9, "Le")
        );
        let names: Vec<&str> = completion
            .candidates
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names[..3], ["Lead Source", "Left", "LeftValues"]);
        assert_eq!(completion.candidates[0].label, "Contacts::Lead Source");
        let left = &completion.candidates[1];
        assert_eq!(left.kind, CandidateKind::Function);
        let selection = completion.apply(left, &mut text);
        assert_eq!(text, "\"😀é\" & Left ( text ; numberOfCharacters ) ; x");
        assert_eq!(
            text.chars()
                .skip(selection.start)
                .take(selection.len())
                .collect::<String>(),
            "text"
        );

        // A name of several words, a qualified field, a Get selector.
        let completion = at("First Na", 8);
        assert_eq!(
            (completion.replace, completion.candidates[0].label.as_str()),
            (0..8, "Contacts::First Name")
        );
        assert_eq!(
            at("Contacts::fi", 12).candidates[0].label,
            "Contacts::First Name"
        );
        let completion = at("Get ( AccountN", 14);
        assert_eq!(completion.candidates[0].kind, CandidateKind::Selector);
        assert_eq!(completion.candidates[0].insertion.text, "AccountName");

        // Nothing inside text, comments or a word, or after a space.
        assert_eq!(at("\"Le", 3).refused, Some(Refusal::InText));
        assert_eq!(at("1 // Le", 7).refused, Some(Refusal::InComment));
        assert_eq!(at("Left", 2).refused, Some(Refusal::InsideName));
        assert_eq!(at("Le ", 3).refused, Some(Refusal::NoName));
        assert_eq!(at("\"a\"", 3).refused, Some(Refusal::NoName));

        // Without the file's names: FileMaker's functions only, and why.
        let bare = complete("Le", 2, Scope::default(), None).unwrap();
        assert_eq!(bare.fields, Some(Unavailable::NoNames));
        assert_eq!(bare.custom_functions, Some(Unavailable::NoNames));
        assert!(bare
            .candidates
            .iter()
            .all(|c| c.kind == CandidateKind::Function));
        assert!(complete("Le", 3, Scope::default(), None).is_err());
    }
}
