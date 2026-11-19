//! Powers, roots, logarithms, `Factorial`, `Pi` and trigonometry (spec
//! `docs/re/calc-values.md` 2.4 and 14).
//!
//! The decimal family follows section 14 digit for digit: `Exp` reduces x
//! by k = round(x / ln 10) with a 45-digit ln 10 and sums the Taylor series
//! at max(45, p + 4 + k) digits (14.3); `Ln` scales into [0.6, 1.4] and sums
//! ln(1 + z) at p + 4 digits, `Log` and `Lg` multiply it by 1 / ln 10 or
//! 1 / ln 2 (14.4); `x ^ y` takes the integer part of y by binary
//! exponentiation at p + 10 digits with every product truncated and the
//! fraction as `Exp(f · Ln(x))`, and `Sqrt(x)` is `x ^ 0.5` (14.5). Results
//! carry guard digits by function (`Exp`, `Lg` 4, `Ln` 3, `Log` 5, `^` and
//! `Sqrt` the base's). Arguments take the evaluation precision when theirs
//! is lower, as a literal evaluated there would.
//!
//! The binary family (`Sin`, `Cos`, `Tan`, `Atan`, `Asin`, `Acos`, 14.8):
//! the argument is cut to p 16 (its whole words, so 18 fractional digits
//! with two guard digits: `Sin(1E-20)` is 0), `Sin`, `Cos` and `Tan` reduce
//! an argument outside [-1000, 1000] modulo 2π at that precision, the
//! decimal becomes a double the way `FixPt::AsFloat` builds it (beyond
//! about 10^324 a zero word gives NaN, so `Atan(10^400)` is 0), the function
//! runs on the double, and the double comes back through `%.20g` at the
//! argument's precision and guard digits. The function value here is the
//! correctly rounded one, computed in decimal so results do not depend on
//! the host's libm; FileMaker Server's glibc 2.39 differs from it near
//! rounding ties (`Sin(1529)`).

use super::consts::{tables, Table};
use super::fixpt::{add_into, as_float, assign_double, clamp_p, div, div_u, le, lt, mul, sub3, Fx};
use super::{mag, Fin, Number, GUARD};
use crate::error::CalcError;
use std::cmp::Ordering;

/// `^` fails with error 14 when a base, an exponent or the result reaches
/// 46 integer words (10^405); `Factorial` when its product does.
const POWER_LIMIT_DIGITS: u32 = 405;
const POWER_LIMIT_WORDS: usize = 46;
/// `Exp` of an argument above this is error 14; at or below its negation,
/// 0.
const EXP_ARGUMENT_LIMIT: i64 = 5000;
/// k! reaches 10^405 from this k on, so `Factorial(n; k)` fails for it.
const MAX_FACTORS: u64 = 213;

/// π truncated to `scale` fractional digits (`scale` <= 720).
pub(crate) fn pi_truncated(scale: u32) -> Fin {
    let pi = &tables().pi;
    Fin::new(
        false,
        mag::shr_digits(&pi.coef, pi.digits - scale).0,
        scale,
        16,
    )
}

fn one() -> Fin {
    Fin::from_i64(1, 16)
}

fn small(n: i64) -> Fin {
    Fin::from_i64(n, 16)
}

fn w_mul(a: &Fin, b: &Fin, scale: u32) -> Fin {
    a.mul_exact(b, 16).truncated(scale)
}

fn w_div(a: &Fin, b: &Fin, scale: u32) -> Fin {
    a.div_truncated(b, scale, 16)
}

/// `a / d` truncated to `scale` digits for a small divisor.
fn w_div_small(a: &Fin, d: u32, scale: u32) -> Fin {
    let a = a.truncated(scale);
    let coef = a.coef_at(scale);
    let (q, _) = mag::divmod_small(&coef, d);
    Fin::new(a.neg, q, scale, 16)
}

/// Integer square root (floor) by Newton's method.
fn isqrt(n: &[u32]) -> Vec<u32> {
    if n.is_empty() {
        return Vec::new();
    }
    // Start above the root: 10^ceil(digits / 2).
    let mut x = mag::pow10(mag::digit_count(n).div_ceil(2));
    loop {
        let (q, _) = mag::divmod(n, &x);
        let (y, _) = mag::divmod_small(&mag::add(&x, &q), 2);
        if mag::cmp(&y, &x) != Ordering::Less {
            return x;
        }
        x = y;
    }
}

