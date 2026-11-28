//! The measured local global Text auto-entry forms that read Self.

use om_calc::eval::{evaluate_formula, Context, MemoryHost};
use om_calc::{Date, Locale, Value, ValueType};
use om_formula::{Expr, Opcode};
use om_hbam::FieldOptions;

use crate::host::{FieldClass, FieldDef};
use crate::DataError;

use super::{error, walk, BOOKKEEPING_OPTIONS, INDEX_OPTIONS, VALIDATE_ALWAYS};

#[derive(Clone, Copy, Default)]
pub(super) enum GlobalTextSelf {
    #[default]
    Unused,
    Supported,
    Unsupported,
}

fn self_value(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { op: Opcode::SELF, args } if args.is_empty())
}

fn text_self_branches(expr: &Expr) -> Option<(&str, &Expr)> {
    let Expr::Call {
        op: Opcode::IF,
        args,
    } = expr
    else {
        return None;
    };
    let [Expr::Call { op, args: tested }, Expr::Text(default), fallback] = args.as_slice() else {
        return None;
    };
    if op.name() != Some("IsEmpty")
        || !matches!(tested.as_slice(), [value] if self_value(value))
        || default.style.is_some()
    {
        return None;
    }
    Some((&default.value, fallback))
}

fn default_with_self(expr: &Expr) -> bool {
    let Some((_, fallback)) = text_self_branches(expr) else {
        return false;
    };
    self_value(fallback)
        || matches!(
            fallback,
            Expr::Call { op: Opcode::CONCAT, args }
                if matches!(args.as_slice(), [value, Expr::Text(suffix)]
                    if self_value(value) && suffix.style.is_none())
        )
}

/// The literal restored on leaving a cleared ordinary Text field in a local
/// desktop window. The measured formula returns nonempty input unchanged.
pub fn typed_text_self_default(field: &FieldDef) -> Option<&str> {
    if field.class != FieldClass::Data
        || field.global
        || field.value_type != ValueType::Text
        || field.repetitions != 1
        || !matches!(field.options, 0x0000_0088 | 0xa000_0088)
        || field.validation_options != 0
    {
        return None;
    }
    let (default, fallback) = text_self_branches(&field.auto_enter.as_ref()?.polish)?;
    self_value(fallback).then_some(default)
}

pub(super) fn classify(
    field: &FieldDef,
    options: Option<&FieldOptions>,
    expr: &Expr,
    local: bool,
) -> GlobalTextSelf {
    let mut uses_self = false;
    walk(expr, &mut |node| uses_self |= self_value(node));
    if !uses_self {
        return GlobalTextSelf::Unused;
    }
    let measured = options.is_some_and(|options| {
        matches!(
            options.options & !(INDEX_OPTIONS | BOOKKEEPING_OPTIONS),
            0x0001_0008 | 0x0001_0088
        ) && options.validation_options & !VALIDATE_ALWAYS == 0
            && options.max_repetitions == 1
    });
    if local
        && measured
        && field.class == FieldClass::Data
        && field.value_type == ValueType::Text
        && default_with_self(expr)
    {
        GlobalTextSelf::Supported
    } else {
        GlobalTextSelf::Unsupported
    }
}

pub(super) fn unavailable(field: &str) -> DataError {
    DataError::with_code(
        error::UNAVAILABLE,
        format!("Field {field}: unsupported global Text auto-entry using Self."),
    )
}

/// The session global that a local scripted Set Field proposes. `None`
/// leaves the supplied value alone, including when replacement is off
/// and the supplied text is nonempty.
pub fn global_text_assignment(
    field: &FieldDef,
    options: Option<&FieldOptions>,
    locale: &Locale,
    local: bool,
    proposed: &str,
) -> Result<Option<String>, DataError> {
    let Some(formula) = &field.auto_enter else {
        return Ok(None);
    };
    if !proposed.is_empty() && !options.is_some_and(FieldOptions::calculation_replaces_existing) {
        return Ok(None);
    }
    match classify(field, options, &formula.polish, local) {
        GlobalTextSelf::Unused => return Ok(None),
        GlobalTextSelf::Unsupported => return Err(unavailable(&field.name)),
        GlobalTextSelf::Supported => {}
    }
    // The admitted expression reads only Self and literals. It does not
    // need a record, clock, or the session's other variables.
    let mut host = MemoryHost::new(locale.clone(), Date::from_ymd(1, 1, 1).unwrap());
    let context = Context {
        self_value: Some(Value::text(proposed)),
        result_type: Some(ValueType::Text),
        ..Context::default()
    };
    let result = evaluate_formula(formula, &mut host, &context);
    match result.value {
        Value::Text(text) if result.error.is_none() => Ok(Some(text.as_str().to_owned())),
        _ => Err(DataError::with_code(
            error::UNAVAILABLE,
            format!(
                "Field {}: could not evaluate global auto-entry.",
                field.name
            ),
        )),
    }
}
