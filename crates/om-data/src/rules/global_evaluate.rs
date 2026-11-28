//! Local global Text auto-entry that discards the result of Evaluate.

use om_formula::Expr;
use om_hbam::{DataType, Field, FieldKind, Table};

use crate::host::{CalcSchema, RecordValues};
use crate::{DataError, GlobalChanges};

use super::error;

fn plain_input(field: &Field) -> Option<u32> {
    let options = field.decoded_options()?;
    (field.kind == FieldKind::Normal
        && field.data_type == DataType::Text
        && matches!(options.options, 0 | 0x0001_0000)
        && options.validation_options == 0
        && options.max_repetitions == 1)
        .then_some(options.options)
}

/// A global rule of the form `Let ( result = Evaluate ( Input ) ; "" )`.
/// A plain local Text input can first be read at creation. A plain global
/// Text input can also be assigned without opening a record.
pub(super) fn classify<'a>(
    table: &Table,
    context: &[u8],
    polish: &'a Expr,
) -> Option<(u64, bool, &'a Expr)> {
    let Expr::Let { bindings, body } = polish else {
        return None;
    };
    let [Expr::Evaluate {
        args,
        need_field_ref: 0,
    }] = bindings.as_slice()
    else {
        return None;
    };
    let [Expr::Field(field)] = args.as_slice() else {
        return None;
    };
    if field.occurrence.0 != context
        || field.repetition != 1
        || !matches!(field.flags, 0 | 4)
        || !matches!(body.as_ref(), Expr::Text(text) if text.style.is_none() && text.value.is_empty())
    {
        return None;
    }
    let id = field.field.id()?;
    let options = plain_input(table.field(id)?)?;
    Some((id, options == 0x0001_0000, body.as_ref()))
}

/// An output whose formula reads the plain global being assigned.
pub struct GlobalEvaluateOutput {
    field: u64,
    replaces_existing: bool,
}

/// Find the same shape the record planner recognizes, from the file's
/// original auto-enter formulas. The global assignment never sees the
/// planner's reduced, empty result formula.
pub fn global_evaluate_outputs(
    schema: &CalcSchema,
    table: &Table,
    input: u64,
) -> Vec<GlobalEvaluateOutput> {
    if table.field(input).and_then(plain_input) != Some(0x0001_0000) {
        return Vec::new();
    }
    let Some(definitions) = schema.table(table.id) else {
        return Vec::new();
    };
    table
        .fields
        .iter()
        .filter_map(|field| {
            let options = field.decoded_options()?;
            if field.kind != FieldKind::Normal
                || field.data_type != DataType::Text
                || !matches!(options.options, 0x0001_0008 | 0x0001_0088)
                || options.validation_options != 0
                || options.max_repetitions != 1
            {
                return None;
            }
            let formula = definitions.fields.get(&field.id)?.auto_enter.as_ref()?;
            if schema.table_of(formula.occurrence()?)?.id != table.id {
                return None;
            }
            let (source, global, _) =
                classify(table, &formula.context.as_ref()?.0, &formula.polish)?;
            (global && source == input).then_some(GlobalEvaluateOutput {
                field: field.id,
                replaces_existing: options.calculation_replaces_existing(),
            })
        })
        .collect()
}

/// Stage all affected outputs before publishing the input. An output
/// that would need dynamic evaluation refuses the whole assignment.
pub fn global_evaluate_changes(
    outputs: Vec<GlobalEvaluateOutput>,
    globals: &RecordValues,
    input_empty: bool,
) -> Result<GlobalChanges, DataError> {
    let mut changes = GlobalChanges::new();
    for output in outputs {
        if clears_output(output.field, output.replaces_existing, globals, input_empty)? {
            changes.insert((output.field, 1), String::new());
        }
    }
    Ok(changes)
}

pub(super) fn clears_output(
    field: u64,
    replaces_existing: bool,
    globals: &RecordValues,
    input_empty: bool,
) -> Result<bool, DataError> {
    if !replaces_existing && globals.get(field, 1).is_some_and(|value| !value.is_empty()) {
        return Ok(false);
    }
    if input_empty {
        return Ok(true);
    }
    Err(DataError::with_code(
        error::UNAVAILABLE,
        "Auto-entry into a global would evaluate nonempty formula text.",
    ))
}
