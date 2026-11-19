//! Financial functions: `FV`, `PMT`, `PV`, `NPV`.
//!
//! Spec: `calc-functions.md` 5.1 and the financial oracle answers at the
//! end of its section 9, `calc-values.md` 14.6; the arithmetic is in
//! [`Number::future_value`] and friends. Arguments are numbers by
//! [`Value::numeric_operand`]; an empty or digitless argument counts as 0
//! (`FV(100;"";10)` = 1000) and only all three empty give empty. `?` gives
//! `?` without an error.
//!
//! `NPV(payment; interestRate)` is an aggregate: the parser requires a
//! field for `payment` (a literal or any other expression is error 1214,
//! `NPV("";.1)`, `NPV(100;.1)`), and the evaluator expands it into every
//! related record or repetition (`NPV(Rel::x; .1)` walks all related
//! records; a plain field is one payment) and passes `interestRate` as a
//! single value. Empty and digitless payments are skipped, so they take no
//! period, and a `?` payment makes the result `?` (calc-values.md 14.6).
//! No payment at all gives 0, not empty.

use super::{Function, Keyword};
use crate::{CalcError, Ctx, Number, Value};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("FV", fv),
    Function::aggregate("NPV", npv),
    Function::pure("PMT", pmt),
    Function::pure("PV", pv),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

type Annuity = fn(&Number, &Number, &Number, u16) -> Result<Number, CalcError>;

fn annuity(args: &[Value], cx: &Ctx, f: Annuity) -> Result<Value, CalcError> {
    if args.iter().all(Value::is_empty) {
        return Ok(args[0].clone());
    }
    let n = |i: usize| args[i].numeric_operand(cx);
    f(&n(0), &n(1), &n(2), cx.precision).map(Value::Number)
}

/// `FV(payment; interestRate; periods)`.
fn fv(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    annuity(args, cx, Number::future_value)
}

/// `PV(payment; interestRate; periods)`.
fn pv(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    annuity(args, cx, Number::present_value)
}

/// `PMT(principal; interestRate; term)`.
fn pmt(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    annuity(args, cx, Number::payment)
}

/// `NPV(payment; interestRate)`.
fn npv(lists: &[Vec<Value>], cx: &Ctx) -> Result<Value, CalcError> {
    let payments: Vec<Number> = lists[0]
        .iter()
        .map(|v| v.to_number(cx))
        .filter(|n| !n.is_empty())
        .collect();
    let rate = lists
        .get(1)
        .and_then(|rate| rate.first())
        .map_or_else(Number::empty, |v| v.numeric_operand(cx));
    Number::net_present_value(&payments, &rate, cx.precision).map(Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Date, Locale};

    #[test]
    fn npv_over_expanded_payments() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        let amounts: Vec<Value> = [-5000, 10000, 0, 10000, 10000]
            .iter()
            .map(|&v| Value::number(v))
            .collect();
        let rate = vec![Value::Number(Number::from_literal(".10", '.').unwrap())];
        let r = npv(&[amounts.clone(), rate.clone()], &cx).unwrap();
        // Claris: 16758.35604870...; the 0 is a period of its own.
        assert_eq!(r.to_text(&cx).as_str(), "16758.3560487050685808");
        let mut gaps = amounts;
        gaps.insert(1, Value::empty_text());
        gaps.insert(3, Value::text("abc"));
        let r = npv(&[gaps.clone(), rate.clone()], &cx).unwrap();
        assert_eq!(r.to_text(&cx).as_str(), "16758.3560487050685808");
        gaps.push(Value::text("?"));
        assert!(npv(&[gaps, rate.clone()], &cx).unwrap().is_invalid());
        let none = npv(&[Vec::new(), rate], &cx).unwrap();
        assert_eq!(none.to_text(&cx).as_str(), "0");
        let all_empty = fv(
            &[
                Value::empty_text(),
                Value::empty_text(),
                Value::empty_text(),
            ],
            &cx,
        );
        assert!(all_empty.unwrap().is_empty());
        let one_empty = fv(
            &[Value::number(100), Value::empty_text(), Value::number(10)],
            &cx,
        );
        assert_eq!(one_empty.unwrap().to_text(&cx).as_str(), "1000");
    }
}
