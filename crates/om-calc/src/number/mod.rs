//! FileMaker's decimal number, `Draco::FixPt` (spec
//! `docs/re/calc-values.md` 2 and 14).
//!
//! A number is empty, `?` (NaN) or a sign-magnitude decimal held in base
//! 10^9 words. It has a display precision p (fractional digits shown, 16 by
//! default) and g guard digits (14.1): it holds p + g fractional digits that
//! arithmetic, truthiness and `*10^18`-style probes see but display and
//! comparison round away. Literals, text and ordinary results have g = 2,
//! `Exp` and `Lg` results 4, `Ln` 3 and `Log` 5; arithmetic keeps the
//! largest, `SetPrecision` drops them. Literals and text keep exactly the
//! digits written, with p = max(16, digits written) (2.1, 2.8).
//!
//! Operation precision (2.1, 2.6, 14.2): `+`, `-` and `*` run at p =
//! max(evaluation precision, p of both operands); `/` at max(evaluation
//! precision, p of the dividend) with the dividend's guard digits (the
//! divisor's precision does not count). The evaluation precision is 16
//! unless `SetPrecision` raised it; callers pass it in. The arithmetic
//! itself is section 14's ([`fixpt`]): products formed row by row over the
//! right operand, opposite-sign sums of operands equal at the display
//! precision exactly 0, Newton division by divisors of more than nine
//! significant digits.

mod consts;
mod finance;
mod fixpt;
mod format;
pub(crate) mod mag;
mod parse;
mod polish;
mod stats;
mod transcend;

use crate::error::CalcError;
use fixpt::Fx;
use std::cmp::Ordering;

pub use polish::PolishError;

/// The default display precision (`kDefltFixedPrecision`).
pub const DEFAULT_PRECISION: u16 = 16;
/// The largest precision `SetPrecision` accepts (`kMaxUserPrecision`).
pub const MAX_USER_PRECISION: u16 = 400;
/// The largest precision a number can carry (`FixPt::SetPrecision` clamps to
/// [16, 800]); only literals and text with that many digits reach it.
pub const MAX_PRECISION: u16 = 800;
/// Guard digits of literals, text and ordinary results.
pub(crate) const GUARD: u32 = 2;
const GUARD_DIGITS: u16 = GUARD as u16;
/// Results of `Average` and the variance functions with more integer digits
/// than this become `?` without an error (the operators' own limit is on
/// their operands, section 14.2).
const MAX_INTEGER_DIGITS: u32 = 1201;

/// Clamps a `SetPrecision` argument to 16..=400 (spec 2.6).
pub fn clamp_user_precision(n: i64) -> u16 {
    n.clamp(i64::from(DEFAULT_PRECISION), i64::from(MAX_USER_PRECISION)) as u16
}

/// A FileMaker number.
#[derive(Clone, Debug)]
pub struct Number(Repr);

/// [`Number::eq_key`]: the sign, the decimal digits and the scale of the
/// rounded value, `None` for `?`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NumberKey(Option<(bool, String, u32)>);

#[derive(Clone, Debug)]
enum Repr {
    Empty,
    NaN,
    Fin(Fin),
}

/// A finite value: `coef / 10^scale`, sign separate, never negative zero;
/// display precision and guard digits.
#[derive(Clone, Debug)]
pub(crate) struct Fin {
    neg: bool,
    coef: Vec<u32>,
    scale: u32,
    precision: u16,
    guard: u16,
}

impl Fin {
    /// A value with the ordinary two guard digits.
    pub(crate) fn new(neg: bool, coef: Vec<u32>, scale: u32, precision: u16) -> Fin {
        let coef = mag::trim(coef);
        Fin {
            neg: neg && !coef.is_empty(),
            coef,
            scale,
            precision,
            guard: GUARD_DIGITS,
        }
    }

    /// Another value with this one's precision and guard digits.
    fn derived(&self, neg: bool, coef: Vec<u32>, scale: u32) -> Fin {
        Fin {
            guard: self.guard,
            ..Fin::new(neg, coef, scale, self.precision)
        }
    }

    fn with_guard(mut self, guard: u16) -> Fin {
        self.guard = guard;
        self
    }

    /// The word form of section 14: ceil((p + g) / 9) fraction words, digits
    /// beyond them truncated.
    fn to_fx(&self) -> Fx {
        let mut v = Fx::int_at(0, self.precision, self.guard);
        let scale = 9 * v.f as u32;
        v.m = if self.scale <= scale {
            mag::shl_digits(&self.coef, scale - self.scale)
        } else {
            mag::shr_digits(&self.coef, self.scale - scale).0
        };
        v.neg = self.neg && !v.m.is_empty();
        v
    }

