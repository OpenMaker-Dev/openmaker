//! Aggregate functions (spec `calc-functions.md` 5.7 and the aggregate
//! oracle answers at the end of its section 9): `Average`, `Count`, `List`,
//! `Max`, `Min`, `StDev`, `StDevP`, `Sum`, `Variance`, `VarianceP`. Each takes
//! [`super::Aggregate`] arguments: every argument already expanded into the
//! values it stands for. The functions read the arguments' values in order
//! as one list.
//!
//! # What the evaluator passes
//!
//! From `calc-values.md` 8.6, 8.7 and 12.1 (record-context engine runs):
//!
//! - **One argument** that is a field: every repetition of a repeating field
//!   and every record of a related field (in related-set order, each
//!   record's repetitions in order), also when the reference has an index
//!   (`List(r[3])` lists all repetitions). Empty repetitions may be passed
//!   or left out: the functions skip empty values either way.
//! - **Two or more arguments**: each field gives one value, its current
//!   repetition (repetition 1 outside a repeating calculation) or the first
//!   related record's value (`Sum(Rel::x; 100)` = first + 100).
//! - A related field with no related records is an empty list: `Count` is 0
//!   and the others are empty (`Sum(Rel::n) + 1` = 1).
//! - **A single argument that is not a field** is rejected by the parser
//!   with error 1214 (`Sum(5)`, `List("")`, `Average(Abs(1))`), so the
//!   functions never see it; plain expressions only appear beside other
//!   arguments, one value each.
//!
//! # Which values count
//!
//! Empty values (empty text, empty numbers, dates, times) are skipped
//! everywhere, and so is the `?` *number* (`1/0`, `GetAsNumber("?")`).
//! `Count` counts every other value, text without digits and the text `?`
//! included (`Count("";0;"x")` = 2). `List` lists every non-empty value as
//! its text, `?` included (`List(1/0;2)` = `?¶2`). `Sum`, `Average` and the
//! variance functions take the values whose number is neither empty nor `?`,
//! so digitless text is skipped (`Sum(1;"";"a";2)` = 3, `Average("a";2)` = 2);
//! dates count as day numbers and times and timestamps as seconds, and the
//! result is a number. `Max` and `Min` return the winning value itself with
//! its type (text stays text: `Max("10";9)` is the text `10`).
//!
//! Open: over a related *text* field FileMaker gives `Average` = 0 for one
//! field and error 15 for another (Task and Category of the FileMaker
//! Server sample), where a list of digitless literals gives empty.

use super::{Function, Keyword};
use crate::ops::{self, CompareOp, Comparison};
use crate::text::extreme_cmp;
use crate::{CalcError, Ctx, Number, Text, Value, ValueType};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::aggregate("Average", average),
    Function::aggregate("Count", count),
    Function::aggregate("List", list),
    Function::aggregate("Max", max),
    Function::aggregate("Min", min),
    Function::aggregate("StDev", stdev),
    Function::aggregate("StDevP", stdevp),
    Function::aggregate("Sum", sum),
    Function::aggregate("Variance", variance),
    Function::aggregate("VarianceP", variancep),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

fn values(lists: &[Vec<Value>]) -> impl Iterator<Item = &Value> {
    lists.iter().flatten()
}

/// Whether a value takes part at all: not empty and not the `?` number.
fn present(v: &Value) -> bool {
    !v.is_empty() && !v.is_invalid()
}

/// The numbers of the values whose number is neither empty nor `?`.
fn numbers(lists: &[Vec<Value>], cx: &Ctx) -> Vec<Number> {
    values(lists)
        .map(|v| v.to_number(cx))
        .filter(|n| !n.is_empty() && !n.is_nan())
        .collect()
}

/// The exact sum at the largest precision involved; `None` for no values.
fn total(numbers: &[Number], cx: &Ctx) -> Option<Number> {
    let (first, rest) = numbers.split_first()?;
    Some(
        rest.iter()
            .fold(first.clone(), |s, n| s.add(n, cx.precision)),
    )
}

