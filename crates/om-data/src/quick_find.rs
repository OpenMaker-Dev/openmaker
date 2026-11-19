//! Quick Find (query-program.md 6.6): the fields a layout's Quick Find
//! searches, and the find requests a search text becomes. The toolbar's
//! search box and Perform Quick Find (150) both run it; the provider
//! applies the requests ([`FoundSetChange::QuickFind`]).
//!
//! The fields are every field the layout shows in a field object that is
//! included for Quick Find (layout-model.md 5.4.1), its own and related
//! ones, in portals or not, and the fields of its merge fields; a field
//! with several objects is searched when any of them is included. Summary,
//! container and global fields are never searched (Claris "Configuring
//! quick find"). Each word of the text, or each `"quoted phrase"`, must be
//! found in one of the fields; a word is a criterion only on fields whose
//! type can read it as a plain value, and it carries no find operator
//! but the quotes ("Performing a quick find in Browse mode").

use om_calc::ValueType;
use om_collate::DateOrder;
use om_hbam::DataType;
use om_query::criteria::{Criterion, FindContext, Ymd, MAX_CRITERION_CHARS};

use crate::found::{code, FindRequest, FoundSetChange};
use crate::host::{CalcSchema, FieldClass, FieldDef};
use crate::DataError;

/// The most requests one Quick Find makes (a request per word and field):
/// OpenMaker's own bound on what a search text costs, as
/// `MAX_CRITERION_CHARS` bounds a criterion. FileMaker's, if any, is not
/// measured.
pub const MAX_REQUESTS: usize = 10_000;

/// A field a Quick Find searches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickFindField {
    /// As a find request names it: the bare name in the layout's own
    /// occurrence, `Occurrence::Field` in another.
    pub name: String,
    pub data_type: DataType,
}

/// One term of a search text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Term {
    text: String,
    /// Typed in double quotation marks: a literal phrase.
    phrase: bool,
}

/// The fields Quick Find searches on `layout`, in the order the layout
/// stores their objects; `None` when the layout has Quick Find turned off.
/// Merge fields count whether the layout names them (as
/// `DataProvider::render_layout` resolves them) or holds only their
/// catalog keys (a layout as stored, which the server reads).
pub fn fields(layout: &om_layout::Layout, schema: &CalcSchema) -> Option<Vec<QuickFindField>> {
    if !layout
        .def
        .as_ref()
        .is_none_or(om_layout::LayoutDef::quick_find)
    {
        return None;
    }
    let own = layout
        .def
        .as_ref()
        .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
    let mut out: Vec<QuickFindField> = Vec::new();
    let mut add = |occurrence: u64, field: &FieldDef| {
        if field.class == FieldClass::Summary
            || field.global
            || field.value_type == ValueType::Container
        {
            return;
        }
        let name = if Some(occurrence) == own {
            field.name.clone()
        } else {
            // A related occurrence the layout's cannot reach (another
            // file's, or unrelated) gives a find no records to search.
            let reachable = own.is_some_and(|own| schema.path(own, occurrence).is_some());
            let Some(occurrence) = schema.occurrence(occurrence).filter(|_| reachable) else {
                return;
            };
            format!("{}::{}", occurrence.name, field.name)
        };
        if !out.iter().any(|f| f.name == name) {
            out.push(QuickFindField {
                name,
                data_type: data_type(field.value_type),
            });
        }
    };
    for object in &layout.objects.objects {
        if let Some(field_obj) = &object.field_obj {
            let Some(spec) = field_obj
                .field_spec
                .as_ref()
                .filter(|_| field_obj.quick_find())
            else {
                continue;
            };
            let field = om_layout::occurrence_id(spec.table_key).and_then(|occurrence| {
                Some((occurrence, schema.field(occurrence, spec.field_id()?)?))
            });
            if let Some((occurrence, field)) = field {
                add(occurrence, field);
            }
        }
        for run in &object.text_runs {
            let om_layout::TextRun::MergeField(name) = run else {
                continue;
            };
            let field = match name.split_once("::") {
                Some((occurrence, field)) => schema
                    .occurrence_named(occurrence)
                    .and_then(|o| Some((o.id, named_field(schema, o.id, field)?))),
                None => own.and_then(|own| Some((own, named_field(schema, own, name)?))),
            };
            if let Some((occurrence, field)) = field {
                add(occurrence, field);
            }
        }
        for spec in object.merge_field_specs() {
            let occurrence = schema.occurrence(spec.table_key);
            if let (Some(occurrence), Some(field)) =
                (occurrence, schema.field(spec.table_key, spec.field_key))
            {
                add(occurrence.id, field);
            }
        }
    }
    Some(out)
}

fn named_field<'s>(schema: &'s CalcSchema, occurrence: u64, name: &str) -> Option<&'s FieldDef> {
    schema
        .table_of(occurrence)?
        .fields
        .values()
        .find(|field| field.name == name)
}

fn data_type(value_type: ValueType) -> DataType {
    match value_type {
        ValueType::Text => DataType::Text,
        ValueType::Number => DataType::Number,
        ValueType::Date => DataType::Date,
        ValueType::Time => DataType::Time,
        ValueType::Timestamp => DataType::Timestamp,
        ValueType::Container => DataType::Container,
    }
}