    /// The word form at a display precision of at least `precision`, as the
    /// algorithms see an argument evaluated at that precision (more words
    /// are zero).
    fn to_fx_at(&self, precision: u16) -> Fx {
        let mut v = self.to_fx();
        if v.disp < precision {
            v.set_disp(precision);
        }
        v
    }

    fn from_fx(v: Fx) -> Fin {
        Fin {
            guard: v.extra,
            ..Fin::new(
                v.neg,
                v.m,
                9 * v.f as u32,
                fixpt::clamp_p(i64::from(v.disp)),
            )
        }
    }

    pub(crate) fn zero(precision: u16) -> Fin {
        Fin::new(false, Vec::new(), 0, precision)
    }

    pub(crate) fn from_i64(n: i64, precision: u16) -> Fin {
        Fin::new(n < 0, mag::from_u64(n.unsigned_abs()), 0, precision)
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.coef.is_empty()
    }

    /// The coefficient at a scale of at least the current one.
    fn coef_at(&self, scale: u32) -> Vec<u32> {
        debug_assert!(scale >= self.scale);
        mag::shl_digits(&self.coef, scale - self.scale)
    }

    /// Number of digits left of the point (0 when |x| < 1).
    pub(crate) fn integer_digits(&self) -> u32 {
        mag::digit_count(&self.coef).saturating_sub(self.scale)
    }

    /// Drops fractional digits beyond `digits` (toward zero).
    pub(crate) fn truncated(&self, digits: u32) -> Fin {
        if self.scale <= digits {
            return self.clone();
        }
        let (coef, _, _) = mag::shr_digits(&self.coef, self.scale - digits);
        self.derived(self.neg, coef, digits)
    }

    /// Rounds half away from zero to `digits` fractional digits; negative
    /// `digits` round left of the point (`Round(1250;-2)` = 1300).
    pub(crate) fn rounded(&self, digits: i64) -> Fin {
        if digits >= i64::from(self.scale) {
            return self.clone();
        }
        let drop = (i64::from(self.scale) - digits).min(i64::from(u32::MAX)) as u32;
        let (mut coef, first, _) = mag::shr_digits(&self.coef, drop);
        if first >= 5 {
            coef = mag::add(&coef, &[1]);
        }
        if digits >= 0 {
            self.derived(self.neg, coef, digits as u32)
        } else {
            let left = (-digits).min(i64::from(u32::MAX)) as u32;
            self.derived(self.neg, mag::shl_digits(&coef, left), 0)
        }
    }

    /// Truncates toward zero to `digits` fractional digits, negative `digits`
    /// left of the point.
    fn truncated_signed(&self, digits: i64) -> Fin {
        if digits >= i64::from(self.scale) {
            return self.clone();
        }
        if digits >= 0 {
            return self.truncated(digits as u32);
        }
        let drop = (i64::from(self.scale) - digits).min(i64::from(u32::MAX)) as u32;
        let (coef, _, _) = mag::shr_digits(&self.coef, drop);
        let left = (-digits).min(i64::from(u32::MAX)) as u32;
        self.derived(self.neg, mag::shl_digits(&coef, left), 0)
    }

    pub(crate) fn negated(&self) -> Fin {
        self.derived(!self.neg, self.coef.clone(), self.scale)
    }

    pub(crate) fn abs(&self) -> Fin {
        self.derived(false, self.coef.clone(), self.scale)
    }

    fn with_precision(mut self, precision: u16) -> Fin {
        self.precision = precision;
        self
    }

    /// Magnitude comparison.
    fn cmp_mag(&self, other: &Fin) -> Ordering {
        let scale = self.scale.max(other.scale);
        mag::cmp(&self.coef_at(scale), &other.coef_at(scale))
    }

    /// Signed comparison of the exact values.
    pub(crate) fn cmp_exact(&self, other: &Fin) -> Ordering {
        match (self.neg, other.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_mag(other),
            (true, true) => other.cmp_mag(self),
        }
    }

    /// The exact sum.
    pub(crate) fn add_exact(&self, other: &Fin, precision: u16) -> Fin {
        let scale = self.scale.max(other.scale);
        let a = self.coef_at(scale);
        let b = other.coef_at(scale);
        if self.neg == other.neg {
            return Fin::new(self.neg, mag::add(&a, &b), scale, precision);
        }
        match mag::cmp(&a, &b) {
            Ordering::Equal => Fin::new(false, Vec::new(), scale, precision),
            Ordering::Greater => Fin::new(self.neg, mag::sub(&a, &b), scale, precision),
            Ordering::Less => Fin::new(other.neg, mag::sub(&b, &a), scale, precision),
        }
    }