/// √a truncated to `scale` digits (a >= 0).
fn sqrt_truncated(a: &Fin, scale: u32) -> Fin {
    // floor(sqrt(coef * 10^(2 scale - a.scale))) at scale `scale`.
    let shift = 2 * i64::from(scale) - i64::from(a.scale);
    let n = if shift >= 0 {
        mag::shl_digits(&a.coef, shift as u32)
    } else {
        mag::shr_digits(&a.coef, (-shift) as u32).0
    };
    Fin::new(false, isqrt(&n), scale, 16)
}

/// An approximate double of an exact decimal.
fn to_f64_exact(x: &Fin) -> f64 {
    let text = format!(
        "{}{}e-{}",
        if x.neg { "-" } else { "" },
        mag::to_digits(&x.coef),
        x.scale
    );
    text.parse().unwrap_or(0.0)
}

// The decimal family (14.3 - 14.5). Each routine evaluates into a result of
// display precision `rdisp` and `rextra` guard digits.

/// `Exp` (14.3).
fn exp_fx(x: &Fx, rdisp: u16, rextra: u16) -> Result<Fx, CalcError> {
    if x.nan {
        return Ok(Fx::nan_at(rdisp, rextra));
    }
    if le(x, &Fx::int(-EXP_ARGUMENT_LIMIT)) {
        return Ok(Fx::int_at(0, rdisp, rextra));
    }
    if !le(x, &Fx::int(EXP_ARGUMENT_LIMIT)) {
        return Err(CalcError::OUT_OF_RANGE);
    }
    let t = tables();
    let mut result = Fx::int_at(1, rdisp, rextra);
    result.widen(12, 796, 4);
    let mut a = x.abs();
    // The number the last series term went into; its precision is the
    // reciprocal's below.
    let mut temp = a.clone();
    if !x.is_zero() {
        // k = round(a / ln 10) with ln 10 to 45 digits.
        let mut power = mul(&Fx::fixed(&t.ln10_inv, 45), &a, &Fx::int(0));
        power.round(0);
        let k = mag::to_u64(power.int_part()).unwrap_or(0) as u32;
        let w = clamp_p(i64::from(result.disp) + i64::from(k));
        result.set_disp(w);
        a.set_disp(w);
        a.set_extra(result.extra);
        temp = a.clone();
        // r = a - k ln 10, exact at the precision of k × l45.
        let mut q = mul(
            &power,
            &Fx::fixed(&t.ln10, 45),
            &Fx::int_at(0, w, result.extra),
        );
        let r = sub3(&temp, &q, &Fx::int_at(0, w, result.extra));
        let mut term = r.clone();
        let mut n = 2u32;
        loop {
            add_into(&mut result, &term);
            q = div_u(&r, n, &q)?;
            temp = mul(&term, &q, &temp);
            term = temp.clone();
            if term.is_zero() {
                break;
            }
            n += 1;
        }
        result.mult10(k);
    }
    if x.neg {
        let mut out = temp;
        let e = out.extra.max(result.extra);
        let d0 = out.disp;
        if e > 0 {
            out.extra = e;
            out.set_disp(clamp_p(i64::from(d0)));
        }
        out.set_disp(clamp_p(i64::from(result.disp.max(out.disp).max(d0))));
        result = div(&Fx::int(1), &result, &out)?;
        result.set_disp(clamp_p(i64::from(result.disp)));
    }
    result.round(u32::from(result.extra) + u32::from(rdisp));
    result.set_disp(clamp_p(i64::from(rdisp)));
    Ok(result)
}

