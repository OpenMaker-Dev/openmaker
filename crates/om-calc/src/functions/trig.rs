//! Trigonometric functions: `Acos`, `Asin`, `Atan`, `Cos`, `Degrees`, `Pi`,
//! `Radians`, `Sin`, `Tan`.
//!
//! Spec: `calc-functions.md` 3.8, `calc-values.md` 2.4, 13 and 14.8. `Sin`,
//! `Cos`, `Tan`, `Atan`, `Asin` and `Acos` run on a C `double`
//! ([`Number::sin`] and friends: the argument cut to 18 fractional digits,
//! reduction modulo 2π outside [-1000, 1000], `FixPt::AsFloat`'s word-by-word
//! conversion, `%.20g` back); `Asin` and `Acos` outside [-1, 1] are error
//! 1218. `Pi`, `Degrees`
//! and `Radians` stay decimal: `Degrees(x)` is `x * 180 / Pi` and
//! `Radians(x)` `x * Pi / 180` with the language's operators and `Pi` held
//! at p + 2 digits, so `Degrees(1)*10^18` is 57295779513082320885 where
//! 180/π is ...876798 (oracle answers). Arguments convert as in the number
//! functions; an empty argument comes back unchanged.

use super::number::unary;
use super::{Function, Keyword};
use crate::{CalcError, Ctx, Number, Value};

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("Acos", acos),
    Function::pure("Asin", asin),
    Function::pure("Atan", atan),
    Function::pure("Cos", cos),
    Function::pure("Degrees", degrees),
    Function::pure("Pi", pi),
    Function::pure("Radians", radians),
    Function::pure("Sin", sin),
    Function::pure("Tan", tan),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

fn sin(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::sin)
}

fn cos(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::cos)
}

fn tan(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::tan)
}

fn atan(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::atan)
}

fn asin(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::asin)
}

fn acos(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, Number::acos)
}

/// `Pi` at the evaluation precision, holding p + 2 digits.
fn pi(_args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    Ok(Value::Number(Number::pi(cx.precision)))
}

/// `Degrees(x)` = `x * 180 / Pi`, `Pi` at the argument's precision if that
/// is larger (`Degrees(1.0000000000000000001)` = `57.2957795130823208825`).
fn degrees(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, p| {
        let pi = Number::pi(p.max(x.precision()));
        x.mul(&Number::from_i64(180), p).div(&pi, p)
    })
}

/// `Radians(x)` = `x * Pi / 180`, `Pi` as in `Degrees`.
fn radians(args: &[Value], cx: &Ctx) -> Result<Value, CalcError> {
    unary(args, cx, |x, p| {
        let pi = Number::pi(p.max(x.precision()));
        x.mul(&pi, p).div(&Number::from_i64(180), p)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Date, Locale};

    fn show(f: super::super::Pure, args: &[Value], precision: u16) -> String {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap()).with_precision(precision);
        f(args, &cx).unwrap().to_text(&cx).into_string()
    }

    #[test]
    fn pi_and_angle_conversions() {
        assert_eq!(show(pi, &[], 16), "3.1415926535897932");
        assert_eq!(show(pi, &[], 30), "3.14159265358979323846264338328");
        let one = [Value::number(1)];
        assert_eq!(show(degrees, &one, 16), "57.2957795130823209");
        assert_eq!(
            show(degrees, &one, 40),
            "57.2957795130823208767981548141051703324055"
        );
        assert_eq!(show(radians, &one, 16), ".0174532925199433");
        assert_eq!(
            show(radians, &[Value::number(180)], 16),
            "3.1415926535897932"
        );
        assert_eq!(show(degrees, &[Value::empty_text()], 16), "");
    }
}