    /// The exact product.
    pub(crate) fn mul_exact(&self, other: &Fin, precision: u16) -> Fin {
        Fin::new(
            self.neg != other.neg,
            mag::mul(&self.coef, &other.coef),
            self.scale + other.scale,
            precision,
        )
    }

    /// The quotient truncated toward zero to `digits` fractional digits,
    /// plus the next digit and whether anything after it was non-zero.
    fn div_digits(&self, other: &Fin, digits: u32) -> (Vec<u32>, u32) {
        // |a/b| * 10^(digits + 1) = a.coef * 10^(b.scale + digits + 1) / (b.coef * 10^a.scale)
        let num = mag::shl_digits(&self.coef, other.scale + digits + 1);
        let den = mag::shl_digits(&other.coef, self.scale);
        let (q, _) = mag::divmod(&num, &den);
        let (q, last) = mag::divmod_small(&q, 10);
        (q, last)
    }

    /// The quotient rounded half up (on the magnitude) at `digits`.
    pub(crate) fn div_rounded(&self, other: &Fin, digits: u32, precision: u16) -> Fin {
        let (mut q, last) = self.div_digits(other, digits);
        if last >= 5 {
            q = mag::add(&q, &[1]);
        }
        Fin::new(self.neg != other.neg, q, digits, precision)
    }

    /// The quotient truncated toward zero at `digits`.
    pub(crate) fn div_truncated(&self, other: &Fin, digits: u32, precision: u16) -> Fin {
        let (q, _) = self.div_digits(other, digits);
        Fin::new(self.neg != other.neg, q, digits, precision)
    }

    /// Whether the value has no fractional part.
    pub(crate) fn is_integer(&self) -> bool {
        let (_, first, sticky) = mag::shr_digits(&self.coef, self.scale);
        first == 0 && !sticky
    }

    /// Integer part toward zero.
    pub(crate) fn trunc_int(&self) -> Fin {
        self.truncated(0)
    }

    /// The largest integer not above the value.
    pub(crate) fn floor_int(&self) -> Fin {
        let t = self.trunc_int();
        if self.neg && !self.is_integer() {
            t.add_exact(&Fin::from_i64(-1, self.precision), self.precision)
        } else {
            t
        }
    }

    /// The integer value if it fits in an `i64` (fraction truncated).
    pub(crate) fn to_i64_trunc(&self) -> Option<i64> {
        let (int, _, _) = mag::shr_digits(&self.coef, self.scale);
        let m = mag::to_u64(&int)?;
        if self.neg {
            0i64.checked_sub_unsigned(m)
        } else {
            i64::try_from(m).ok()
        }
    }
}

impl Number {
    /// The empty number (`FixPt::mIsEmpty`).
    pub const fn empty() -> Number {
        Number(Repr::Empty)
    }

    /// The `?` result: NaN.
    pub const fn nan() -> Number {
        Number(Repr::NaN)
    }

    /// Zero at the default precision.
    pub fn zero() -> Number {
        Number(Repr::Fin(Fin::zero(DEFAULT_PRECISION)))
    }

    /// An integer at the default precision.
    pub fn from_i64(n: i64) -> Number {
        Number(Repr::Fin(Fin::from_i64(n, DEFAULT_PRECISION)))
    }

    /// 0 or 1, the result of comparisons and logical operators.
    pub fn from_bool(b: bool) -> Number {
        Number::from_i64(i64::from(b))
    }

    pub(crate) fn from_fin(fin: Fin) -> Number {
        Number(Repr::Fin(fin))
    }

    pub(crate) fn fin(&self) -> Option<&Fin> {
        match &self.0 {
            Repr::Fin(f) => Some(f),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.0, Repr::Empty)
    }

    pub fn is_nan(&self) -> bool {
        matches!(self.0, Repr::NaN)
    }

    /// Display precision; 16 for empty and `?`.
    pub fn precision(&self) -> u16 {
        self.fin().map_or(DEFAULT_PRECISION, |f| f.precision)
    }

    /// Whether the value is zero, using every held digit. Empty is not zero.
    pub fn is_zero(&self) -> bool {
        self.fin().is_some_and(Fin::is_zero)
    }

