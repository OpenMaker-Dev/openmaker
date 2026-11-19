//! The calculation operators (spec `docs/re/calc-values.md` 6): arithmetic
//! with the result-type table of 6.1 and the units of 4.3, comparison with
//! the comparison-type table of 6.2, the empty-operand rules of 6.4, and
//! `&`.
//!
//! Errors are returned, not recorded: the evaluator keeps the first one
//! ([`crate::FirstError`]) and uses `?` for the value.

use crate::datetime::{Date, Time, TimeStyle, Timestamp};
use crate::error::CalcError;
use crate::number::Number;
use crate::text::{text_cmp, text_eq};
use crate::value::{Ctx, Value, ValueType};
use std::cmp::Ordering;

/// The result type of `+` and `-` (`nativeTypeMatrix`, spec 6.1).
pub fn sum_type(left: ValueType, right: ValueType) -> ValueType {
    use ValueType::*;
    match (left, right) {
        (Container, _) | (_, Container) => Number,
        (Text | Number, Text | Number) => Number,
        (Text | Number, other) => other,
        (Date, Text | Number) => Date,
        (Date, Date) => Number,
        (Date, Time) => Timestamp,
        (Date, Timestamp) => Time,
        (Time, Text | Number | Time) => Time,
        (Time, Date | Timestamp) => Timestamp,
        (Timestamp, Text | Number | Time) => Timestamp,
        (Timestamp, Date | Timestamp) => Time,
    }
}

/// An operand in the unit of a result: days for a date result, seconds for
/// time and timestamp results; a date beside a time or timestamp counts as
/// its timestamp seconds.
fn in_unit(v: &Value, result: ValueType, cx: &Ctx) -> Number {
    match (v, result) {
        (Value::Date(d) | Value::DateText(d, _), ValueType::Time | ValueType::Timestamp) => {
            d.to_timestamp().to_number()
        }
        _ => v.to_number(cx),
    }
}

/// A sum or difference in the result's unit as a value of the result type.
/// A date result of zero or less and a negative timestamp result become
/// plain numbers (`Date(1;1;1) - 1` = 0, `Time(1;0;0) - Timestamp(...)` =
/// -63113900400); a date result in (0, 1) and results above the range are
/// error 500 (`Date(1;1;1) - 0.5`; oracle answers). Dates truncate the day
/// count (`Date(1;1;2001) - 0.5` = 12/31/2000).
fn typed(n: Number, ty: ValueType) -> Result<Value, CalcError> {
    if n.is_nan() {
        return Ok(Value::Number(n));
    }
    match ty {
        ValueType::Date => {
            if n.is_negative() || n.is_zero() {
                return Ok(Value::Number(n));
            }
            Date::from_number(&n)
                .map(Value::Date)
                .ok_or(CalcError::INVALID_DATE)
        }
        ValueType::Time => Time::from_seconds(&n)
            .map(Value::Time)
            .ok_or(CalcError::INVALID_TIME),
        ValueType::Timestamp => {
            if n.is_negative() {
                return Ok(Value::Number(n));
            }
            Timestamp::from_seconds(&n)
                .map(Value::Timestamp)
                .ok_or(CalcError::INVALID_DATE)
        }
        _ => Ok(Value::Number(n)),
    }
}

fn additive(a: &Value, b: &Value, subtract: bool, cx: &Ctx) -> Result<Value, CalcError> {
    // Both empty: the left operand, empty (spec 6.4).
    if a.is_empty() && b.is_empty() {
        return Ok(a.clone());
    }
    let ty = sum_type(a.value_type(), b.value_type());
    let x = in_unit(a, ty, cx);
    let y = in_unit(b, ty, cx);
    if subtract && !b.is_empty() && y.is_empty() {
        // `Date(1;1;2001) - "abc"` is error 500 and `Time(1;0;0) - "abc"`
        // 501, while `+ "abc"` and `- ""` count it as 0 (oracle answers).
        match ty {
            ValueType::Date | ValueType::Timestamp => return Err(CalcError::INVALID_DATE),
            ValueType::Time => return Err(CalcError::INVALID_TIME),
            _ => {}
        }
    }
    let n = if subtract {
        x.sub(&y, cx.precision)
    } else {
        x.add(&y, cx.precision)
    };
    typed(n, ty)
}

/// `+` (spec 6.1, 4.3): `Date + 1` is the next day, `Date + Time` a
/// timestamp, `Time + 1` a time one second later.
pub fn add(a: &Value, b: &Value, cx: &Ctx) -> Result<Value, CalcError> {
    additive(a, b, false, cx)
}