/// `Ln` (14.4).
fn ln_fx(x: &Fx, rdisp: u16, rextra: u16) -> Result<Fx, CalcError> {
    if x.nan {
        return Ok(Fx::nan_at(rdisp, rextra));
    }
    if x.is_zero() || x.neg {
        return Err(CalcError::OUT_OF_RANGE);
    }
    let t = tables();
    let mut result = Fx::int_at(0, rdisp, rextra);
    result.widen(12, 796, 4);
    let mut u = x.clone();
    u.set_disp(clamp_p(i64::from(result.disp)));
    u.set_extra(result.extra);
    let mut f8 = Fx::like(0, &result);
    // Decimal scaling into [0.1, 2).
    let mut shift10 = 0i64;
    if !lt(&u, &Fx::int(1)) {
        if !lt(&u, &Fx::int(2)) {
            let d = u.int_digits();
            u.div10(d);
            shift10 = i64::from(d);
        }
    } else {
        let first = u.first_frac_digit();
        if first > 1 {
            u.mult10(first - 1);
            shift10 = 1 - i64::from(first);
        }
    }
    // Binary scaling into [0.6, 1.4], the bounds from doubles.
    let mut hi = Fx::int(0);
    assign_double(&mut hi, 1.4);
    let mut lo = Fx::int(0);
    assign_double(&mut lo, 0.6);
    let mut shift2 = 0i64;
    if le(&u, &hi) {
        while lt(&u, &lo) {
            let c = u.clone();
            add_into(&mut u, &c);
            shift2 -= 1;
        }
    } else {
        loop {
            shift2 += 1;
            f8 = div_u(&u, 2, &f8)?;
            u = f8.clone();
            u.set_disp(clamp_p(i64::from(u.disp)));
            if le(&u, &hi) {
                break;
            }
        }
    }
    // ln(1 + z) = z - z²/2 + z³/3 - ...
    add_into(&mut u, &Fx::int(-1));
    let z = u;
    let mut numer = z.clone();
    let mut n = 1u32;
    let mut limit = 2 * i64::from(result.disp) + 200;
    loop {
        f8 = div_u(&numer, n, &f8)?;
        if n % 2 == 0 {
            add_into(&mut result, &f8.negated());
        } else {
            add_into(&mut result, &f8);
        }
        numer = mul(&numer, &z, &numer);
        if f8.is_zero() {
            break;
        }
        n += 1;
        limit -= 1;
        if limit < 1 {
            break;
        }
    }
    for (count, table) in [(shift10, &t.ln10), (shift2, &t.ln2)] {
        if count != 0 {
            let c = Fx::table(table, &result);
            let k = Fx::int_at(count, numer.disp, numer.extra);
            f8 = mul(&c, &k, &f8);
            add_into(&mut result, &f8);
        }
    }
    result.round(u32::from(result.extra) + u32::from(rdisp));
    result.set_disp(clamp_p(i64::from(rdisp)));
    Ok(result)
}

/// `Log` and `Lg` (14.4): `Ln` four digits wider times the reciprocal
/// constant.
fn log_fx(x: &Fx, rdisp: u16, rextra: u16, inverse: &Table) -> Result<Fx, CalcError> {
    if x.nan {
        return Ok(Fx::nan_at(rdisp, rextra));
    }
    let mut result = Fx::int_at(0, rdisp, rextra);
    result.widen(12, 796, 4);
    let like = Fx::like(0, &result);
    let ln = ln_fx(x, like.disp, like.extra)?;
    result = mul(&Fx::table(inverse, &result), &ln, &result);
    result.round(u32::from(result.extra) + u32::from(rdisp));
    result.set_disp(clamp_p(i64::from(rdisp)));
    Ok(result)
}

/// `x ^ y` (14.5).
pub(super) fn power_fx(x: &Fx, y: &Fx, rdisp: u16, rextra: u16) -> Result<Fx, CalcError> {
    if x.nan || y.nan {
        return Ok(Fx::nan_at(rdisp, rextra));
    }
    if x.int_words() >= POWER_LIMIT_WORDS || y.int_words() >= POWER_LIMIT_WORDS {
        return Err(CalcError::OUT_OF_RANGE);
    }
    let mut result = Fx::int_at(1, rdisp, rextra);
    result.widen(6, 790, 10);
    if x.neg && y.has_fraction() {
        return Err(CalcError::OUT_OF_RANGE);
    }
    // y = i + f, i toward zero, f with y's sign.
    let mut whole = y.clone();
    whole.truncate(0);
    let mut fraction = y.clone();
    fraction.m = mag::trim(fraction.m.iter().take(fraction.f).copied().collect());
    fraction.normalize();
    // A failed reciprocal (the power is 0 at the working digits) is error
    // 15 unless the fraction's Ln and Exp run after it: their outcome
    // replaces it and the result is the zero power times e^(f ln x)
    // (`.037828396891^-32.5555388` is 0 without an error, `0.1^-404` is
    // 15).
    let mut failed = None;
    if !whole.is_zero() {
        let mut n = whole.int_part().to_vec();
        let mut square = x.clone();
        square.set_disp(clamp_p(i64::from(result.disp)));
        square.set_extra(result.extra);
        let mut overflow = false;
        loop {
            if n.is_empty() {
                if y.neg {
                    match div(&Fx::like(1, &result), &result, &Fx::like(0, &result)) {
                        Ok(q) => {
                            result = q;
                            result.set_disp(clamp_p(i64::from(result.disp)));
                        }
                        Err(e) => failed = Some(e),
                    }
                }
                break;
            }
            if n[0] % 2 == 1 {
                result = mul(&result, &square, &result);
                if result.int_words() >= POWER_LIMIT_WORDS {
                    overflow = true;
                    break;
                }
            }
            n = mag::divmod_small(&n, 2).0;
            square = mul(&square, &square, &square);
            if !n.is_empty() && square.int_words() >= POWER_LIMIT_WORDS {
                overflow = true;
                break;
            }
        }
        if overflow {
            // 2^-1350 is 0 without an error.
            return if y.neg {
                Ok(Fx::int_at(0, rdisp, rextra))
            } else {
                Err(CalcError::OUT_OF_RANGE)
            };
        }
    }
    if !fraction.is_zero() {
        let mut c = result.clone();
        if x.is_zero() {
            c.m.clear();
            c.neg = false;
        } else {
            failed = None;
            let like = Fx::like(0, &c);
            let ln = ln_fx(x, like.disp, like.extra)?;
            let m = mul(&fraction, &ln, &Fx::like(0, &c));
            c = exp_fx(&m, c.disp, c.extra)?;
        }
        result = mul(&result, &c, &result);
    }
    if let Some(e) = failed {
        return Err(e);
    }
    result.round(u32::from(result.extra) + u32::from(rdisp));
    result.set_disp(clamp_p(i64::from(rdisp)));
    Ok(result)
}