    pub fn is_negative(&self) -> bool {
        self.fin().is_some_and(|f| f.neg)
    }

    /// Truthiness (spec 2.9): not zero using all held digits; `?` is true,
    /// empty is false.
    pub fn is_true(&self) -> bool {
        match &self.0 {
            Repr::Empty => false,
            Repr::NaN => true,
            Repr::Fin(f) => !f.is_zero(),
        }
    }

    /// The value with empty read as 0 (one empty operand of an operator
    /// counts as 0, spec 6.4); `None` for `?`.
    fn operand(&self) -> Option<Fin> {
        match &self.0 {
            Repr::Empty => Some(Fin::zero(DEFAULT_PRECISION)),
            Repr::NaN => None,
            Repr::Fin(f) => Some(f.clone()),
        }
    }

    /// Applies the magnitude limit of `Average` and the variance functions.
    fn checked(fin: Fin) -> Number {
        if fin.integer_digits() > MAX_INTEGER_DIGITS {
            Number::nan()
        } else {
            Number::from_fin(fin)
        }
    }

    fn from_fx(v: Fx) -> Number {
        if v.nan {
            Number::nan()
        } else {
            Number::from_fin(Fin::from_fx(v))
        }
    }

    /// The word form of a function argument evaluated at `precision`: empty
    /// is 0, `?` is `None`.
    fn fx_operand(&self, precision: u16) -> Option<Fx> {
        self.operand().map(|f| f.to_fx_at(precision))
    }

    /// `self + other` (spec 14.2): the exact sum at the larger precision
    /// and guard digits; operands of opposite signs that are equal at the
    /// larger display precision give exactly 0. Empty counts as 0; `?`
    /// stays `?`.
    pub fn add(&self, other: &Number, precision: u16) -> Number {
        let (Some(mut a), Some(b)) = (self.fx_operand(precision), other.fx_operand(precision))
        else {
            return Number::nan();
        };
        fixpt::add_into(&mut a, &b);
        Number::from_fx(a)
    }

    /// `self - other`, by the rule of `+`.
    pub fn sub(&self, other: &Number, precision: u16) -> Number {
        let (Some(a), Some(b)) = (self.fx_operand(precision), other.fx_operand(precision)) else {
            return Number::nan();
        };
        Number::from_fx(fixpt::sub3(&a, &b, &Fx::int_at(0, precision, 2)))
    }

    /// `self * other` (spec 14.2): each word of `other` times all of `self`
    /// is formed exactly, cut after the result's last fraction word, and
    /// the rows summed and truncated to p + g digits, so the last digit
    /// depends on the operand order (`(1/3)*(2/3)*10^18` is
    /// `222222222222222221`, `(2/3)*(1/3)*10^18` `…222`). An operand of
    /// 10^801 or more gives `?` without an error.
    pub fn mul(&self, other: &Number, precision: u16) -> Number {
        let (Some(a), Some(b)) = (self.fx_operand(precision), other.fx_operand(precision)) else {
            return Number::nan();
        };
        Number::from_fx(fixpt::mul(&a, &b, &Fx::int_at(0, precision, 2)))
    }

    /// `self / other` (spec 14.2) at max(evaluation precision, the
    /// dividend's) with the dividend's guard digits: rounded half up at the
    /// held digits, through Newton division for divisors of more than nine
    /// significant digits. Error 15 when `other` is zero or empty, 14 for an
    /// operand of 10^801 or more.
    pub fn div(&self, other: &Number, precision: u16) -> Result<Number, CalcError> {
        let (Some(a), Some(b)) = (self.operand(), other.operand()) else {
            return Ok(Number::nan());
        };
        quotient(&a, &b, precision).map(Number::from_fx)
    }

    /// Unary minus; `-0` is 0. Empty stays empty.
    pub fn neg(&self) -> Number {
        match &self.0 {
            Repr::Fin(f) => Number::from_fin(f.negated()),
            _ => self.clone(),
        }
    }

    /// `Abs`. Empty stays empty.
    pub fn abs(&self) -> Number {
        self.map_fin(Fin::abs)
    }

    /// `Sign`: -1, 0 or 1, guard digits kept. Empty stays empty.
    pub fn sign(&self) -> Number {
        self.map_fin(|f| {
            let s = if f.is_zero() {
                0
            } else if f.neg {
                -1
            } else {
                1
            };
            Fin::from_i64(s, f.precision).with_guard(f.guard)
        })
    }

