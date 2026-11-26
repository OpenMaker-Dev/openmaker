//! Local Text auto-entry through the measured one-argument GetField helper.

use om_calc::ValueType;
use om_formula::{Expr, Field, Opcode};
use om_hbam::{DataType, FieldKind, Table};

use crate::host::{CalcSchema, FieldClass};

/// Its direct references are the trigger and the argument naming a field.
/// The value reached through the helper must not add a change trigger.
pub(super) fn classify(
    schema: &CalcSchema,
    table: &Table,
    occurrence: &[u8],
    expr: &Expr,
) -> Option<u64> {
    let Expr::Call {
        op: Opcode::CONCAT,
        args,
    } = expr
    else {
        return None;
    };
    let [Expr::Call {
        op: Opcode::CONCAT,
        args: prefix,
    }, Expr::CustomCall { function, args, .. }] = args.as_slice()
    else {
        return None;
    };
    let [Expr::Field(trigger), Expr::Text(separator)] = prefix.as_slice() else {
        return None;
    };
    let [Expr::Field(name)] = args.as_slice() else {
        return None;
    };
    let helper = schema.custom_function(function.id()?)?;
    if separator.style.is_some()
        || helper.parameters.len() != 1
        || !matches!(&helper.body, Expr::GetField { arg, need_field_ref: 0 }
            if matches!(arg.as_ref(), Expr::Local(0)))
    {
        return None;
    }
    let plain_text = |field: &Field| {
        if field.occurrence.0 != occurrence
            || field.repetition != 1
            || !matches!(field.flags, 0 | 4)
        {
            return None;
        }
        let id = field.field.id()?;
        let def = table.field(id)?;
        let options = def.decoded_options()?;
        (def.kind == FieldKind::Normal
            && def.data_type == DataType::Text
            && options.options == 0
            && options.validation_options == 0
            && options.max_repetitions == 1)
            .then_some(id)
    };
    plain_text(trigger)?;
    plain_text(name)
}

/// Use the same compiler as GetField, and permit only an unqualified,
/// same-occurrence scalar read of a plain Text field other than the output.
pub(super) fn can_read(schema: &CalcSchema, occurrence: u64, output: u64, name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let Some(table) = schema.table_of(occurrence) else {
        return false;
    };
    // Check for a field name before sending arbitrary record text to the
    // compiler. A qualified expression or computed target is unmeasured.
    let Some(target) = table.fields.values().find(|field| {
        field.id != output
            && field.name == name
            && field.class == FieldClass::Data
            && field.value_type == ValueType::Text
            && field.options == 0
            && field.validation_options == 0
            && field.repetitions == 1
    }) else {
        return false;
    };
    let mut context = om_formula::Context::new(schema.names());
    context.occurrence = Some(occurrence);
    om_formula::compile(name, &context).is_ok_and(|compiled| {
        matches!(compiled.expr, Expr::Field(field)
            if field.occurrence.id() == Some(occurrence)
                && field.field.id() == Some(target.id) && field.repetition == 1)
    })
}
