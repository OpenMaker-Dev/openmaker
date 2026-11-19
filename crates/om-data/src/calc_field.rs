//! A calculation field's definition as FileMaker Pro 26's Specify
//! Calculation shows it: the occurrence it is evaluated from, the formula,
//! "Calculation result is", "Number of repetitions" and "Do not evaluate if
//! all referenced fields are empty"; and its Storage Options: global
//! storage, indexing and "Do not store calculation results". They are read
//! from the field's 26-byte definition `[T, 3, 5, F, 02]` and its formula
//! `[T, 3, 5, F, 05]`, and written back by [`CalculationField::encode`]
//! (docs/re/schema-path.md 5.5, 5.10).

use om_formula::{NameTable, Names};

use crate::field_options::{Indexing, VALUE_INDEX, WORD_INDEX};
use crate::FieldKind;

/// Option bits of a calculation's definition (schema-path.md 5.5, 5.10).
const ALWAYS_EVALUATE: u32 = 0x20;
const GLOBAL: u32 = 0x1_0000;
const UNSTORED: u32 = 0x2_0000;
const NO_AUTOMATIC_INDEX: u32 = 0x4000_0000;
/// The bits this module edits; FileMaker's own bits (such as `0x80000`,
/// which the engine sets on a formula without field inputs) are kept or
/// set by the caller.
const EDITED: u32 =
    ALWAYS_EVALUATE | GLOBAL | UNSTORED | VALUE_INDEX | WORD_INDEX | NO_AUTOMATIC_INDEX;

/// Definition offsets (schema-path.md 5.5).
const KIND: usize = 0;
const TYPE: usize = 1;
/// `mAccessLevel`: 1 on a calculation that is not stored (unstored or
/// global), 0 otherwise (5.10, c05, c10, c16).
const ACCESS: usize = 2;
const LANGUAGE: std::ops::Range<usize> = 4..8;
const OPTIONS: std::ops::Range<usize> = 8..12;
const REPETITIONS: std::ops::Range<usize> = 24..26;

/// A calculation field (definition kind 2).
pub const KIND_CALCULATION: u8 = 2;

/// The most repetitions FileMaker Pro accepts.
pub const MAX_REPETITIONS: u16 = 32000;

/// Storage Options for a calculation field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalcStorage {
    /// "Use global storage (one value for all records)", option `0x10000`.
    pub global: bool,
    /// "Do not store calculation results -- recalculate when needed",
    /// option `0x20000`.
    pub unstored: bool,
    pub indexing: Indexing,
    /// "Automatically create indexes as needed": option `0x40000000` clear.
    pub automatic_indexes: bool,
    /// The index language, definition bytes 4-7 (21 English).
    pub language: u32,
}

impl Default for CalcStorage {
    fn default() -> Self {
        CalcStorage {
            global: false,
            unstored: false,
            indexing: Indexing::None,
            automatic_indexes: true,
            language: 21,
        }
    }
}

/// What Specify Calculation edits for a calculation field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalculationField {
    /// The formula as Specify Calculation shows it.
    pub formula: String,
    /// The table occurrence the formula is evaluated from ("Evaluate this
    /// calculation from the context of"); its unqualified field names are
    /// that occurrence's table's.
    pub context: u64,
    /// "Calculation result is".
    pub result: FieldKind,
    /// "Number of repetitions", `mMaxReps`.
    pub repetitions: u16,
    /// "Do not evaluate if all referenced fields are empty": option `0x20`
    /// ("always evaluate") clear.
    pub skip_when_empty: bool,
    pub storage: CalcStorage,
}

impl CalculationField {
    /// A new calculation as Specify Calculation opens for it: a text
    /// result, one repetition, "Do not evaluate if all referenced fields
    /// are empty" checked, stored, no index.
    pub fn new(formula: impl Into<String>, context: u64) -> Self {
        CalculationField {
            formula: formula.into(),
            context,
            result: FieldKind::Text,
            repetitions: 1,
            skip_when_empty: true,
            storage: CalcStorage::default(),
        }
    }

    /// Read a calculation field's options from its 26-byte definition; the
    /// formula's text and context come from member `05`.
    pub fn decode(definition: &[u8], formula: String, context: u64) -> Option<Self> {
        if definition.len() != 26 || definition[KIND] != KIND_CALCULATION {
            return None;
        }
        let options = u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes"));
        let result = data_type(definition[TYPE])?;
        let text = result == FieldKind::Text;
        let indexing = if options & (VALUE_INDEX | WORD_INDEX) == 0 {
            Indexing::None
        } else if options & WORD_INDEX != 0 || !text {
            Indexing::All
        } else {
            Indexing::Minimal
        };
        Some(CalculationField {
            formula,
            context,
            result,
            repetitions: u16::from_be_bytes(definition[REPETITIONS].try_into().expect("2 bytes")),
            skip_when_empty: options & ALWAYS_EVALUATE == 0,
            storage: CalcStorage {
                global: options & GLOBAL != 0,
                unstored: options & UNSTORED != 0,
                indexing,
                automatic_indexes: options & NO_AUTOMATIC_INDEX == 0,
                language: u32::from_be_bytes(definition[LANGUAGE].try_into().expect("4 bytes")),
            },
        })
    }