/// `Sum`: empty (a number) when no value counts.
fn sum(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(match total(&numbers(lists, cx), cx) {
        Some(s) => Value::Number(s),
        None => Value::empty(ValueType::Number),
    })
}

/// `Average`: the exact sum divided at the evaluation precision (the
/// values' precision does not carry over: `Average(0.1234567890123456789;
/// 0.1234567890123456789)` = `.1234567890123457`).
fn average(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    let numbers = numbers(lists, cx);
    let Some(s) = total(&numbers, cx) else {
        return Ok(Value::empty(ValueType::Number));
    };
    let count = Number::from_i64(numbers.len() as i64);
    s.div_at(&count, cx.precision).map(Value::Number)
}

fn count(lists: &[Vec<Value>], _cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::number(
        values(lists).filter(|v| present(v)).count() as i64
    ))
}

/// `List`: the non-empty values' texts, styles kept, separated by CR, no
/// trailing CR; a value's own CR stays (`List("a¶";"b")` = `a¶¶b`).
/// The values joined by returns; each return takes the style of the
/// character before it (`GetAsCSS ( List ( TextStyleAdd ( "a" ; Bold ) ;
/// "b" ) )` puts the `<br/>` inside the bold span, calc-value vectors).
/// Error 7 before a value would take the list past the text limit.
fn list(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    let mut text = Text::new();
    for (i, v) in values(lists).filter(|v| !v.is_empty()).enumerate() {
        if i > 0 {
            let last = text.as_str().char_indices().last().map_or(0, |(at, _)| at);
            let style = text.style_at(last);
            text.push_styled("\r", &style);
        }
        let value = v.to_text(cx);
        cx.check_size(text.size() + value.size())?;
        text.push(&value);
    }
    Ok(Value::Text(text))
}

/// How `Max` and `Min` order a candidate against the value kept so far
/// (oracle answers):
///
/// - text against text by the file's collation, characters whose key is
///   an expansion (`æ`, `ß`, German `ä`) after all others
///   ([`extreme_cmp`]; `Max("10";"9")` = `9`, `Max("ä";"z")` = `ä` in a
///   German file);
/// - a number against anything, and text against a number, by number:
///   dates as day numbers, times and timestamps as seconds, digitless text
///   as 0 (`Max("abc";-1)` = `abc`, `Max(Date(1;1;2001);10^7)` = 10000000);
/// - otherwise the candidate converts to the kept value's type: to its text
///   when the kept value is text (`Max("5";Date(1;1;2001))` = `5`), and to a
///   date, time or timestamp by that type's conversion (a timestamp's date,
///   a date's elapsed seconds as a time, a time on 1/1/0001 as a
///   timestamp, text parsed). A candidate that does not convert is
///   incomparable to a kept date or time (`Max(Date(1;1;1);Time(1;0;0))`
///   keeps the date, `Min` takes the time), except date-like text against
///   a date ([`date_like`]), and unordered, like `?`, against a kept
///   timestamp.
fn order(candidate: &Value, kept: &Value, cx: &Ctx) -> Comparison {
    use ValueType as T;
    let by_number =
        |a: Number| ops::compare(&Value::Number(a), &Value::Number(kept.to_number(cx)), cx);
    let by_text = |a: &Text, b: &Text| {
        Comparison::Ordered(extreme_cmp(a.as_str(), b.as_str(), cx.locale.language))
    };
    match (candidate.value_type(), kept.value_type()) {
        (T::Text, T::Text) => by_text(&candidate.to_text(cx), &kept.to_text(cx)),
        (T::Number, _) | (_, T::Number) => by_number(candidate.to_number(cx)),
        (_, T::Text) => by_text(&candidate.to_text(cx), &kept.to_text(cx)),
        (_, T::Container) | (T::Container, _) => ops::compare(
            &Value::Text(candidate.to_text(cx)),
            &Value::Text(kept.to_text(cx)),
            cx,
        ),
        (_, T::Date) => match candidate.to_date(cx) {
            Ok(Some(d)) => by_number(d.to_number()),
            _ if date_like(candidate) => Comparison::Unordered,
            _ => Comparison::Incomparable,
        },
        (_, T::Time) => match candidate.to_time(cx) {
            Ok(Some(t)) => by_number(t.to_number()),
            _ => Comparison::Incomparable,
        },
        (_, T::Timestamp) => match candidate.to_timestamp(cx) {
            Ok(Some(t)) => by_number(t.to_number()),
            _ => Comparison::Unordered,
        },
    }
}