/// `-`: `Date - Date` is a number of days, `Timestamp - Timestamp` a time.
pub fn subtract(a: &Value, b: &Value, cx: &Ctx) -> Result<Value, CalcError> {
    additive(a, b, true, cx)
}

/// The numbers of a numeric operator's operands, or the empty left operand
/// when both are empty (spec 6.4).
fn numbers(a: &Value, b: &Value, cx: &Ctx) -> Result<(Number, Number), Value> {
    if a.is_empty() && b.is_empty() {
        return Err(a.clone());
    }
    Ok((a.to_number(cx), b.to_number(cx)))
}

/// `*`: a number, with FileMaker's row product (spec 14.2: the last held
/// digit depends on the operand order).
pub fn multiply(a: &Value, b: &Value, cx: &Ctx) -> Value {
    match numbers(a, b, cx) {
        Ok((x, y)) => Value::Number(x.mul(&y, cx.precision)),
        Err(empty) => empty,
    }
}

/// `/`: a number; error 15 when the divisor is zero or empty.
pub fn divide(a: &Value, b: &Value, cx: &Ctx) -> Result<Value, CalcError> {
    match numbers(a, b, cx) {
        Ok((x, y)) => Ok(Value::Number(x.div(&y, cx.precision)?)),
        Err(empty) => Ok(empty),
    }
}

/// `^` (spec 2.4, 14.5).
pub fn power(a: &Value, b: &Value, cx: &Ctx) -> Result<Value, CalcError> {
    match numbers(a, b, cx) {
        Ok((x, y)) => Ok(Value::Number(x.pow(&y, cx.precision)?)),
        Err(empty) => Ok(empty),
    }
}

/// Unary `-`: a number (`-Date(1;1;2001)` = -730486, `-"abc"` = 0); an
/// empty operand stays empty.
pub fn negate(a: &Value, cx: &Ctx) -> Value {
    if a.is_empty() {
        return a.clone();
    }
    Value::Number(a.numeric_operand(cx).neg())
}

/// `&`: text, styles kept (spec 6.1, 5.2).
pub fn concat(a: &Value, b: &Value, cx: &Ctx) -> Value {
    let mut text = a.to_text(cx);
    text.push(&b.to_text(cx));
    Value::Text(text)
}

/// [`concat`] within the evaluation's text limit: error 7 before the
/// joined text is built when it would be past it. The operands are taken,
/// so that text is extended in place rather than copied first.
pub fn concat_within(a: Value, b: Value, cx: &Ctx) -> Result<Value, CalcError> {
    let owned = |v: Value| match v {
        Value::Text(t) | Value::NumberText(_, t) | Value::DateText(_, t) => t.with_json_state(0),
        other => other.to_text(cx),
    };
    let mut text = owned(a);
    let tail = owned(b);
    cx.check_size(text.size() + tail.size())?;
    text.push(&tail);
    Ok(Value::Text(text))
}

/// How two values compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Ordered(Ordering),
    /// `?` against a number: `<`, `≤` and `=` are false and their negations
    /// `≥`, `>` and `≠` true (`(1/0) > 1` and `1 > (1/0)` are both 1).
    Unordered,
    /// A time against a timestamp: every comparison, `≠` included, is false
    /// (spec 6.2).
    Incomparable,
    /// Two different containers: `=`, `<` and `>` are false, `≠`, `≤` and
    /// `≥` true (`HexDecode ( "01" ; "a" ) ≤ HexDecode ( "00" ; "a" )` = 1).
    Unequal,
}

/// The comparison type of two values (`compareTypeMatrix`, spec 6.2).
pub fn compare_type(left: ValueType, right: ValueType) -> Option<ValueType> {
    use ValueType::*;
    match (left, right) {
        (Text, _) | (_, Text) => Some(Text),
        (Time, Timestamp) | (Timestamp, Time) => None,
        (Container, Container) => Some(Container),
        _ => Some(Number),
    }
}

