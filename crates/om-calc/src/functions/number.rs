//! Number functions: `Abs`, `Ceiling`, `Combination`, `Div`, `Exp`,
//! `Factorial`, `Floor`, `Int`, `Lg`, `Ln`, `Log`, `Mod`, `Round`, `Sign`,
//! `Sqrt`, `Truncate`, and `GetAsBoolean`. `SetPrecision` and `Random` belong
//! to the evaluator.
//!
//! Spec: `calc-functions.md` 5.1 and the number oracle answers at the end of
//! its section 9, `calc-values.md` 2.3, 2.4, 6.4 and 14. Arguments are
//! numbers by [`Value::numeric_operand`]: dates are day numbers, times and
//! timestamps seconds, text goes through the lenient parser and text
//! without digits is 0 (`Abs("abc")` = 0, `Lg("abc")` is error 14). An empty
//! argument of a single-argument function comes back unchanged (`Abs("")`
//! is empty text, `Abs(GetAsDate(" "))` an empty date). `Div` and `Mod`
//! follow the operators: both empty give the first, one empty counts as 0,
//! except that an empty divisor of `Mod` is error 15 (`Mod(5;"")`, while
//! `Mod(5;"abc")` = 5).

use super::{arg, Function, Keyword};
use crate::{ops, CalcError, Ctx, Number, Value};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("Abs", abs),
    Function::pure("Ceiling", ceiling),
    Function::pure("Combination", combination),
    Function::pure("Div", div),
    Function::pure("Exp", exp),
    Function::pure("Factorial", factorial),
    Function::pure("Floor", floor),
    Function::pure("GetAsBoolean", get_as_boolean),
    Function::pure("Int", int),
    Function::pure("Lg", lg),
    Function::pure("Ln", ln),
    Function::pure("Log", log),
    Function::pure("Mod", modulo),
    Function::pure("Round", round),
    Function::pure("Sign", sign),
    Function::pure("Sqrt", sqrt),
    Function::pure("Truncate", truncate),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

/// A single-argument numeric function: an empty argument comes back
/// unchanged, anything else is computed on its number at the evaluation
/// precision.
pub(super) fn unary(
    args: &[Value],
    cx: &Ctx,
    f: impl FnOnce(&Number, u16) -> Result<Number, CalcError>,
) -> Result<Value, CalcError> {
    let x = &args[0];
    if x.is_empty() {
        return Ok(x.clone());
    }
    f(&x.numeric_operand(cx), cx.precision).map(Value::Number)
}

/// `Abs`: a time stays a time and a timestamp is returned as it is (it is
/// never negative); everything else becomes a number (`Abs(Date(1;1;2001))`
/// = 730486).
fn abs(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(match &args[0] {
        Value::Timestamp(_) => args[0].clone(),
        x => ops::abs(x, cx),
    })
}

fn ceiling(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, _| Ok(x.ceiling()))
}

fn floor(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, _| Ok(x.floor()))
}

fn int(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, _| Ok(x.int()))
}

fn sign(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, _| Ok(x.sign()))
}

fn exp(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::exp)
}

fn ln(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::ln)
}

fn log(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::log10)
}

fn lg(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::log2)
}

fn sqrt(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::sqrt)
}

fn get_as_boolean(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::boolean(args[0].is_true(cx)))
}

/// The digits argument of `Round` and `Truncate`: `FixPt::AsLongLong`
/// narrowed to a 32-bit `int`, so it wraps modulo 2^32
/// (`Round(1.5;4294967297)` = 1.5, `Round(15;-4294967297)` = 20); empty,
/// text without digits and `?` are 0.
fn digits(v: &Value, cx: &Ctx) -> i32 {
    v.numeric_operand(cx).as_long_long() as i32
}

/// `FixPt::Round` and `Truncate` at a 32-bit digits argument: the smallest
/// `int` leaves the number unchanged and digits within 7 of the largest
/// overflow inside FixPt and give 0 (`Round(1.55;2147483640)` = 1.55,
/// `Round(1.55;2147483641)` = 0; oracle answers).
fn at_digits(x: &Number, digits: i32, op: impl FnOnce(&Number, i64) -> Number) -> Number {
    const OVERFLOW: i32 = i32::MAX - 6;
    if digits == i32::MIN {
        x.clone()
    } else if digits >= OVERFLOW {
        x.round(i64::from(i32::MIN) + 1)
    } else {
        op(x, i64::from(digits))
    }
}

/// `Round(number; digits)`: half away from zero (`calc-values.md` 2.3).
fn round(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let d = digits(&args[1], cx);
    unary(args, cx, |x, _| Ok(at_digits(x, d, Number::round)))
}

/// `Truncate(number; digits)`: toward zero after rounding to the display
/// precision.
fn truncate(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let d = digits(&args[1], cx);
    unary(args, cx, |x, _| Ok(at_digits(x, d, Number::truncate)))
}