/// Whether a value is text with two or more runs of digits: date text that
/// fails to convert (`13/45/2001`, `2003-01-03` in a day-first file)
/// compares unordered with a date, other text (`abc`, `9999999`) is
/// incomparable [inference from the oracle answers].
fn date_like(v: &Value) -> bool {
    let Value::Text(t) = v else {
        return false;
    };
    let mut runs = 0;
    let mut in_digits = false;
    for c in t.as_str().chars() {
        let digit = c.is_ascii_digit();
        if digit && !in_digits {
            runs += 1;
        }
        in_digits = digit;
    }
    runs >= 2
}

/// The value kept by `Max` or `Min`: the first value, replaced by each
/// later one for which `replace` holds; empty text when no value counts.
fn extreme(lists: &[Vec<Value>], cx: &Ctx, replace: fn(Comparison) -> bool) -> Value {
    let mut kept: Option<&Value> = None;
    for v in values(lists).filter(|v| present(v)) {
        kept = match kept {
            Some(k) if !replace(order(v, k, cx)) => Some(k),
            _ => Some(v),
        };
    }
    kept.cloned().unwrap_or_else(Value::empty_text)
}

/// `Max`: a later value wins when it is `>` the kept one, so ties keep the
/// first and `?` text beats a number (`Max(1;"?")` = `?`, `Max("?";1)` = 1).
fn max(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(extreme(lists, cx, |c| CompareOp::Gt.holds(c)))
}

/// `Min`: a later value wins unless it is `≥` the kept one
/// (`Min(2;"?")` = 2, `Min("?";1)` = `?`).
fn min(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(extreme(lists, cx, |c| !CompareOp::Ge.holds(c)))
}

/// The variance at the evaluation precision, empty text when no value
/// counts.
fn spread(
    lists: &[Vec<Value>],
    cx: &Ctx,
    population: bool,
    root: bool,
) -> Result<Value, CalcError> {
    let numbers = numbers(lists, cx);
    if root && population && numbers.len() == 1 {
        // `StDevP` of one value is 0 although `VarianceP` is error 15.
        return Ok(Value::number(0));
    }
    match Number::variance(&numbers, population, cx.precision) {
        None => Ok(Value::empty_text()),
        Some(v) if root => v?.sqrt(cx.precision).map(Value::Number),
        Some(v) => v.map(Value::Number),
    }
}

/// `Variance`: over n - 1, error 15 for one value.
fn variance(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    spread(lists, cx, false, false)
}

/// `VarianceP`: over n; error 15 for one value too.
fn variancep(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    spread(lists, cx, true, false)
}

/// `StDev`: the square root of the rounded `Variance`
/// (`StDev(1;2;3;4)*10^18` = 1290994448735805629, √(5/3) being ...628).
fn stdev(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    spread(lists, cx, false, true)
}