/// Compares two values as `=`, `<` and friends do (spec 6.2): as text when
/// either is text (a number by its display text, a date by its formatted
/// text, by the file's collation), else as numbers rounded to the larger
/// precision, with a date beside a time or timestamp taken as timestamp
/// seconds; an empty number compares as 0.
pub fn compare(a: &Value, b: &Value, cx: &Ctx) -> Comparison {
    match compare_type(a.value_type(), b.value_type()) {
        None => Comparison::Incomparable,
        Some(ValueType::Text) => {
            let (x, y) = (a.to_text(cx), b.to_text(cx));
            let language = cx.locale.language;
            if text_eq(x.as_str(), y.as_str(), language) {
                Comparison::Ordered(Ordering::Equal)
            } else {
                Comparison::Ordered(text_cmp(x.as_str(), y.as_str(), language))
            }
        }
        // Containers compare by data and name: equal or not, never ordered.
        Some(ValueType::Container) => match (a, b) {
            (Value::Container(x), Value::Container(y)) if x != y => Comparison::Unequal,
            _ => Comparison::Ordered(Ordering::Equal),
        },
        Some(_) => {
            let mixed_date = |v: &Value, other: &Value| {
                matches!(v, Value::Date(_) | Value::DateText(..))
                    && matches!(other.value_type(), ValueType::Time | ValueType::Timestamp)
            };
            let num = |v: &Value, other: &Value| match v {
                Value::Date(d) | Value::DateText(d, _) if mixed_date(v, other) => {
                    d.to_timestamp().to_number()
                }
                _ => v.to_number(cx),
            };
            match num(a, b).compare(&num(b, a)) {
                Some(order) => Comparison::Ordered(order),
                None => Comparison::Unordered,
            }
        }
    }
}

/// The comparison operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CompareOp {
    /// Whether the operator holds for a comparison.
    pub fn holds(self, c: Comparison) -> bool {
        match c {
            Comparison::Incomparable => false,
            Comparison::Unequal => matches!(self, CompareOp::Ne | CompareOp::Le | CompareOp::Ge),
            Comparison::Unordered => {
                matches!(self, CompareOp::Ne | CompareOp::Gt | CompareOp::Ge)
            }
            Comparison::Ordered(o) => match self {
                CompareOp::Eq => o == Ordering::Equal,
                CompareOp::Ne => o != Ordering::Equal,
                CompareOp::Lt => o == Ordering::Less,
                CompareOp::Le => o != Ordering::Greater,
                CompareOp::Gt => o == Ordering::Greater,
                CompareOp::Ge => o != Ordering::Less,
            },
        }
    }
}

/// A comparison operator's 0/1 result.
pub fn compare_op(a: &Value, op: CompareOp, b: &Value, cx: &Ctx) -> Value {
    Value::boolean(op.holds(compare(a, b, cx)))
}