/// The angle reduction of `Sin`, `Cos` and `Tan` (`Mod2Pi`, 14.8): outside
/// [-1000, 1000] the argument becomes `Mod(x; 2π)` with π the table
/// constant at its precision rounded at its held digits and the quotient
/// rounded at its held digits and floored.
fn mod_two_pi(a: &Fx) -> Fx {
    let limit = Fx::int(1000);
    if !lt(a, &limit.negated()) && le(a, &limit) {
        return a.clone();
    }
    let mut pi = Fx::table(&tables().pi, a);
    pi.round(pi.prec());
    let two_pi = mul(&pi, &Fx::int(2), &Fx::like(0, a));
    let out = Fx::like(0, &pi);
    // A quotient that fails (an argument of 10^801 or more) counts as 0.
    let k = match div(a, &two_pi, &out) {
        Ok(q) if !q.nan => {
            let mut k = q.int_part().to_vec();
            if q.neg && q.has_fraction() {
                k = mag::add(&k, &[1]);
            }
            Fx::from_words(q.neg, &k)
        }
        _ => Fx::int(0),
    };
    let t = mul(&k, &two_pi, &out);
    sub3(a, &t, &out)
}

impl Number {
    /// A double as `FixPt::operator=(double)` stores it at `precision`
    /// (`Random`, calc-functions.md 3.9): printed with `%.20g` and truncated
    /// to p + 2 digits; NaN and infinities are `?`.
    pub fn from_f64(value: f64, precision: u16) -> Number {
        from_f64_20g(value, precision)
    }

    /// `Pi` at the evaluation precision, holding p + 2 digits.
    pub fn pi(precision: u16) -> Number {
        let scale = u32::from(precision) + GUARD + 4;
        let mut f = pi_truncated(scale).rounded(i64::from(precision) + i64::from(GUARD));
        f.precision = precision;
        Number::from_fin(f)
    }

    /// `Sqrt`: `x ^ 0.5` with 0.5 from a double; error 14 for negative
    /// input.
    pub fn sqrt(&self, precision: u16) -> Result<Number, CalcError> {
        self.decimal(precision, |x| {
            let mut half = Fx::int(0);
            assign_double(&mut half, 0.5);
            power_fx(x, &half, x.disp, x.extra)
        })
    }

    /// `Exp`: 4 guard digits; error 14 above 5000 and between -5000 and
    /// about -1844.37, 0 at -5000 and below.
    pub fn exp(&self, precision: u16) -> Result<Number, CalcError> {
        self.decimal(precision, |x| exp_fx(x, x.disp, 4))
    }

    /// `Ln`: 3 guard digits; error 14 for zero and negatives.
    pub fn ln(&self, precision: u16) -> Result<Number, CalcError> {
        self.decimal(precision, |x| ln_fx(x, x.disp, 3))
    }

    /// `Log` (base 10): 5 guard digits; error 14 for zero and negatives.
    pub fn log10(&self, precision: u16) -> Result<Number, CalcError> {
        self.decimal(precision, |x| log_fx(x, x.disp, 5, &tables().ln10_inv))
    }

    /// `Lg` (base 2): 4 guard digits; error 14 for zero and negatives.
    pub fn log2(&self, precision: u16) -> Result<Number, CalcError> {
        self.decimal(precision, |x| log_fx(x, x.disp, 4, &tables().ln2_inv))
    }

    /// A single-argument decimal function: empty stays empty, `?` gives `?`.
    fn decimal(
        &self,
        precision: u16,
        f: impl FnOnce(&Fx) -> Result<Fx, CalcError>,
    ) -> Result<Number, CalcError> {
        if self.is_empty() {
            return Ok(Number::empty());
        }
        match self.fx_operand(precision) {
            None => Ok(Number::nan()),
            Some(x) => f(&x).map(Number::from_fx),
        }
    }

