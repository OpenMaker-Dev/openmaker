//! The measured local Date clamp that reads its proposed value.

use om_calc::locale::DateOrder;
use om_calc::{Ctx, Date, Language, Locale, Value, ValueType};
use om_formula::{Expr, Opcode};

use crate::host::{session::selector::CURRENT_DATE, FieldClass, FieldDef};
use crate::DataError;

pub(super) fn supported(field: &FieldDef, locale: &Locale) -> bool {
    if field.class != FieldClass::Data
        || field.global
        || field.value_type != ValueType::Date
        || field.repetitions != 1
        || field.options != 0x88
        || field.validation_options != 0
        || locale.language != Language::English
        || locale.date_order != DateOrder::MonthDayYear
        || locale.date_separator != '/'
        || locale.pad_day
        || locale.pad_month
    {
        return false;
    }
    let Some(Expr::Call {
        op: Opcode::IF,
        args,
    }) = field.auto_enter.as_ref().map(|formula| &formula.polish)
    else {
        return false;
    };
    let [Expr::Call {
        op: Opcode::GREATER,
        args: test,
    }, Expr::Get(CURRENT_DATE), fallback] = args.as_slice()
    else {
        return false;
    };
    let self_value =
        |expr: &Expr| matches!(expr, Expr::Call { op: Opcode::SELF, args } if args.is_empty());
    self_value(fallback)
        && matches!(test.as_slice(), [value, Expr::Get(CURRENT_DATE)] if self_value(value))
}

pub(super) fn evaluate(proposed: &str, cx: &Ctx) -> Option<(Value, String)> {
    if proposed.is_empty() {
        return Some((Value::empty(ValueType::Date), String::new()));
    }
    let date = Date::parse(proposed, cx.locale, cx.today).ok().flatten()?;
    let (date, text) = if date > cx.today {
        (cx.today, cx.today.format(cx.locale))
    } else {
        (date, proposed.to_owned())
    };
    Some((Value::DateText(date, om_calc::Text::plain(&text)), text))
}

/// A changed Date value to show after leaving the typed editor. Keep
/// unparseable input in the editor for ordinary Date validation.
pub fn typed_date_self_clamp(
    field: &FieldDef,
    proposed: &str,
    locale: &Locale,
    today: Date,
) -> Option<String> {
    if !supported(field, locale) || proposed.is_empty() {
        return None;
    }
    let cx = Ctx::new(locale, today);
    let entered = super::typing::entered(om_hbam::DataType::Date, proposed, &cx).ok()?;
    let (_, clamped) = evaluate(&entered, &cx)?;
    (clamped != entered).then_some(clamped)
}

/// The value to hold at a local Set Field, before the record commits.
pub fn scripted_date_self_clamp(
    field: &FieldDef,
    proposed: &str,
    locale: &Locale,
    today: Date,
) -> Result<Option<String>, DataError> {
    if !supported(field, locale) {
        return Ok(None);
    }
    let cx = Ctx::new(locale, today);
    let (_, clamped) = evaluate(proposed, &cx).ok_or_else(|| {
        DataError::with_code(
            super::error::UNAVAILABLE,
            format!("Field {}: unsupported Date input to Self.", field.name),
        )
    })?;
    Ok((clamped != proposed).then_some(clamped))
}