/// `Abs`, which keeps a time a time (spec 6.1: `Abs(Time(-1;0;0))` =
/// 1:00:00) and gives a number otherwise; empty input stays empty.
pub fn abs(a: &Value, cx: &Ctx) -> Value {
    match a {
        Value::Time(t) => Value::Time(Time::from_micros(t.micros().abs(), TimeStyle::Elapsed)),
        v if v.is_empty() => v.clone(),
        v => Value::Number(v.numeric_operand(cx).abs()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Locale;
    use crate::text::Text;

    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap())
    }

    fn date(y: i64, m: u32, d: u32) -> Value {
        Value::Date(Date::from_ymd(y, m, d).unwrap())
    }

    fn time(secs: i64) -> Value {
        Value::Time(Time::from_seconds(&Number::from_i64(secs)).unwrap())
    }

    fn show(v: &Value, cx: &Ctx) -> String {
        v.to_text(cx).into_string()
    }

    #[test]
    fn result_types_and_units() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let d = date(2001, 1, 1);
        assert_eq!(
            show(&add(&d, &Value::number(1), &cx).unwrap(), &cx),
            "1/2/2001"
        );
        assert_eq!(
            show(&subtract(&d, &date(2000, 1, 1), &cx).unwrap(), &cx),
            "366"
        );
        assert_eq!(
            show(&subtract(&d, &time(3600), &cx).unwrap(), &cx),
            "12/31/2000 11:00 PM"
        );
        assert_eq!(
            show(&subtract(&time(3600), &time(7200), &cx).unwrap(), &cx),
            "-1:00:00"
        );
        assert_eq!(
            show(&add(&time(86399), &Value::number(1), &cx).unwrap(), &cx),
            "24:00:00"
        );
        assert_eq!(
            show(&multiply(&time(3600), &Value::number(2), &cx), &cx),
            "7200"
        );
        assert_eq!(show(&negate(&d, &cx), &cx), "-730486");
        let first = date(1, 1, 1);
        let below = subtract(&first, &Value::number(1), &cx).unwrap();
        assert!(matches!(below, Value::Number(_)));
        assert_eq!(show(&below, &cx), "0");
        let half = Value::Number(Number::from_literal("0.5", '.').unwrap());
        assert_eq!(
            subtract(&first, &half, &cx).unwrap_err(),
            CalcError::INVALID_DATE
        );
        assert_eq!(
            add(&date(4000, 12, 31), &Value::number(1), &cx).unwrap_err(),
            CalcError::INVALID_DATE
        );
        assert_eq!(
            show(&add(&Value::text("1:30"), &time(3600), &cx).unwrap(), &cx),
            "1:02:10"
        );
        assert_eq!(
            subtract(&d, &Value::text("abc"), &cx).unwrap_err(),
            CalcError::INVALID_DATE
        );
        assert_eq!(
            show(&add(&d, &Value::text("abc"), &cx).unwrap(), &cx),
            "1/1/2001"
        );
    }

    #[test]
    fn empty_operands() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let empty = Value::empty_text();
        assert!(add(&empty, &empty, &cx).unwrap().is_empty());
        assert_eq!(
            show(&add(&empty, &Value::number(1), &cx).unwrap(), &cx),
            "1"
        );
        assert_eq!(show(&multiply(&empty, &Value::number(2), &cx), &cx), "0");
        assert_eq!(
            divide(&Value::number(1), &empty, &cx).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
        assert_eq!(
            show(&add(&empty, &date(2001, 1, 1), &cx).unwrap(), &cx),
            "1/1/2001"
        );
        let empty_ts = Value::empty(ValueType::Timestamp);
        assert_eq!(
            show(&add(&empty_ts, &Value::number(1), &cx).unwrap(), &cx),
            "1/1/0001 12:00:01 AM"
        );
        assert!(negate(&empty, &cx).is_empty());
        assert_eq!(show(&negate(&Value::text("abc"), &cx), &cx), "0");
    }

    #[test]
    fn comparisons() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let test = |a: &Value, op, b: &Value| op_holds(a, op, b, &cx);
        assert!(test(&Value::number(10), CompareOp::Lt, &Value::text("9")));
        assert!(!test(&Value::number(1), CompareOp::Eq, &Value::text("1.0")));
        assert!(!test(&Value::text(""), CompareOp::Eq, &Value::number(0)));
        assert!(test(&Value::text(""), CompareOp::Lt, &Value::number(0)));
        assert!(test(
            &date(2003, 1, 2),
            CompareOp::Eq,
            &Value::text("1/2/2003")
        ));
        assert!(!test(
            &date(2003, 1, 2),
            CompareOp::Eq,
            &Value::text("01/02/2003")
        ));
        let ts = Value::Timestamp(Date::from_ymd(1, 1, 1).unwrap().to_timestamp());
        assert!(!test(&time(0), CompareOp::Eq, &ts));
        assert!(!test(&time(0), CompareOp::Ne, &ts));
        assert!(test(&date(1, 1, 1), CompareOp::Eq, &time(0)));
        assert!(!test(&date(1, 1, 2), CompareOp::Eq, &Value::number(86400)));
        assert!(test(&Value::invalid(), CompareOp::Eq, &Value::invalid()));
        assert!(!test(&Value::invalid(), CompareOp::Lt, &Value::number(1)));
        assert!(test(&Value::invalid(), CompareOp::Gt, &Value::number(1)));
        assert!(test(&Value::number(1), CompareOp::Gt, &Value::invalid()));
        assert!(!test(&Value::invalid(), CompareOp::Le, &Value::number(1)));
        assert!(test(&Value::invalid(), CompareOp::Ne, &Value::number(1)));
        assert!(test(&Value::text("?"), CompareOp::Eq, &Value::invalid()));
        let bold = Value::Text(Text::styled(
            "ab",
            crate::text::CharStyle {
                flags: 1,
                ..Default::default()
            },
        ));
        assert!(test(&bold, CompareOp::Eq, &Value::text("ab")));
        let empty_number = Value::Number(Number::empty());
        assert!(test(&empty_number, CompareOp::Eq, &Value::number(0)));
        assert!(test(&empty_number, CompareOp::Eq, &Value::text("")));
    }

    fn op_holds(a: &Value, op: CompareOp, b: &Value, cx: &Ctx) -> bool {
        op.holds(compare(a, b, cx))
    }
}