    /// `x ^ y` (spec 14.5) at the base's precision and guard digits.
    /// Errors: 14 when a base, an exponent or the result reaches 10^405 or
    /// a negative base has a fractional exponent, 15 for `0^-n` and for a
    /// negative exponent whose power is 0 at the working digits
    /// (`0.1^-404`).
    pub fn pow(&self, exponent: &Number, precision: u16) -> Result<Number, CalcError> {
        let (Some(x), Some(y)) = (self.fx_operand(precision), exponent.fx_operand(precision))
        else {
            return Ok(Number::nan());
        };
        power_fx(&x, &y, x.disp, x.extra).map(Number::from_fx)
    }

    /// `Factorial(n; k)`: n (n - 1) ... (n - k + 1), all of n! without k.
    /// n and k must be integers with 0 <= k <= n; otherwise, or when the
    /// result reaches 10^405, error 14. Empty counts as 0. n may be any size
    /// (`Factorial(10^30;3)`; oracle answers).
    pub fn factorial(&self, factors: Option<&Number>, precision: u16) -> Result<Number, CalcError> {
        let integer = |x: &Fin| -> Result<Vec<u32>, CalcError> {
            if x.neg || !x.is_integer() {
                return Err(CalcError::OUT_OF_RANGE);
            }
            Ok(mag::shr_digits(&x.coef, x.scale).0)
        };
        let Some(n) = self.operand() else {
            return Ok(Number::nan());
        };
        let n = integer(&n)?;
        let k = match factors {
            None => n.clone(),
            Some(k) => {
                let Some(k) = k.operand() else {
                    return Ok(Number::nan());
                };
                let k = integer(&k)?;
                if mag::cmp(&k, &n) == Ordering::Greater {
                    return Err(CalcError::OUT_OF_RANGE);
                }
                k
            }
        };
        // The product is at least k!, and 213! reaches 10^405.
        let Some(k) = mag::to_u64(&k).filter(|&k| k < MAX_FACTORS) else {
            return Err(CalcError::OUT_OF_RANGE);
        };
        let mut product = vec![1u32];
        let mut factor = n;
        for _ in 0..k {
            product = mag::mul(&product, &factor);
            if mag::digit_count(&product) > POWER_LIMIT_DIGITS {
                return Err(CalcError::OUT_OF_RANGE);
            }
            factor = mag::sub(&factor, &[1]);
        }
        Ok(Number::from_fin(Fin::new(false, product, 0, precision)))
    }

