//! Local Text auto-entry through the measured one-argument GetField helper.

use std::collections::BTreeMap;

use om_calc::ValueType;
use om_formula::{Expr, Field, Opcode};
use om_hbam::{DataType, FieldKind, Table};

use crate::host::{CalcSchema, FieldClass, FieldDef};

use super::{error, Evaluation, Expression, PlannedField, RecordPlan, WriteMode, WriteSequence};
use crate::DataError;

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
    let Some(target) = table
        .fields
        .values()
        .find(|field| field.id != output && field.name == name && plain_input(field))
    else {
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

fn plain_input(field: &FieldDef) -> bool {
    field.class == FieldClass::Data
        && field.value_type == ValueType::Text
        && field.options == 0
        && field.validation_options == 0
        && field.repetitions == 1
}

impl RecordPlan {
    /// Is the local window required to retain Set Field order for this
    /// table, and does even a same-value assignment to this cell open it?
    pub fn ordered_script_entry(&self, cell: u64) -> (bool, bool) {
        let mut ordered = false;
        for (id, expression) in self.named_auto_entries() {
            if self.fields[&id].options.calculation_replaces_existing()
                && expression.dependencies.contains(&cell)
            {
                return (true, true);
            }
            ordered = true;
        }
        (ordered, false)
    }

    /// Calculated results that assigning the direct inputs may change.
    pub fn ordered_auto_entries<'a>(
        &'a self,
        changes: &'a BTreeMap<u64, Option<String>>,
    ) -> impl Iterator<Item = u64> + 'a {
        self.named_auto_entries()
            .filter(|(_, expr)| expr.dependencies.iter().any(|id| changes.contains_key(id)))
            .map(|(id, _)| id)
    }

    fn named_auto_entries(&self) -> impl Iterator<Item = (u64, &Expression)> {
        self.fields.iter().filter_map(|(id, field)| {
            field.named_auto_entry?;
            Some((*id, field.expression.as_ref()?))
        })
    }

    pub(super) fn ordered_named_value(
        &self,
        (id, field, expression): (u64, &PlannedField, &Expression),
        mode: WriteMode,
        existing: &BTreeMap<u64, String>,
        changes: &BTreeMap<u64, Option<String>>,
        session: &Evaluation,
        context: &super::WriteContext,
    ) -> Result<Option<String>, DataError> {
        let unsupported = || {
            DataError::with_code(
                error::UNAVAILABLE,
                format!(
                    "Field {}: GetField auto-entry requires ordered direct Set Field assignments.",
                    field.name
                ),
            )
        };
        let replacing = field.options.calculation_replaces_existing();
        if mode == WriteMode::Update
            && !replacing
            && existing.get(&id).is_some_and(|s| !s.is_empty())
        {
            return Ok(None);
        }
        let mut remaining = session.calculation_budget();
        let mut prefix = existing.clone();
        let mut result = if mode == WriteMode::Create {
            Some(
                self.evaluate_repetition(id, 1, expression, &prefix, session, &mut remaining)
                    .map_err(super::RunError::into_error)?,
            )
        } else {
            None
        };
        if changes.contains_key(&id) {
            return Err(unsupported());
        }
        if !expression
            .dependencies
            .iter()
            .any(|id| changes.contains_key(id))
        {
            return Ok(result);
        }
        if context.sequence == WriteSequence::Unmeasured {
            return Err(unsupported());
        }
        let assignments = context.assignments.as_deref().ok_or_else(unsupported)?;
        let inputs = self.schema.table(self.table).ok_or_else(unsupported)?;
        let is_input = |cell| inputs.fields.get(cell).is_some_and(plain_input);
        // Other rule families may add raw cells (on-creation serials, for
        // example). Only plain Text inputs can affect this formula. Every
        // such cell written must have a corresponding direct assignment.
        let replayed: BTreeMap<_, _> = assignments
            .iter()
            .filter(|(cell, _)| is_input(cell))
            .map(|(cell, text)| (cell, text))
            .collect();
        if replayed
            .into_iter()
            .ne(changes.iter().filter(|(cell, _)| is_input(cell)))
        {
            return Err(unsupported());
        }
        for (cell, value) in assignments {
            if !is_input(cell) {
                continue;
            }
            if let Some(text) = value.as_ref().filter(|text| !text.is_empty()) {
                prefix.insert(*cell, text.clone());
            } else {
                prefix.remove(cell);
            }
            if expression.dependencies.contains(cell)
                && (replacing || result.as_deref().is_none_or(str::is_empty))
            {
                result = Some(
                    self.evaluate_repetition(id, 1, expression, &prefix, session, &mut remaining)
                        .map_err(super::RunError::into_error)?,
                );
            }
        }
        Ok(result)
    }
}