    /// Whether the result is stored in each record: not unstored and not
    /// global.
    pub fn stored(&self) -> bool {
        !self.storage.unstored && !self.storage.global
    }

    /// Why FileMaker Pro's dialogs would not produce these options, if
    /// they would not: an index on a value that is not stored or on a
    /// container, Minimal indexing on another result than text, global
    /// storage with "Do not store", or repetitions outside 1-32000.
    pub fn check(&self) -> Result<(), String> {
        if !(1..=MAX_REPETITIONS).contains(&self.repetitions) {
            return Err(format!(
                "The number of repetitions must be from 1 to {MAX_REPETITIONS}."
            ));
        }
        if self.storage.global && self.storage.unstored {
            return Err(
                "A calculation with global storage cannot also be recalculated when needed.".into(),
            );
        }
        if self.storage.indexing != Indexing::None {
            if !self.stored() {
                return Err("Only a stored calculation result can be indexed.".into());
            }
            if self.result == FieldKind::Container {
                return Err("A container result cannot be indexed.".into());
            }
            if self.storage.indexing == Indexing::Minimal && self.result != FieldKind::Text {
                return Err("Minimal indexing is offered for a text result only.".into());
            }
        }
        Ok(())
    }

    /// Write these options into `definition` (26 bytes): the kind, result
    /// type, access byte, index language, the option bits this module
    /// names and the repetitions. Other bits are kept.
    pub fn encode(&self, definition: &mut [u8]) {
        assert_eq!(definition.len(), 26, "a field definition has 26 bytes");
        definition[KIND] = KIND_CALCULATION;
        definition[TYPE] = type_code(self.result);
        definition[ACCESS] = u8::from(!self.stored());
        definition[LANGUAGE].copy_from_slice(&self.storage.language.to_be_bytes());
        let mut options =
            u32::from_be_bytes(definition[OPTIONS].try_into().expect("4 bytes")) & !EDITED;
        if !self.skip_when_empty {
            options |= ALWAYS_EVALUATE;
        }
        if self.storage.global {
            options |= GLOBAL;
        }
        if self.storage.unstored {
            options |= UNSTORED;
        }
        options |= match self.storage.indexing {
            Indexing::None => 0,
            Indexing::All if self.result == FieldKind::Text => VALUE_INDEX | WORD_INDEX,
            Indexing::Minimal | Indexing::All => VALUE_INDEX,
        };
        if !self.storage.automatic_indexes {
            options |= NO_AUTOMATIC_INDEX;
        }
        definition[OPTIONS].copy_from_slice(&options.to_be_bytes());
        definition[REPETITIONS].copy_from_slice(&self.repetitions.to_be_bytes());
    }
}

/// What a schema's formulas are compiled and shown with besides its own
/// occurrences and fields: the file's decimal separator and its custom and
/// plug-in functions (from the file's calculation schema); and the file's
/// value lists, which field validation may name.
#[derive(Clone, Debug, Default)]
pub struct FormulaSettings {
    /// The file's locale writes decimals with a comma (`1,5`).
    pub decimal_comma: bool,
    /// Custom and plug-in functions only ([`NameTable::functions_only`]).
    pub functions: NameTable,
    /// Each value list's id and, when OpenMaker's record rules cannot check
    /// values against it, why ([`crate::rules::value_list_problem`]).
    pub value_lists: Vec<(u64, Option<String>)>,
    /// The file's language (a `Locale::Type`), which sorts fields that
    /// name none; 0 when unknown.
    pub language: u32,
    /// The fields value lists take their values from (the first and the
    /// second field), as (table id, field id): a field deletion leaves them
    /// alone.
    pub value_list_fields: Vec<(u64, u64)>,
}

impl FormulaSettings {
    pub fn from_calc_schema(schema: &crate::host::CalcSchema) -> Self {
        FormulaSettings {
            decimal_comma: schema.locale.decimal_separator == ',',
            functions: schema.names().functions_only(),
            value_lists: schema
                .value_lists()
                .iter()
                .map(|list| (list.id, crate::rules::value_list_problem(list)))
                .collect(),
            language: u32::from(schema.locale.language.id()),
            value_list_fields: schema
                .value_lists()
                .iter()
                .filter_map(|list| match &list.source {
                    crate::meta::ValueSource::Field {
                        primary, secondary, ..
                    } => Some([Some(primary), secondary.as_ref()]),
                    _ => None,
                })
                .flatten()
                .flatten()
                .filter_map(|spec| {
                    let table = schema.table_of(spec.occurrence?)?;
                    Some((table.id, spec.field))
                })
                .collect(),
        }
    }

    /// These functions with `schema`'s occurrences and fields.
    pub fn names(&self, schema: &crate::DatabaseSchema) -> NameTable {
        let mut names = self.functions.clone();
        for table in &schema.tables {
            for field in &table.fields {
                names.add_field(table.id, field.id, &field.name);
                names.set_summary_field(
                    table.id,
                    field.id,
                    field.kind == crate::SchemaFieldKind::Summary,
                );
            }
        }
        for occurrence in &schema.occurrences {
            if let Some(table) = occurrence.table_id {
                names.add_occurrence(occurrence.id, &occurrence.name, table);
            }
        }
        names
    }
}