    /// `Sin`.
    pub fn sin(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Sin)
    }

    /// `Cos`.
    pub fn cos(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Cos)
    }

    /// `Tan`.
    pub fn tan(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Tan)
    }

    /// `Atan`.
    pub fn atan(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Atan)
    }

    /// `Asin`: error 1218 outside [-1, 1].
    pub fn asin(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Asin)
    }

    /// `Acos`: error 1218 outside [-1, 1].
    pub fn acos(&self, precision: u16) -> Result<Number, CalcError> {
        self.trig(precision, Trig::Acos)
    }

    fn trig(&self, precision: u16, f: Trig) -> Result<Number, CalcError> {
        if self.is_empty() {
            return Ok(Number::empty());
        }
        let Some(x) = self.fx_operand(precision) else {
            return Ok(Number::nan());
        };
        // The domain is checked on the double of the whole argument.
        if matches!(f, Trig::Asin | Trig::Acos) && !(-1.0..=1.0).contains(&as_float(&x)) {
            return Err(CalcError::CANNOT_EVALUATE_NUMBER);
        }
        let mut a = x.clone();
        a.set_precision(16);
        if matches!(f, Trig::Sin | Trig::Cos | Trig::Tan) {
            a = mod_two_pi(&a);
        }
        let d = as_float(&a);
        let value = if d.is_nan() {
            f64::NAN
        } else if d.is_infinite() {
            match f {
                Trig::Atan => std::f64::consts::FRAC_PI_2.copysign(d),
                _ => f64::NAN,
            }
        } else {
            match f {
                Trig::Sin => cr_sin(d),
                Trig::Cos => cr_cos(d),
                Trig::Tan => cr_tan(d),
                Trig::Atan => cr_atan(d),
                Trig::Asin => cr_asin(d),
                Trig::Acos => cr_acos(d),
            }
        };
        let mut out = Fx::int_at(0, 16, x.extra);
        assign_double(&mut out, value);
        out.set_precision(i64::from(x.disp));
        Ok(Number::from_fx(out))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trig {
    Sin,
    Cos,
    Tan,
    Atan,
    Asin,
    Acos,
}

/// `FixPt::operator=(double)` for a fresh number at `precision`: the
/// double printed with `%.20g`, parsed, and truncated to p + 2 digits
/// (`Random`); NaN and infinities are `?`.
fn from_f64_20g(value: f64, precision: u16) -> Number {
    if !value.is_finite() {
        return Number::nan();
    }
    let text = format!("{value:.19e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let neg = mantissa.starts_with('-');
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    // value = 0.d1d2...d20 * 10^(exponent + 1)
    let scale = 19 - exponent;
    let coef = mag::from_digits(&digits);
    let fin = if scale >= 0 {
        Fin::new(neg, coef, scale as u32, precision)
    } else {
        Fin::new(neg, mag::shl_digits(&coef, (-scale) as u32), 0, precision)
    };
    Number::from_fin(fin.truncated(u32::from(precision) + GUARD))
}

/// The exact decimal value of a finite double.
fn exact_decimal(d: f64) -> Fin {
    let bits = d.to_bits();
    let neg = bits >> 63 == 1;
    let exp_bits = ((bits >> 52) & 0x7ff) as i64;
    let frac_bits = bits & ((1 << 52) - 1);
    let (m, e) = if exp_bits == 0 {
        (frac_bits, -1074)
    } else {
        (frac_bits | (1 << 52), exp_bits - 1075)
    };
    let mut coef = mag::from_u64(m);
    if e >= 0 {
        for _ in 0..e {
            coef = mag::mul_small(&coef, 2);
        }
        Fin::new(neg, coef, 0, 16)
    } else {
        for _ in 0..(-e) {
            coef = mag::mul_small(&coef, 5);
        }
        Fin::new(neg, coef, (-e) as u32, 16)
    }
}

/// The nearest double to an exact decimal.
fn nearest_double(x: &Fin) -> f64 {
    to_f64_exact(x)
}

/// Working scale for a trigonometric argument or result near `d`.
fn trig_scale(d: f64) -> u32 {
    let lead = if d == 0.0 { 0.0 } else { -d.abs().log10() };
    60 + lead.max(0.0).ceil() as u32
}

/// sin and cos of a small decimal |r| <= π/4 by their Taylor series.
fn sin_cos_series(r: &Fin, scale: u32) -> (Fin, Fin) {
    let r2 = w_mul(r, r, scale);
    let mut term = r.truncated(scale);
    let mut sin = term.clone();
    let mut n = 1u32;
    loop {
        term = w_div_small(&w_mul(&term, &r2, scale), (2 * n) * (2 * n + 1), scale).negated();
        if term.is_zero() {
            break;
        }
        sin = sin.add_exact(&term, 16);
        n += 1;
    }
    let mut term = one();
    let mut cos = one();
    let mut n = 1u32;
    loop {
        term = w_div_small(&w_mul(&term, &r2, scale), (2 * n - 1) * (2 * n), scale).negated();
        if term.is_zero() {
            break;
        }
        cos = cos.add_exact(&term, 16);
        n += 1;
    }
    (sin, cos)
}

/// (sin d, cos d) to about 60 significant digits.
fn sin_cos(d: f64) -> (Fin, Fin) {
    let x = exact_decimal(d);
    let scale = trig_scale(d) + 20;
    let half_pi = w_div_small(&pi_truncated(scale + 10), 2, scale + 8);
    // k = round(x / (π/2))
    let q = w_div(&x, &half_pi, 2);
    let k = q.rounded(0).to_i64_trunc().unwrap_or(0);
    let r = x.add_exact(&half_pi.mul_exact(&small(k), 16).negated(), 16);
    let r = r.truncated(scale);
    let (s, c) = sin_cos_series(&r, scale);
    match k.rem_euclid(4) {
        0 => (s, c),
        1 => (c, s.negated()),
        2 => (s.negated(), c.negated()),
        _ => (c.negated(), s),
    }
}

fn cr_sin(d: f64) -> f64 {
    if d == 0.0 {
        return d;
    }
    nearest_double(&sin_cos(d).0)
}

fn cr_cos(d: f64) -> f64 {
    nearest_double(&sin_cos(d).1)
}

fn cr_tan(d: f64) -> f64 {
    if d == 0.0 {
        return d;
    }
    let (s, c) = sin_cos(d);
    if c.is_zero() {
        return f64::INFINITY;
    }
    nearest_double(&w_div(&s, &c, trig_scale(d) + 40))
}

/// atan of an exact decimal.
fn atan_fin(x: &Fin, scale: u32) -> Fin {
    let neg = x.neg;
    let a = x.abs();
    let one = one();
    let (mut y, offset) = if a.cmp_exact(&one) == Ordering::Greater {
        (w_div(&one, &a, scale), true)
    } else {
        (a.truncated(scale), false)
    };
    // atan y = 2 atan(y / (1 + sqrt(1 + y^2))), twice.
    for _ in 0..2 {
        let root = sqrt_truncated(&one.add_exact(&w_mul(&y, &y, scale), 16), scale);
        y = w_div(&y, &one.add_exact(&root, 16), scale);
    }
    let y2 = w_mul(&y, &y, scale);
    let mut power = y.clone();
    let mut sum = y.clone();
    let mut k = 1u32;
    loop {
        power = w_mul(&power, &y2, scale).negated();
        let term = w_div_small(&power, 2 * k + 1, scale);
        if term.is_zero() {
            break;
        }
        sum = sum.add_exact(&term, 16);
        k += 1;
    }
    let mut result = sum.mul_exact(&small(4), 16);
    if offset {
        let half_pi = w_div_small(&pi_truncated(scale + 4), 2, scale);
        result = half_pi.add_exact(&result.negated(), 16);
    }
    if neg {
        result.negated()
    } else {
        result
    }
}

fn cr_atan(d: f64) -> f64 {
    if d == 0.0 {
        return d;
    }
    let scale = trig_scale(d) + 20;
    nearest_double(&atan_fin(&exact_decimal(d), scale))
}

fn asin_fin(d: f64, scale: u32) -> Fin {
    let x = exact_decimal(d);
    let one = one();
    let a = x.abs();
    if a.cmp_exact(&one) == Ordering::Equal {
        let half_pi = w_div_small(&pi_truncated(scale + 4), 2, scale);
        return if x.neg { half_pi.negated() } else { half_pi };
    }
    let root = sqrt_truncated(
        &one.add_exact(&w_mul(&x, &x, scale + 10).negated(), 16),
        scale + 10,
    );
    atan_fin(&w_div(&x, &root, scale + 5), scale)
}

fn cr_asin(d: f64) -> f64 {
    if d == 0.0 {
        return d;
    }
    nearest_double(&asin_fin(d, trig_scale(d) + 20))
}

fn cr_acos(d: f64) -> f64 {
    let scale = 80;
    let half_pi = w_div_small(&pi_truncated(scale + 4), 2, scale);
    nearest_double(&half_pi.add_exact(&asin_fin(d, scale).negated(), 16))
}

#[cfg(test)]
mod tests {
    use crate::number::Number;

    fn lit(s: &str) -> Number {
        match s.strip_prefix('-') {
            Some(rest) => Number::from_literal(rest, '.').unwrap().neg(),
            None => Number::from_literal(s, '.').unwrap(),
        }
    }

    fn show(n: &Number) -> String {
        n.to_text_with('.')
    }

    #[test]
    fn decimal_functions() {
        assert_eq!(show(&lit("2").sqrt(16).unwrap()), "1.414213562373095");
        assert_eq!(
            show(&lit("2").sqrt(50).unwrap()),
            "1.41421356237309504880168872420969807856967187537695"
        );
        assert_eq!(show(&lit("1").exp(16).unwrap()), "2.7182818284590452");
        assert_eq!(
            show(&lit("1").exp(50).unwrap()),
            "2.71828182845904523536028747135266249775724709369996"
        );
        assert_eq!(show(&lit("-1").exp(16).unwrap()), ".3678794411714423");
        assert_eq!(show(&lit("-1000").exp(16).unwrap()), "0");
        assert_eq!(
            show(&lit("2").ln(50).unwrap()),
            ".69314718055994530941723212145817656807550013436026"
        );
        assert_eq!(show(&lit("100").log10(16).unwrap()), "2");
        assert_eq!(show(&lit("0.001").log10(16).unwrap()), "-3");
        assert_eq!(show(&lit("3").log2(16).unwrap()), "1.5849625007211562");
        assert!(lit("0").ln(16).is_err());
        assert!(lit("-4").sqrt(16).is_err());
        assert_eq!(show(&Number::pi(16)), "3.1415926535897932");
    }

    #[test]
    fn powers() {
        let pow = |a: &str, b: &str| lit(a).pow(&lit(b), 16).map(|n| show(&n));
        assert_eq!(pow("2", "10").unwrap(), "1024");
        assert_eq!(pow("2", "0.5").unwrap(), "1.414213562373095");
        assert_eq!(pow("2", "-10").unwrap(), ".0009765625");
        assert_eq!(pow("-2", "3").unwrap(), "-8");
        assert_eq!(pow("0", "0").unwrap(), "1");
        assert_eq!(pow("0", "0.5").unwrap(), "0");
        let third = lit("1").div(&lit("3"), 16).unwrap();
        assert_eq!(show(&lit("27").pow(&third, 16).unwrap()), "3");
        assert_eq!(show(&lit("1000").pow(&third, 16).unwrap()), "10");
        assert_eq!(pow("1.0001", "10000").unwrap(), "2.7181459268252249");
        assert_eq!(pow("0.9999", "10000").unwrap(), ".3678610464329299");
        assert!(pow("10", "404").is_ok());
        assert_eq!(pow("10", "405").unwrap_err().code(), 14);
        assert_eq!(pow("3", "850").unwrap_err().code(), 14);
        assert_eq!(pow("-8", "0.5").unwrap_err().code(), 14);
        assert_eq!(pow("0", "-1").unwrap_err().code(), 15);
        assert_eq!(pow("0.1", "-404").unwrap_err().code(), 15);
        assert_eq!(pow("10", "-500").unwrap(), "0");
        // The reciprocal of a power that is 0 at the working digits fails,
        // unless the fraction's Ln and Exp run after it.
        assert_eq!(pow(".037828396891", "-32.5555388").unwrap(), "0");
        assert_eq!(pow("0", "-1.5").unwrap_err().code(), 15);
        assert_eq!(pow("0", "-0.5").unwrap(), "0");
        // Square-and-multiply with truncated products.
        let held = |a: &str, b: &str| {
            let n = lit(a).pow(&lit(b), 16).unwrap();
            show(&n.mul(&lit("1000000000000000000"), 16))
        };
        assert_eq!(held("1.5", "100"), "406561177535215237397279707567041643");
        assert_eq!(
            show(&lit("1000000000000000000000000000000").sqrt(16).unwrap()),
            "999999999999999.9999999999999345"
        );
    }

    #[test]
    fn factorial() {
        let f =
            |n: &str, k: Option<&str>| lit(n).factorial(k.map(lit).as_ref(), 16).map(|n| show(&n));
        assert_eq!(f("0", None).unwrap(), "1");
        assert_eq!(f("3", Some("2")).unwrap(), "6");
        assert_eq!(f("5", Some("0")).unwrap(), "1");
        assert!(f("212", None).is_ok());
        assert_eq!(f("213", None).unwrap_err().code(), 14);
        assert_eq!(f("2.9", None).unwrap_err().code(), 14);
        assert_eq!(f("10", Some("20")).unwrap_err().code(), 14);
    }

    #[test]
    fn trigonometry_through_doubles() {
        let t = |s: &str, f: fn(&Number, u16) -> Result<Number, crate::CalcError>| {
            show(&f(&lit(s), 16).unwrap())
        };
        assert_eq!(t("1", Number::sin), ".8414709848078965");
        assert_eq!(t("21", Number::sin), ".8366556385360561");
        assert_eq!(t("200", Number::sin), "-.8732972972139946");
        assert_eq!(t("2000", Number::sin), ".9300395044161369");
        assert_eq!(t("-10000", Number::sin), ".3056143888882539");
        assert_eq!(t("100000000000000000000", Number::sin), ".993720817727796");
        assert_eq!(t("1", Number::atan), ".7853981633974483");
        assert_eq!(t("0.5", Number::acos), "1.0471975511965979");
        assert_eq!(t("1", Number::asin), "1.5707963267948966");
        assert_eq!(lit("2").asin(16).unwrap_err().code(), 1218);
        let sin1 = lit("1").sin(16).unwrap();
        assert_eq!(
            show(&sin1.mul(&lit("1000000000000000000"), 16)),
            "841470984807896504"
        );
        let small = lit("1e-15").sin(16).unwrap();
        assert_eq!(show(&small.mul(&lit("1000000000000000"), 16)), ".999");
        let pi = Number::pi(16);
        assert_eq!(show(&pi.sin(16).unwrap()), "1.0E-16");
        let half_pi = pi.div(&lit("2"), 16).unwrap();
        assert_eq!(show(&half_pi.tan(16).unwrap()), "16331239353195370");
        // The argument is cut to 18 fractional digits.
        assert_eq!(t("0.0000000000000000015", Number::sin), "1.0E-18");
        assert_eq!(t("1e-20", Number::sin), "0");
        // Beyond 10^324 a zero word makes the double NaN.
        assert_eq!(t("1e400", Number::atan), "0");
        assert_eq!(t("1e309", Number::atan), "1.5707963267948966");
        assert_eq!(lit("1e400").asin(16).unwrap_err().code(), 1218);
        // The result keeps the argument's guard digits.
        let e = lit("1").exp(16).unwrap();
        assert_eq!(
            show(&e.sin(16).unwrap().mul(&lit("1000000000000000000"), 16)),
            "410781290502908402.74"
        );
    }
}