/// `Mod(number; divisor)`: the floor remainder with the divisor's sign;
/// `Mod(x; 0)` is x.
fn modulo(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let (x, y) = (&args[0], &args[1]);
    if x.is_empty() && y.is_empty() {
        return Ok(x.clone());
    }
    if y.is_empty() {
        return Err(CalcError::DIVIDE_BY_ZERO);
    }
    let r = x.to_number(cx).modulo(&y.numeric_operand(cx), cx.precision);
    Ok(Value::Number(r))
}

/// `Div(number; divisor)`: floor division; error 15 for a zero, empty or
/// digitless divisor.
fn div(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let (x, y) = (&args[0], &args[1]);
    if x.is_empty() && y.is_empty() {
        return Ok(x.clone());
    }
    x.to_number(cx)
        .div_int(&y.to_number(cx), cx.precision)
        .map(Value::Number)
}

/// `Factorial(number {; numberOfFactors})`: an empty number gives 1 whatever
/// the count (`Factorial("";2)` = 1, while `Factorial(0;2)` and
/// `Factorial(" ";2)` are error 14).
fn factorial(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    let n = &args[0];
    if n.is_empty() {
        return Ok(Value::number(1));
    }
    let k = arg(args, 1).map(|k| k.numeric_operand(cx));
    n.numeric_operand(cx)
        .factorial(k.as_ref(), cx.precision)
        .map(Value::Number)
}

/// `Combination(setSize; numberOfChoices)`: FallFact(n; k) / k!. An empty
/// argument comes back unchanged; `?` gives `?`; non-integers, negatives
/// and k > n are error 14, and so is a falling factorial of 10^405 or more
/// (`Combination(1000;500)`, although the result has 300 digits).
fn combination(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    for v in args {
        if v.is_empty() {
            return Ok(v.clone());
        }
    }
    let n = args[0].numeric_operand(cx);
    let k = args[1].numeric_operand(cx);
    if n.is_nan() || k.is_nan() {
        return Ok(Value::invalid());
    }
    let falling = n.factorial(Some(&k), cx.precision)?;
    let choices = k.factorial(None, cx.precision)?;
    falling.div(&choices, cx.precision).map(Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Date, Locale};

    fn call(f: super::super::Pure, args: &[Value]) -> (String, i32) {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        match f(args, &cx) {
            Ok(v) => (v.to_text(&cx).into_string(), 0),
            Err(e) => ("?".into(), e.code()),
        }
    }

    fn n(s: &str) -> Value {
        Value::Number(Number::from_literal(s, '.').unwrap())
    }

    #[test]
    fn digits_wrap_like_a_32_bit_int() {
        let r = |x: &str, d: Value| call(round, &[n(x), d]).0;
        assert_eq!(r("1.5", n("4294967296")), "2");
        assert_eq!(r("1.5", n("4294967297")), "1.5");
        let minus = Value::Number(Number::from_literal("4294967297", '.').unwrap().neg());
        assert_eq!(r("15", minus), "20");
        assert_eq!(r("1.55", n("2147483640")), "1.55");
        assert_eq!(r("1.55", n("2147483641")), "0");
        assert_eq!(r("1.55", n("2147483648")), "1.55");
        assert_eq!(r("1.55", n("18446744073709551617")), "1.6");
        assert_eq!(r("1.55", n("1000000000000000000000000000000")), "2");
        assert_eq!(r("2.5", Value::empty_text()), "3");
        assert_eq!(r("2.5", Value::invalid()), "3");
        assert_eq!(call(truncate, &[n("1.55"), n("2147483646")]).0, "0");
    }

    #[test]
    fn empty_and_digitless_arguments() {
        assert_eq!(
            call(modulo, &[n("5"), Value::empty_text()]),
            ("?".into(), 15)
        );
        assert_eq!(call(modulo, &[n("5"), Value::text("abc")]).0, "5");
        assert_eq!(call(modulo, &[Value::empty_text(), n("2")]).0, "0");
        assert_eq!(call(div, &[n("5"), Value::text("abc")]).1, 15);
        assert_eq!(call(factorial, &[Value::empty_text(), n("2")]).0, "1");
        assert_eq!(call(factorial, &[Value::text(" "), n("2")]).1, 14);
        assert_eq!(call(combination, &[n("5"), Value::empty_text()]).0, "");
        assert_eq!(call(combination, &[n("1000"), n("500")]).1, 14);
        assert_eq!(
            call(combination, &[n("100"), n("50")]).0,
            "100891344545564193334812497256"
        );
        assert_eq!(call(lg, &[Value::text("abc")]).1, 14);
        assert_eq!(call(exp, &[Value::text("abc")]).0, "1");
    }
}
