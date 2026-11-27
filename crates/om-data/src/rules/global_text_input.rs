//! Measured local global Text auto-entry from a plain global Text input.

use om_calc::Value;
use om_formula::{Expr, Formula};
use om_hbam::{DataType, Field, FieldKind, Table};

use crate::host::CalcSchema;
use crate::{DataError, GlobalChanges};

use super::error;

#[derive(Clone, Copy)]
pub(super) struct GlobalTextInput {
    pub(super) source: u64,
    echo: bool,
}

fn plain_global_text(field: &Field, word: u32) -> bool {
    field.kind == FieldKind::Normal
        && field.data_type == DataType::Text
        && field.decoded_options().is_some_and(|options| {
            options.options == word
                && options.validation_options == 0
                && options.max_repetitions == 1
        })
}

/// Both definitions in #1138 have one binding in the output's occurrence.
/// The calculation either reads it back or returns a literal empty Text.
pub(super) fn classify(
    table: &Table,
    schema: &CalcSchema,
    output: &Field,
    formula: &Formula,
) -> Option<GlobalTextInput> {
    if !plain_global_text(output, 0x0001_0088) {
        return None;
    }
    let occurrence = formula.occurrence()?;
    if schema.table_of(occurrence)?.id != table.id {
        return None;
    }
    let Expr::Let { bindings, body } = &formula.polish else {
        return None;
    };
    let [Expr::Field(input)] = bindings.as_slice() else {
        return None;
    };
    let source = input.field.id()?;
    // Flags 0 and 4 are the unqualified and qualified forms of a field
    // in the same occurrence (calc-encoding.md, "Field references").
    if input.occurrence.id() != Some(occurrence)
        || input.repetition != 1
        || !matches!(input.flags, 0 | 4)
        || !plain_global_text(table.field(source)?, 0x0001_0000)
    {
        return None;
    }
    let echo = match body.as_ref() {
        Expr::Local(0) => true,
        Expr::Text(text) if text.style.is_none() && text.value.is_empty() => false,
        _ => return None,
    };
    Some(GlobalTextInput { source, echo })
}

impl GlobalTextInput {
    fn result(self, text: &str) -> String {
        if self.echo {
            text.to_owned()
        } else {
            String::new()
        }
    }

    pub(super) fn result_from_session(self, value: Option<&Value>) -> Result<String, DataError> {
        match value {
            None => Ok(self.result("")),
            Some(Value::Text(text)) if text.is_plain() => Ok(self.result(text.as_str())),
            _ => Err(DataError::with_code(
                error::UNAVAILABLE,
                "Global Text auto-entry requires a plain Text input.",
            )),
        }
    }
}

/// Effects of an explicit assignment to this local global, computed before
/// publishing the input and its dependents together. A same-value write
/// still runs the calculation.
pub fn global_text_input_changes(
    schema: &CalcSchema,
    table: &Table,
    input: u64,
    text: &str,
) -> GlobalChanges {
    if !table
        .field(input)
        .is_some_and(|field| plain_global_text(field, 0x0001_0000))
    {
        return GlobalChanges::new();
    }
    let Some(definitions) = schema.table(table.id) else {
        return GlobalChanges::new();
    };
    table
        .fields
        .iter()
        .filter_map(|field| {
            let formula = definitions.fields.get(&field.id)?.auto_enter.as_deref()?;
            let rule = classify(table, schema, field, formula)?;
            (rule.source == input).then(|| ((field.id, 1), rule.result(text)))
        })
        .collect()
}