    fn map_fin(&self, op: impl FnOnce(&Fin) -> Fin) -> Number {
        match &self.0 {
            Repr::Fin(f) => Number::from_fin(op(f)),
            _ => self.clone(),
        }
    }

    /// `Round(x; digits)`: half away from zero at `digits` (negative rounds
    /// left of the point), precision kept.
    pub fn round(&self, digits: i64) -> Number {
        self.map_fin(|f| f.rounded(digits))
    }

    /// `Truncate(x; digits)`: rounds to the display precision first
    /// (`RemoveExtraPrecision`), then truncates toward zero; the guard
    /// digits count stays (`Truncate(Exp(1);16)/3*10^18` holds 20 digits).
    pub fn truncate(&self, digits: i64) -> Number {
        self.map_fin(|f| f.rounded(i64::from(f.precision)).truncated_signed(digits))
    }

    /// `Int`: toward zero, after rounding to the display precision
    /// (`Int((1/3)*3)` = 1; oracle answers), guard digits kept.
    pub fn int(&self) -> Number {
        self.map_fin(|f| f.rounded(i64::from(f.precision)).trunc_int())
    }

    /// `Floor`, after rounding to the display precision; the result has
    /// the ordinary two guard digits (`Floor(Exp(1))/3*10^18` =
    /// 666666666666666667).
    pub fn floor(&self) -> Number {
        self.map_fin(|f| {
            f.rounded(i64::from(f.precision))
                .floor_int()
                .with_guard(GUARD_DIGITS)
        })
    }

    /// `Ceiling`, after rounding to the display precision, two guard
    /// digits.
    pub fn ceiling(&self) -> Number {
        self.map_fin(|f| {
            f.rounded(i64::from(f.precision))
                .negated()
                .floor_int()
                .negated()
                .with_guard(GUARD_DIGITS)
        })
    }

    /// `Mod(x; y)` = x - y * floor(x / y), sign of the divisor; `Mod(x; 0)`
    /// is x (spec 2.3, 14.2). The quotient is that of `/`, floored on its
    /// held digits; the product is exact and the difference follows `-`:
    /// so `Mod(-1;1/3)` floors -3.000000000000000003 to -4, a quotient that
    /// rounds to 0 leaves x (`Mod(-1;9*10^18)` = -1; oracle answers), and
    /// a remainder of operands equal at the display precision is 0
    /// (`Mod(2/3;1/3)` = 0 although the held digits leave 10^-18). Guard
    /// digits are kept. A `?` quotient (an operand of 10^801 or more) gives
    /// `?`.
    pub fn modulo(&self, other: &Number, precision: u16) -> Number {
        let (Some(a), Some(b)) = (self.operand(), other.operand()) else {
            return Number::nan();
        };
        let p = precision.max(a.precision).max(b.precision);
        if b.is_zero() {
            return Number::from_fin(a.with_precision(p));
        }
        let Ok(q) = quotient(&a, &b, precision) else {
            return Number::nan();
        };
        let q = Fin::from_fx(q).floor_int().to_fx();
        let out = Fx::int_at(0, p, 2);
        let product = fixpt::mul(&b.to_fx(), &q, &out);
        Number::from_fx(fixpt::sub3(&a.to_fx_at(p), &product, &out))
    }

    /// `Div(x; y)`: `Floor` of the quotient after rounding it to the display
    /// precision (`Div(-1;1/3)` = -3 where `Mod` floors -3.000000000000000003
    /// to -4); error 15 when y is zero.
    pub fn div_int(&self, other: &Number, precision: u16) -> Result<Number, CalcError> {
        let q = self.div(other, precision)?;
        Ok(q.floor())
    }

    /// `self / other` rounded half up at `precision` + 2 digits, with the
    /// result at exactly `precision`: the dividend's own precision does not
    /// count, as in `Average` and the variance functions
    /// (`Average(0.1234567890123456789;1)` = `.5617283945061728`; oracle
    /// answers in calc-functions.md 9). Error 15 when `other` is zero or
    /// empty.
    pub fn div_at(&self, other: &Number, precision: u16) -> Result<Number, CalcError> {
        let (Some(a), Some(b)) = (self.operand(), other.operand()) else {
            return Ok(Number::nan());
        };
        if b.is_zero() {
            return Err(CalcError::DIVIDE_BY_ZERO);
        }
        Ok(Number::checked(a.div_rounded(
            &b,
            u32::from(precision) + GUARD,
            precision,
        )))
    }