/// `StDevP`: the square root of the rounded `VarianceP`; 0 for one value.
fn stdevp(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    spread(lists, cx, true, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Date, Locale, Time};

    fn cx(locale: &Locale) -> Ctx<'_> {
        Ctx::new(locale, Date::from_ymd(2026, 10, 28).unwrap())
    }

    fn run(f: super::super::Aggregate, list: Vec<Value>, cx: &Ctx) -> (String, i32) {
        match f(&[list], cx) {
            Ok(v) => (v.to_text(cx).into_string(), 0),
            Err(e) => ("?".into(), e.code()),
        }
    }

    fn date(y: i64, m: u32, d: u32) -> Value {
        Value::Date(Date::from_ymd(y, m, d).unwrap())
    }

    /// A related field's 12 values in the FileMaker Server sample
    /// (`calc-values.md` 12.1), as one expanded argument; results from the
    /// engine in record context.
    #[test]
    fn related_records() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let ids: Vec<Value> = [7, 1, 3, 12, 5, 6, 8, 11, 9, 2, 10, 4]
            .iter()
            .map(|&v| Value::number(v))
            .collect();
        let r = |f| run(f, ids.clone(), &cx).0;
        assert_eq!(r(sum), "78");
        assert_eq!(r(average), "6.5");
        assert_eq!(r(count), "12");
        assert_eq!(r(min), "1");
        assert_eq!(r(max), "12");
        assert_eq!(r(stdev), "3.6055512754639893");
        assert_eq!(r(stdevp), "3.4520525295346632");
        assert_eq!(r(variance), "13");
        assert_eq!(r(variancep), "11.9166666666666667");
        assert_eq!(r(list), "7\r1\r3\r12\r5\r6\r8\r11\r9\r2\r10\r4");
        let due: Vec<Value> = [
            (2014, 2, 17),
            (2014, 9, 30),
            (2014, 11, 24),
            (2015, 8, 2),
            (2015, 8, 12),
            (2015, 9, 5),
            (2015, 9, 5),
            (2015, 9, 11),
            (2015, 9, 13),
            (2015, 9, 19),
            (2015, 10, 9),
            (2019, 8, 8),
        ]
        .iter()
        .map(|&(y, m, d)| date(y, m, d))
        .collect();
        let d = |f| run(f, due.clone(), &cx).0;
        assert_eq!(d(sum), "8830399");
        assert_eq!(d(average), "735866.5833333333333333");
        assert_eq!(d(max), "8/8/2019");
        assert_eq!(d(min), "2/17/2014");
        assert_eq!(d(stdev), "483.1373714555413038");
        // No related records.
        assert_eq!(run(count, Vec::new(), &cx).0, "0");
        for f in [
            sum, average, max, min, stdev, stdevp, variance, variancep, list,
        ] {
            assert!(f(&[Vec::new()], &cx).unwrap().is_empty());
        }
    }

    #[test]
    fn mixed_types_in_max_and_min() {
        let en = Locale::en_us();
        let cx = cx(&en);
        let hour = Value::Time(Time::from_seconds(&Number::from_i64(3600)).unwrap());
        let first = date(1, 1, 1);
        assert_eq!(
            run(max, vec![first.clone(), hour.clone()], &cx).0,
            "1/1/0001"
        );
        assert_eq!(
            run(min, vec![first.clone(), hour.clone()], &cx).0,
            "1:00:00"
        );
        assert_eq!(
            run(max, vec![hour.clone(), first.clone()], &cx).0,
            "1:00:00"
        );
        assert_eq!(run(min, vec![hour, first], &cx).0, "1/1/0001");
        let d = date(2001, 1, 1);
        assert_eq!(
            run(max, vec![d.clone(), Value::text("1/2/2001")], &cx).0,
            "1/2/2001"
        );
        assert_eq!(
            run(max, vec![Value::text("1/2/2001"), d.clone()], &cx).0,
            "1/2/2001"
        );
        assert_eq!(
            run(max, vec![d.clone(), Value::text("730487")], &cx).0,
            "1/1/2001"
        );
        assert_eq!(run(min, vec![d, Value::text("730487")], &cx).0, "730487");
        assert_eq!(
            run(max, vec![Value::text("10"), Value::number(9)], &cx).0,
            "10"
        );
        assert_eq!(
            run(max, vec![Value::text("10"), Value::text("9")], &cx).0,
            "9"
        );
    }
}