/// The change a Quick Find for `text` over `fields` makes: for each term,
/// one find request per field that can read it, dates read as Find mode
/// reads them (month/day/year, `today` for a date without a year). A text
/// without terms is FileMaker's 400 ("Find criteria are empty"); one
/// longer than `MAX_CRITERION_CHARS`, or that would make more than
/// [`MAX_REQUESTS`] requests, is refused (3) before any is made.
pub fn change(
    text: &str,
    fields: &[QuickFindField],
    today: Ymd,
) -> Result<FoundSetChange, DataError> {
    let text = text.trim();
    if text.chars().count() > MAX_CRITERION_CHARS {
        return Err(too_long());
    }
    let terms = terms(text);
    if terms.is_empty() {
        return Err(DataError::with_code(
            code::EMPTY_CRITERIA,
            "Provide at least one word to find records.",
        ));
    }
    if terms.len().saturating_mul(fields.len()) > MAX_REQUESTS {
        return Err(too_long());
    }
    let context = FindContext::new(DateOrder::MonthDayYear, Some(today));
    let context = &context;
    Ok(FoundSetChange::QuickFind(
        terms
            .iter()
            .map(|term| {
                fields
                    .iter()
                    .filter_map(|field| {
                        let criterion = criterion(term, field.data_type, context)?;
                        let mut request = FindRequest::default();
                        request.set(&field.name, criterion);
                        Some(request)
                    })
                    .collect()
            })
            .collect(),
    ))
}

fn too_long() -> DataError {
    DataError::with_code(
        code::UNSUPPORTED,
        "The Quick Find text has more words than OpenMaker searches at once.",
    )
}

/// The words and quoted phrases of a search text. A quote left open runs
/// to the end of the text.
fn terms(text: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut phrase = false;
    for c in text.chars() {
        if c == '"' || (!phrase && c.is_whitespace()) {
            let text = std::mem::take(&mut current);
            if !text.trim().is_empty() {
                terms.push(Term {
                    text: if phrase { text } else { text.trim().to_owned() },
                    phrase,
                });
            }
            if c == '"' {
                phrase = !phrase;
            }
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        terms.push(Term {
            text: if phrase {
                current
            } else {
                current.trim().to_owned()
            },
            phrase,
        });
    }
    terms
}

/// The criterion `term` is on a field of `data_type`, or `None` when that
/// type cannot read it as a plain value. Text gets every character
/// literally (`\` before each find operator); other types take only a
/// number, date or time typed with digits and separators.
fn criterion(term: &Term, data_type: DataType, context: &FindContext) -> Option<String> {
    let criterion = match data_type {
        DataType::Text => {
            let mut out = String::with_capacity(term.text.len() + 2);
            if term.phrase {
                out.push('"');
            }
            for c in term.text.chars() {
                let operator = if term.phrase {
                    matches!(c, '\\' | '"')
                } else {
                    crate::found::is_find_operator(c)
                };
                if operator {
                    out.push('\\');
                }
                out.push(c);
            }
            if term.phrase {
                out.push('"');
            }
            out
        }
        DataType::Number | DataType::Date | DataType::Time | DataType::Timestamp => {
            let separators: &[char] = match data_type {
                DataType::Number => &['.', ',', '-'],
                _ => &['/', '-', '.', ':'],
            };
            let plain = !term.phrase
                && term.text.chars().any(|c| c.is_ascii_digit())
                && term
                    .text
                    .chars()
                    .all(|c| c.is_ascii_digit() || separators.contains(&c))
                && !term.text.contains("..")
                && !term.text.contains("//");
            if !plain {
                return None;
            }
            term.text.clone()
        }
        _ => return None,
    };
    matches!(
        Criterion::parse(&criterion, data_type, context),
        Ok(Some(_))
    )
    .then_some(criterion)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criteria(text: &str, fields: &[QuickFindField]) -> Vec<Vec<String>> {
        match change(text, fields, (2026, 11, 13)) {
            Ok(FoundSetChange::QuickFind(groups)) => groups
                .into_iter()
                .map(|group| {
                    group
                        .into_iter()
                        .flat_map(|r| r.criteria)
                        .map(|(field, criterion)| format!("{field}: {criterion}"))
                        .collect()
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn each_word_or_phrase_is_a_literal_criterion_on_the_fields_that_read_it() {
        let field = |name: &str, data_type| QuickFindField {
            name: name.into(),
            data_type,
        };
        let fields = [
            field("Name", DataType::Text),
            field("Due", DataType::Date),
            field("Units", DataType::Number),
        ];
        assert_eq!(
            criteria(r#"Alpha  "two words" 12 1/2/2026 a*b Beta" open"#, &fields),
            [
                vec!["Name: Alpha"],
                vec![r#"Name: "two words""#],
                vec!["Name: 12", "Units: 12"],
                vec![r"Name: 1\/2\/2026", "Due: 1/2/2026"],
                vec![r"Name: a\*b"],
                vec!["Name: Beta"],
                vec![r#"Name: " open""#],
            ]
        );
        assert!(criteria("  \"\" ", &fields).is_empty());
        // A text that would make too many requests is refused, not built.
        let words = vec!["w"; MAX_REQUESTS / fields.len() + 1].join(" ");
        assert_eq!(
            change(&words, &fields, (2026, 11, 13)).unwrap_err().code,
            Some(3)
        );
    }
}