/// A field formula's stored streams: the formatted stream (child `05`) and
/// the Polish stream (child `06`) of member `05` (calc-encoding.md 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledFormula {
    pub formatted: Vec<u8>,
    pub polish: Vec<u8>,
    pub expr: om_formula::Expr,
}

/// Compile a calculation field's formula from occurrence `context`, as
/// Specify Calculation's OK does: unqualified names are the context's
/// fields and `Self` is allowed. An error is FileMaker's code and message,
/// with the range of the text it concerns.
pub fn compile(
    text: &str,
    context: u64,
    names: &dyn Names,
    decimal_comma: bool,
) -> Result<CompiledFormula, crate::DataError> {
    let ctx = om_formula::Context {
        occurrence: Some(context),
        support_self: true,
        decimal_comma,
        ..om_formula::Context::new(names)
    };
    match om_formula::compile(text, &ctx) {
        Ok(compiled) => Ok(CompiledFormula {
            formatted: om_formula::formatted::encode(&compiled.tokens),
            polish: om_formula::polish::encode(&compiled.expr),
            expr: compiled.expr,
        }),
        Err(error) => {
            let code = i32::from(error.code.number());
            let message = crate::runtime::ErrorCode(code)
                .message()
                .unwrap_or("There is an error in this calculation.");
            Err(crate::DataError::with_code(code, message))
        }
    }
}

/// A stored formula's text as Specify Calculation shows it: its formatted
/// stream printed with the current names. `None` when the text was
/// stripped (calc-encoding.md 1) or does not decode: such a formula is
/// shown, not edited.
pub fn formula_text(formatted: &[u8], names: &dyn Names, decimal_comma: bool) -> Option<String> {
    let ctx = om_formula::Context {
        decimal_comma,
        ..om_formula::Context::new(names)
    };
    let tokens = om_formula::formatted::decode(formatted).ok()?;
    Some(om_formula::print(&tokens, &ctx))
}

/// The definition's data type byte (schema-path.md 5.5).
pub fn type_code(data_type: FieldKind) -> u8 {
    match data_type {
        FieldKind::Text => 1,
        FieldKind::Number => 2,
        FieldKind::Date => 3,
        FieldKind::Time => 4,
        FieldKind::Timestamp => 5,
        FieldKind::Container => 6,
    }
}

/// The data type a definition's type byte names.
pub fn data_type(code: u8) -> Option<FieldKind> {
    Some(match code {
        1 => FieldKind::Text,
        2 => FieldKind::Number,
        3 => FieldKind::Date,
        4 => FieldKind::Time,
        5 => FieldKind::Timestamp,
        6 => FieldKind::Container,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The definitions FileMaker Server's engine wrote for calculation
    /// fields it added to the Contacts starter solution (schema-path.md
    /// 5.10: c01, c02, c05, c10, c11, c12, c16, c19, c20) decode to the
    /// options they were made with and encode back byte for byte from the
    /// definition a new field starts from.
    #[test]
    fn definitions_the_engine_wrote_round_trip() {
        let new = |result| CalculationField {
            result,
            ..CalculationField::new("", 0xd00002)
        };
        let cases = [
            (
                "0201000000000015000000000000000000000001000000000001",
                new(FieldKind::Text),
            ),
            (
                "0201000000000015000000200000000000000001000000000001",
                CalculationField {
                    skip_when_empty: false,
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201010000000015000200000000000000000001000000000001",
                CalculationField {
                    storage: CalcStorage {
                        unstored: true,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201010000000015000100000000000000000001000000000001",
                CalculationField {
                    storage: CalcStorage {
                        global: true,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201000000000015a00000000000000000000001000000000001",
                CalculationField {
                    storage: CalcStorage {
                        indexing: Indexing::All,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201000000000015000000000000000000000001000000000003",
                CalculationField {
                    repetitions: 3,
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201010000000015000200200000000000000001000000000001",
                CalculationField {
                    skip_when_empty: false,
                    storage: CalcStorage {
                        unstored: true,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201000000000015800000000000000000000001000000000001",
                CalculationField {
                    storage: CalcStorage {
                        indexing: Indexing::Minimal,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0201000000000015400000000000000000000001000000000001",
                CalculationField {
                    storage: CalcStorage {
                        automatic_indexes: false,
                        ..CalcStorage::default()
                    },
                    ..new(FieldKind::Text)
                },
            ),
            (
                "0202000000000015000000000000000000000001000000000001",
                new(FieldKind::Number),
            ),
        ];
        for (stored, options) in cases {
            let stored = hex(stored);
            let decoded = CalculationField::decode(&stored, String::new(), 0xd00002).unwrap();
            assert_eq!(decoded, options, "{}", stored.len());
            let mut definition = crate::field_catalog::definition(
                crate::field_catalog::ScalarType::Text,
                options.storage.language,
            )
            .to_vec();
            options.encode(&mut definition);
            assert_eq!(definition, stored);
        }
    }
}
