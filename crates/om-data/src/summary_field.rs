//! What Options for Summary Field may define (file-metadata.md 5.1,
//! schema-path.md 5.11): which fields each kind of summary takes, the
//! summary's result type, and the options OpenMaker writes. A summary's
//! values are computed by [`crate::summary`].

use crate::meta::{summary_flag, SummaryDef, SummaryKind};
use crate::{FieldKind, SchemaField, SchemaFieldKind};

/// The data type a summary of `kind` over a field of type `data_type`
/// takes, or why that field cannot be summarized that way: totals,
/// averages, deviations and fractions summarize numbers, minimums and
/// maximums numbers, dates, times and timestamps (and keep their type), a
/// count and a list any field but a container.
pub fn kind_result(kind: SummaryKind, data_type: FieldKind) -> Result<FieldKind, String> {
    match (kind, data_type) {
        (_, FieldKind::Container) => Err("A container field cannot be summarized.".into()),
        (SummaryKind::Count, _) => Ok(FieldKind::Number),
        (SummaryKind::List, _) => Ok(FieldKind::Text),
        (
            SummaryKind::Total
            | SummaryKind::Average
            | SummaryKind::StdDeviation
            | SummaryKind::Fraction,
            FieldKind::Number,
        ) => Ok(FieldKind::Number),
        (
            SummaryKind::Minimum | SummaryKind::Maximum,
            FieldKind::Number | FieldKind::Date | FieldKind::Time | FieldKind::Timestamp,
        ) => Ok(data_type),
        (SummaryKind::Other(_), _) => Err("Unknown summary kind.".into()),
        _ => Err("That kind of summary needs a number field.".into()),
    }
}

/// The options a summary of `def.kind` may have, and those OpenMaker
/// writes: running and restarting totals and counts, the weighted average,
/// the standard deviation by population, the subtotaled fraction and
/// repetitions summarized individually (schema-path.md 5.11).
pub fn check_options(def: &SummaryDef) -> Result<(), String> {
    let running = matches!(def.kind, SummaryKind::Total | SummaryKind::Count);
    if def.has(summary_flag::RUNNING) && !running {
        return Err("Only a total or a count can be running.".into());
    }
    if def.has(summary_flag::RESTART) {
        if !def.has(summary_flag::RUNNING) {
            return Err("Only a running total or count restarts.".into());
        }
        if def.additional.is_none() {
            return Err("Choose the field whose sorted groups restart the summary.".into());
        }
    } else if def.additional.is_some() && !def.has(summary_flag::OPTION) {
        return Err("Only a weighted, subtotaled or restarting summary has a second field.".into());
    }
    if def.has(summary_flag::OPTION) {
        match def.kind {
            SummaryKind::StdDeviation if def.additional.is_some() => {
                return Err("A standard deviation has no second field.".into())
            }
            SummaryKind::StdDeviation => {}
            SummaryKind::Average if def.additional.is_none() => {
                return Err("Choose the field the average is weighted by.".into())
            }
            SummaryKind::Fraction if def.additional.is_none() => {
                return Err("Choose the field whose sorted groups subtotal the fraction.".into())
            }
            SummaryKind::Average | SummaryKind::Fraction => {}
            _ => return Err("That kind of summary has no such option.".into()),
        }
    }
    let known = summary_flag::OPTION
        | summary_flag::RUNNING
        | summary_flag::RESTART
        | summary_flag::INDIVIDUAL_REPETITIONS;
    if def.flags & !known != 0 {
        return Err("Unknown summary options.".into());
    }
    if def.field.is_none() {
        return Err("Choose the field to summarize.".into());
    }
    Ok(())
}

/// The result type of summary `def` over the fields of its table, checking
/// what it summarizes and its options.
pub fn result_type(def: &SummaryDef, fields: &[SchemaField]) -> Result<FieldKind, String> {
    check_options(def)?;
    let id = def.field.map(|f| f.field).unwrap_or_default();
    let field = fields
        .iter()
        .find(|f| f.id == id)
        .ok_or_else(|| "The summarized field no longer exists.".to_owned())?;
    if field.kind == SchemaFieldKind::Summary {
        return Err("A summary field cannot summarize another summary field.".into());
    }
    kind_result(def.kind, field.data_type)
}