    /// Rounds to `precision` digits as `SetPrecision(expr; n)` does to its
    /// result (spec 2.6); the number's own precision is unchanged and its
    /// guard digits are the ordinary two again (`SetPrecision(Exp(1);16)/3`
    /// holds 18 digits, spec 14.1).
    pub fn round_to_precision(&self, precision: u16) -> Number {
        self.map_fin(|f| f.rounded(i64::from(precision)).with_guard(GUARD_DIGITS))
    }

    /// Integer value truncated toward zero, as counts, positions and indexes
    /// take it (`FixPt::AsLongLong`, calc-functions.md 3.1); empty is 0,
    /// `?` and values beyond `i64` are `None`.
    pub fn to_i64(&self) -> Option<i64> {
        match &self.0 {
            Repr::Empty => Some(0),
            Repr::NaN => None,
            Repr::Fin(f) => f.to_i64_trunc(),
        }
    }

    /// The value as a double from every digit held, guard digits included
    /// (what JSON numbers are made of: `10^-5/7` shows `.0000014285714286`
    /// but holds `.000001428571428571`); `None` for empty and `?`, infinite
    /// beyond the double range.
    pub fn to_f64(&self) -> Option<f64> {
        let f = self.fin()?;
        if f.is_zero() {
            return Some(0.0);
        }
        let sign = if f.neg { "-" } else { "" };
        let digits = mag::to_digits(&f.coef);
        format!("{sign}{digits}e-{}", f.scale).parse().ok()
    }

    /// `FixPt::AsLongLong` for any value (calc-functions.md 3.1 and the
    /// oracle answers in section 9): the integer part toward zero built
    /// from the three lowest base-10^9 words with wrapping 64-bit
    /// arithmetic, the sign applied afterwards. Higher words are ignored and
    /// overflow wraps: 2^64 + 1 gives 1, 10^20 gives 7766279631452241920 and
    /// 10^30 gives 0. Empty and `?` give 0.
    pub fn as_long_long(&self) -> i64 {
        let Some(f) = self.fin() else {
            return 0;
        };
        let (int, _, _) = mag::shr_digits(&f.coef, f.scale);
        let magnitude = int.iter().take(3).rev().fold(0u64, |acc, &w| {
            acc.wrapping_mul(mag::BASE).wrapping_add(u64::from(w))
        }) as i64;
        if f.neg {
            magnitude.wrapping_neg()
        } else {
            magnitude
        }
    }

    /// The comparison of `=` and `<` between numbers: both rounded to the
    /// larger display precision (spec 2.5). Empty compares as 0; `?` equals
    /// `?` and has no order against a number (`None`).
    pub fn compare(&self, other: &Number) -> Option<Ordering> {
        match (&self.0, &other.0) {
            (Repr::NaN, Repr::NaN) => Some(Ordering::Equal),
            (Repr::NaN, _) | (_, Repr::NaN) => None,
            _ => {
                let a = self.operand()?;
                let b = other.operand()?;
                let p = i64::from(a.precision.max(b.precision));
                Some(a.rounded(p).cmp_exact(&b.rounded(p)))
            }
        }
    }

    /// What `=` compares between numbers of one display precision
    /// ([`Self::compare`]), built once per number: the value rounded to its
    /// precision, without trailing zeros. Two numbers with the same
    /// [`Self::precision`] are equal exactly when their keys are; numbers
    /// of different precisions are compared rounded to the larger one, which
    /// these keys do not show. `?` has a key of its own, empty that of 0.
    pub fn eq_key(&self) -> NumberKey {
        let Some(fin) = self.operand() else {
            return NumberKey(None);
        };
        let fin = fin.rounded(i64::from(fin.precision));
        if fin.is_zero() {
            return NumberKey(Some((false, String::new(), 0)));
        }
        let mut digits = mag::to_digits(&fin.coef);
        let mut scale = fin.scale;
        while scale > 0 && digits.ends_with('0') {
            digits.pop();
            scale -= 1;
        }
        NumberKey(Some((fin.neg, digits, scale)))
    }

    /// The value times 10^`digits`, truncated toward zero; empty is 0, `?`
    /// and values beyond `i128` are `None`.
    pub(crate) fn to_scaled_i128(&self, digits: u32) -> Option<i128> {
        let fin = match &self.0 {
            Repr::Empty => return Some(0),
            Repr::NaN => return None,
            Repr::Fin(f) => f,
        };
        let scaled = if fin.scale >= digits {
            mag::shr_digits(&fin.coef, fin.scale - digits).0
        } else {
            mag::shl_digits(&fin.coef, digits - fin.scale)
        };
        let mut n: i128 = 0;
        for &w in scaled.iter().rev() {
            n = n
                .checked_mul(mag::BASE as i128)?
                .checked_add(i128::from(w))?;
        }
        Some(if fin.neg { -n } else { n })
    }

    /// `value / 10^digits` at the default precision.
    pub(crate) fn from_scaled_i128(value: i128, digits: u32) -> Number {
        let mut coef = Vec::new();
        let mut m = value.unsigned_abs();
        while m > 0 {
            coef.push((m % u128::from(mag::BASE as u32)) as u32);
            m /= u128::from(mag::BASE as u32);
        }
        Number::from_fin(Fin::new(value < 0, coef, digits, DEFAULT_PRECISION))
    }
}

/// The quotient of `/`: at max(evaluation precision, the dividend's), with
/// the dividend's guard digits.
fn quotient(a: &Fin, b: &Fin, precision: u16) -> Result<Fx, CalcError> {
    let out = Fx::int_at(0, precision.max(a.precision), a.guard.max(GUARD_DIGITS));
    fixpt::div(&a.to_fx(), &b.to_fx(), &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Number {
        match s.strip_prefix('-') {
            Some(rest) => Number::from_literal(rest, '.').unwrap().neg(),
            None => Number::from_literal(s, '.').unwrap(),
        }
    }

    fn text(n: &Number) -> String {
        n.to_text_with('.')
    }

    #[test]
    fn row_product_matches_the_spec_examples() {
        let e18 = lit("1000000000000000000");
        let third = lit("1").div(&lit("3"), 16).unwrap();
        let two_thirds = lit("2").div(&lit("3"), 16).unwrap();
        let p = third.mul(&third, 16).mul(&e18, 16);
        assert_eq!(text(&p), "111111111111111109");
        let x = lit("0.333333333333333333");
        let p = x.mul(&x, 16).mul(&e18, 16);
        assert_eq!(text(&p), "111111111111111110.88");
        // The last held digit depends on the operand order.
        let p = third.mul(&two_thirds, 16).mul(&e18, 16);
        assert_eq!(text(&p), "222222222222222221");
        let p = two_thirds.mul(&third, 16).mul(&e18, 16);
        assert_eq!(text(&p), "222222222222222222");
    }

    #[test]
    fn opposite_signs_equal_at_the_display_precision_give_zero() {
        let e20 = lit("100000000000000000000");
        let third = lit("1").div(&lit("3"), 16).unwrap();
        let d = third.sub(&lit(".3333333333333333"), 16).mul(&e20, 16);
        assert!(d.is_zero());
        let d = third.add(&lit("-.3333333333333333"), 16).mul(&e20, 16);
        assert!(d.is_zero());
        let d = third.sub(&lit(".333333333333333"), 16).mul(&e20, 16);
        assert_eq!(text(&d), "33300");
        // `+` compares at the evaluation precision too:
        // `Let(x=1/3;SetPrecision(x + -.3333333333333333;20))`.
        let d = third.add(&lit("-.3333333333333333"), 20);
        assert_eq!(
            text(&d.mul(&lit("10000000000000000000000000"), 20)),
            "330000000"
        );
    }

    #[test]
    fn guard_digits_by_operation() {
        let e18 = lit("1000000000000000000");
        let held = |n: &Number| text(&n.div(&lit("3"), 16).unwrap().mul(&e18, 16));
        let e = lit("1").exp(16).unwrap();
        assert_eq!(text(&e.mul(&e18, 16)), "2718281828459045235.36");
        assert_eq!(held(&e), "906093942819681745.12");
        assert_eq!(held(&e.round_to_precision(16)), "906093942819681733");
        assert_eq!(held(&e.round(16)), "906093942819681733.33");
        assert_eq!(held(&e.truncate(16)), "906093942819681733.33");
        assert_eq!(held(&e.int()), "666666666666666666.67");
        assert_eq!(held(&e.floor()), "666666666666666667");
        assert_eq!(held(&e.sign()), "333333333333333333.33");
        assert_eq!(held(&e.modulo(&lit("1"), 16)), "239427276153015078.45");
        assert_eq!(
            held(&e.div_int(&lit("1"), 16).unwrap()),
            "666666666666666667"
        );
    }

    #[test]
    fn division_rounds_half_up_at_p_plus_two() {
        let q = lit("2").div(&lit("3"), 16).unwrap();
        assert_eq!(text(&q), ".6666666666666667");
        let scaled = q.mul(&lit("1000000000000000000"), 16);
        assert_eq!(text(&scaled), "666666666666666667");
        let neg = lit("-2").div(&lit("3"), 16).unwrap();
        assert_eq!(
            text(&neg.mul(&lit("1000000000000000000"), 16)),
            "-666666666666666667"
        );
        assert_eq!(
            lit("1").div(&lit("0"), 16).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
        // The divisor's precision does not count.
        let q = lit("1").div(&lit("3.0000000000000000000"), 16).unwrap();
        assert_eq!(text(&q), ".3333333333333333");
        let q = lit("1.0000000000000000000").div(&lit("3"), 16).unwrap();
        assert_eq!(text(&q), ".3333333333333333333");
    }

    #[test]
    fn mod_div_round_truncate() {
        let m = |a: &str, b: &str| text(&lit(a).modulo(&lit(b), 16));
        assert_eq!(m("7", "3"), "1");
        assert_eq!(m("-7", "3"), "2");
        assert_eq!(m("7", "-3"), "-2");
        assert_eq!(m("-7", "-3"), "-1");
        assert_eq!(m("-7.5", "2"), ".5");
        assert_eq!(m("1", "0"), "1");
        let third = lit("1").div(&lit("3"), 16).unwrap();
        let two_thirds = lit("2").div(&lit("3"), 16).unwrap();
        assert_eq!(text(&two_thirds.modulo(&third, 16)), "0");
        let e18 = lit("1000000000000000000");
        assert_eq!(
            text(&lit("1").neg().modulo(&third, 16).mul(&e18, 16)),
            "333333333333333332"
        );
        assert_eq!(text(&third.mul(&lit("3"), 16).int()), "1");
        let d = |a: &str, b: &str| text(&lit(a).div_int(&lit(b), 16).unwrap());
        assert_eq!(d("-7", "3"), "-3");
        assert_eq!(d("-7.5", "2"), "-4");
        assert_eq!(text(&lit("-2.5").round(0)), "-3");
        assert_eq!(text(&lit("1250").round(-2)), "1300");
        assert_eq!(text(&lit("-2567").truncate(-2)), "-2500");
        assert_eq!(text(&lit("-2.9").int()), "-2");
        assert_eq!(text(&lit("-1.2").floor()), "-2");
        assert_eq!(text(&lit("-1.2").ceiling()), "-1");
    }

    #[test]
    fn comparison_rounds_to_the_larger_precision() {
        let third = lit("1").div(&lit("3"), 16).unwrap();
        assert_eq!(
            third.compare(&lit("0.33333333333333333")),
            Some(Ordering::Equal)
        );
        assert_eq!(
            third.compare(&lit("0.3333333333333333333")),
            Some(Ordering::Less)
        );
        assert_eq!(Number::nan().compare(&Number::nan()), Some(Ordering::Equal));
        assert_eq!(Number::nan().compare(&lit("1")), None);
        assert_eq!(Number::empty().compare(&lit("0")), Some(Ordering::Equal));
        // `eq_key` is `=` between numbers of one precision.
        let numbers = [
            third.clone(),
            lit("0.33333333333333333"),
            lit("0.3333333333333333333"),
            lit("0.3333333333333333"),
            lit("1.50"),
            lit("1.5"),
            lit("15"),
            lit("150"),
            lit("-0"),
            lit("0"),
            lit("0.000"),
            Number::empty(),
            Number::nan(),
            lit("-2"),
            lit("2"),
            lit("1").div(&lit("3"), 2).unwrap(),
            lit("0.33"),
        ];
        for a in &numbers {
            for b in numbers.iter().filter(|b| b.precision() == a.precision()) {
                let equal = a.compare(b) == Some(Ordering::Equal);
                assert_eq!(equal, a.eq_key() == b.eq_key(), "{a:?} = {b:?}");
            }
        }
    }

    #[test]
    fn operands_of_801_digits_are_out_of_range() {
        let big = lit("1e400");
        let square = big.mul(&big, 16);
        assert_eq!(text(&square.mul(&square, 16)), "1.0E+1600");
        let cube = square.mul(&big, 16);
        assert!(!cube.is_nan());
        assert!(cube.mul(&lit("10"), 16).is_nan());
        assert!(cube.mul(&lit("1"), 16).is_nan());
        assert_eq!(cube.div(&lit("1"), 16).unwrap_err().code(), 14);
        assert_eq!(lit("1").div(&cube, 16).unwrap_err().code(), 14);
        assert!(!cube.add(&cube, 16).is_nan());
        assert!(!square.div(&lit("7"), 16).unwrap().is_nan());
    }
}
