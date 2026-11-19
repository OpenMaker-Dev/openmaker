//! `Draco::FixPt` as the transcendental algorithms see it (spec
//! `docs/re/calc-values.md` 14.1, 14.2): a sign-magnitude decimal in base
//! 10^9 words with F fraction words, a display precision p and g guard
//! digits. F follows the precision (F = ceil((p + g) / 9)); digits after
//! position p + g inside the last word survive a precision change and are
//! only cleared by [`Fx::trim`] and [`Fx::round`]. Arithmetic here follows
//! the spec's rules for which precision a result takes and where digits are
//! truncated or rounded: row-wise multiplication, opposite-sign addition
//! that gives 0 for operands equal at the display precision, correctly
//! rounded division by short divisors and Newton division by wide ones.

use super::consts::Table;
use super::mag;
use crate::error::CalcError;
use std::cmp::Ordering;

/// The word base.
const W: u64 = mag::BASE;
/// The precision range of `FixPt::SetPrecision`.
pub(super) const MIN_P: u16 = 16;
pub(super) const MAX_P: u16 = 800;
/// Operands of `*` and `/` with this many integer words (10^801) are `?`.
const OPERAND_WORDS: usize = 90;

pub(super) fn clamp_p(p: i64) -> u16 {
    p.clamp(i64::from(MIN_P), i64::from(MAX_P)) as u16
}

/// Words holding `digits` fractional digits.
fn words_for(digits: u32) -> usize {
    digits.div_ceil(mag::WORD_DIGITS) as usize
}

/// `v` shifted by `k` words: left (more fraction words) for positive `k`,
/// right (dropping the lowest words) for negative.
fn shift_words(v: &[u32], k: isize) -> Vec<u32> {
    if v.is_empty() {
        return Vec::new();
    }
    if k >= 0 {
        let mut out = vec![0u32; k as usize];
        out.extend_from_slice(v);
        out
    } else {
        v.get(k.unsigned_abs()..)
            .map(<[u32]>::to_vec)
            .unwrap_or_default()
    }
}

/// Clears the lowest `k` digits.
fn clear_low_digits(v: &mut Vec<u32>, k: u32) {
    let words = (k / mag::WORD_DIGITS) as usize;
    for w in v.iter_mut().take(words) {
        *w = 0;
    }
    if let Some(w) = v.get_mut(words) {
        *w -= *w % 10u32.pow(k % mag::WORD_DIGITS);
    }
    *v = mag::trim(std::mem::take(v));
}

/// A number in the word form of section 14.
#[derive(Clone, Debug)]
pub(super) struct Fx {
    pub(super) neg: bool,
    /// The magnitude times 10^(9 f), in words, least significant first,
    /// without high zero words.
    pub(super) m: Vec<u32>,
    /// Fraction words.
    pub(super) f: usize,
    /// Display precision.
    pub(super) disp: u16,
    /// Guard digits.
    pub(super) extra: u16,
    pub(super) nan: bool,
}

impl Fx {
    /// The integer `v` at display precision `disp` with `extra` guard
    /// digits.
    pub(super) fn int_at(v: i64, disp: u16, extra: u16) -> Fx {
        let f = words_for(u32::from(disp) + u32::from(extra));
        Fx {
            neg: v < 0,
            m: shift_words(&mag::from_u64(v.unsigned_abs()), f as isize),
            f,
            disp,
            extra,
            nan: false,
        }
    }

    /// A fresh integer: p 16, g 2.
    pub(super) fn int(v: i64) -> Fx {
        Fx::int_at(v, MIN_P, 2)
    }

    /// A fresh integer of any size from its words (least significant
    /// first).
    pub(super) fn from_words(neg: bool, words: &[u32]) -> Fx {
        let mut v = Fx::int(0);
        v.m = shift_words(&mag::trim(words.to_vec()), v.f as isize);
        v.neg = neg && !v.m.is_empty();
        v
    }

    pub(super) fn nan_at(disp: u16, extra: u16) -> Fx {
        let mut v = Fx::int_at(0, disp, extra);
        v.nan = true;
        v
    }

    /// `FixPt(int, example)`: the integer at the example's precision
    /// (clamped) and guard digits.
    pub(super) fn like(v: i64, example: &Fx) -> Fx {
        let extra = if example.extra > 0 { example.extra } else { 2 };
        Fx::int_at(v, clamp_p(i64::from(example.disp)), extra)
    }

    /// The magnitude: the sign cleared.
    pub(super) fn abs(&self) -> Fx {
        let mut c = self.clone();
        c.neg = false;
        c
    }

    pub(super) fn negated(&self) -> Fx {
        let mut c = self.clone();
        if !c.m.is_empty() {
            c.neg = !c.neg;
        }
        c
    }

    /// Held digits, p + g.
    pub(super) fn prec(&self) -> u32 {
        u32::from(self.disp) + u32::from(self.extra)
    }

    /// Changes the fraction word count: fewer words truncate, more are
    /// zero.
    fn set_words(&mut self, f: usize) {
        self.m = shift_words(&self.m, f as isize - self.f as isize);
        self.f = f;
    }

    /// Sets the display precision without clamping; the words follow.
    pub(super) fn set_disp(&mut self, d: u16) {
        self.disp = d;
        self.set_words(words_for(self.prec()));
    }

    /// `SetPrecision`: clamped to [16, 800].
    pub(super) fn set_precision(&mut self, p: i64) {
        self.set_disp(clamp_p(p));
    }

    /// `SetExtraPrecision`, for a positive count.
    pub(super) fn set_extra(&mut self, e: u16) {
        if e > 0 {
            let d = clamp_p(i64::from(self.disp));
            self.extra = e;
            self.set_disp(d);
        }
    }

    /// A routine's working precision: clamp(p, lo, hi) + add, not clamped
    /// to [16, 800].
    pub(super) fn widen(&mut self, lo: u16, hi: u16, add: u16) {
        self.set_disp(self.disp.clamp(lo, hi) + add);
    }

    /// The integer words (least significant first).
    pub(super) fn int_part(&self) -> &[u32] {
        self.m.get(self.f..).unwrap_or(&[])
    }

    /// Integer words in use, at least 1.
    pub(super) fn int_words(&self) -> usize {
        self.int_part().len().max(1)
    }

    /// Integer digits, 0 when |x| < 1.
    pub(super) fn int_digits(&self) -> u32 {
        mag::digit_count(self.int_part())
    }

    pub(super) fn normalize(&mut self) {
        if self.m.is_empty() {
            self.neg = false;
        }
    }

    /// Clears the digits after position p + g in the word that holds it.
    pub(super) fn trim(&mut self) {
        let p = self.prec();
        let last = words_for(p);
        if p == 0 || p % mag::WORD_DIGITS == 0 || last > self.f {
            return;
        }
        let i = self.f - last;
        if let Some(w) = self.m.get_mut(i) {
            *w -= *w % 10u32.pow(mag::WORD_DIGITS - p % mag::WORD_DIGITS);
            self.m = mag::trim(std::mem::take(&mut self.m));
        }
    }

    /// Rounds half away from zero at `n` fractional digits when the words
    /// hold digits beyond them, clears those, then trims.
    pub(super) fn round(&mut self, n: u32) {
        if self.nan {
            return;
        }
        let scale = 9 * self.f as u32;
        if n < scale {
            let k = scale - n;
            self.m = mag::add(&self.m, &mag::shl_digits(&[5], k - 1));
            clear_low_digits(&mut self.m, k);
        }
        self.normalize();
        self.trim();
    }

    /// Truncates toward zero at `n` fractional digits.
    pub(super) fn truncate(&mut self, n: u32) {
        let scale = 9 * self.f as u32;
        if n < scale {
            clear_low_digits(&mut self.m, scale - n);
        }
        self.normalize();
    }

    /// ÷ 10^k inside the existing words (digits leaving them are lost).
    pub(super) fn div10(&mut self, k: u32) {
        self.m = mag::shr_digits(&self.m, k).0;
    }

    /// × 10^k.
    pub(super) fn mult10(&mut self, k: u32) {
        self.m = mag::shl_digits(&self.m, k);
    }

    /// `IsZero`: zero in every word above the last fraction word and in the
    /// first (p + g) mod 9 (or 9) digits of the last word.
    pub(super) fn is_zero(&self) -> bool {
        if self.nan {
            return false;
        }
        if self.f == 0 || self.m.len() > 1 {
            return self.m.is_empty();
        }
        let w = self.m.first().copied().unwrap_or(0);
        match self.prec() % mag::WORD_DIGITS {
            0 => w == 0,
            r => w < 10u32.pow(mag::WORD_DIGITS - r),
        }
    }

    /// Position of the first non-zero fractional digit (1-based), 0 for
    /// none.
    pub(super) fn first_frac_digit(&self) -> u32 {
        let frac = mag::trim(self.m.iter().take(self.f).copied().collect());
        if frac.is_empty() {
            return 0;
        }
        9 * self.f as u32 - mag::digit_count(&frac) + 1
    }

    /// Whether any fraction word is non-zero.
    pub(super) fn has_fraction(&self) -> bool {
        self.m.iter().take(self.f).any(|&w| w != 0)
    }

    /// `FixPt::operator=(FixPt)`: value and precision, the display precision
    /// clamped.
    fn assign(&mut self, other: &Fx) {
        *self = other.clone();
        self.set_disp(clamp_p(i64::from(other.disp)));
    }

    /// The magnitude aligned to `f` fraction words (truncated when fewer).
    fn aligned(&self, f: usize) -> Vec<u32> {
        shift_words(&self.m, f as isize - self.f as isize)
    }

    /// A table constant at the precision of `like`: every one of its
    /// fraction words filled from the table, so 9 F digits, truncated.
    pub(super) fn table(t: &Table, like: &Fx) -> Fx {
        let mut v = Fx::like(0, like);
        let keep = 9 * v.f as u32;
        v.m = if keep <= t.digits {
            mag::shr_digits(&t.coef, t.digits - keep).0
        } else {
            mag::shl_digits(&t.coef, keep - t.digits)
        };
        v
    }

    /// A constant truncated to `digits` = 9 `f` digits in `f` words, at p
    /// `digits` and g 2 (`ExpComputer`'s 45-digit copies).
    pub(super) fn fixed(t: &Table, digits: u32) -> Fx {
        Fx {
            neg: false,
            m: mag::shr_digits(&t.coef, t.digits - digits).0,
            f: words_for(digits),
            disp: digits as u16,
            extra: 2,
            nan: false,
        }
    }
}

// Comparison (14.2): different signs by sign, otherwise both rounded half
// away at the larger display precision.

/// |a| rounded at `d` digits, as an integer times 10^d.
fn rounded_mag(a: &Fx, d: u32) -> Vec<u32> {
    let scale = 9 * a.f as u32;
    if d >= scale {
        return mag::shl_digits(&a.m, d - scale);
    }
    let (q, first, _) = mag::shr_digits(&a.m, scale - d);
    if first >= 5 {
        mag::add(&q, &[1])
    } else {
        q
    }
}

pub(super) fn cmp(a: &Fx, b: &Fx) -> Ordering {
    if a.neg != b.neg {
        return if a.neg {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let d = u32::from(clamp_p(i64::from(a.disp.max(b.disp))));
    let c = mag::cmp(&rounded_mag(a, d), &rounded_mag(b, d));
    if a.neg {
        c.reverse()
    } else {
        c
    }
}

pub(super) fn lt(a: &Fx, b: &Fx) -> bool {
    !a.nan && !b.nan && cmp(a, b) == Ordering::Less
}

pub(super) fn eq(a: &Fx, b: &Fx) -> bool {
    !a.nan && !b.nan && a.neg == b.neg && cmp(a, b) == Ordering::Equal
}

pub(super) fn le(a: &Fx, b: &Fx) -> bool {
    lt(a, b) || eq(a, b)
}

// Addition (14.2).

/// Raises `this` to the larger guard digits and display precision.
fn widen_to(this: &mut Fx, that: &Fx) {
    let e = this.extra.max(that.extra);
    let d0 = this.disp;
    if e > 0 {
        this.extra = e;
        this.set_disp(clamp_p(i64::from(d0)));
    }
    let d = clamp_p(i64::from(this.disp.max(that.disp).max(d0)));
    this.set_disp(d);
}

/// `this += that`. Same signs: the exact sum, not trimmed. Different signs:
/// the magnitudes compared at the larger display precision; equal there
/// gives 0, otherwise the exact difference with the larger's sign, trimmed.
pub(super) fn add_into(this: &mut Fx, that: &Fx) {
    if this.nan || that.nan {
        this.nan = true;
        return;
    }
    if this.neg == that.neg {
        widen_to(this, that);
        this.m = mag::add(&this.m, &that.aligned(this.f));
        return;
    }
    match cmp(&this.abs(), &that.abs()) {
        Ordering::Greater => {
            widen_to(this, that);
            this.m = mag::sub(&this.m, &that.aligned(this.f));
        }
        Ordering::Equal => {
            this.m.clear();
            this.neg = false;
        }
        Ordering::Less => {
            let old = this.clone();
            this.assign(that);
            widen_to(this, &old);
            this.m = mag::sub(&this.m, &old.aligned(this.f));
        }
    }
    this.normalize();
    this.trim();
}

/// `FixPt::Add(a, b, out)`: a copy of a, then `+= b`.
pub(super) fn add3(a: &Fx, b: &Fx) -> Fx {
    let mut r = a.clone();
    add_into(&mut r, b);
    r
}

/// `FixPt::Subtract(a, b, out)`: out = -b at the larger precision of a, b
/// and out's previous display precision, then `+= a`.
pub(super) fn sub3(a: &Fx, b: &Fx, out: &Fx) -> Fx {
    let prev = out.disp;
    let mut o = b.clone();
    let e = a.extra.max(b.extra);
    if e > 0 {
        o.extra = e;
        o.set_disp(clamp_p(i64::from(o.disp)));
    }
    o.set_disp(clamp_p(i64::from(a.disp.max(b.disp).max(o.disp))));
    if o.disp < prev {
        o.set_disp(clamp_p(i64::from(prev)));
    }
    if !o.m.is_empty() {
        o.neg = !o.neg;
    }
    add_into(&mut o, a);
    o
}

// Multiplication (14.2).

/// `a × b` into a number of `out`'s precision: g = max of the three, p the
/// clamped max of the three; each word of b times all of a is formed
/// exactly and its words after the result's last fraction word dropped, the
/// rows summed and the sum trimmed. An operand of 90 or more integer words
/// gives `?`.
pub(super) fn mul(a: &Fx, b: &Fx, out: &Fx) -> Fx {
    if a.nan || b.nan || a.int_words() >= OPERAND_WORDS || b.int_words() >= OPERAND_WORDS {
        return Fx::nan_at(out.disp, out.extra);
    }
    let mut o = Fx::int_at(0, out.disp, out.extra);
    let e = a.extra.max(b.extra).max(out.extra);
    if e > 0 {
        o.extra = e;
        o.set_disp(clamp_p(i64::from(out.disp)));
    }
    o.set_disp(clamp_p(i64::from(a.disp.max(b.disp).max(out.disp))));
    let big_f = o.f as isize;
    let mut total = vec![0u64; a.m.len() + b.m.len() + big_f as usize + 2];
    let mut row = Vec::with_capacity(a.m.len() + 1);
    for (j, &wj) in b.m.iter().enumerate() {
        if wj == 0 {
            continue;
        }
        // The row's lowest word lands at word `sh` of the result.
        let sh = big_f - a.f as isize + j as isize - b.f as isize;
        row.clear();
        let mut carry = 0u64;
        for &x in &a.m {
            let t = u64::from(x) * u64::from(wj) + carry;
            row.push(t % W);
            carry = t / W;
        }
        if carry > 0 {
            row.push(carry);
        }
        let skip = if sh < 0 { sh.unsigned_abs() } else { 0 };
        let start = sh.max(0) as usize;
        for (i, &w) in row.iter().enumerate().skip(skip) {
            total[start + i - skip] += w;
        }
    }
    // Carries (each slot holds at most a few rows' words, far below 2^64).
    let mut carry = 0u64;
    let mut m = Vec::with_capacity(total.len());
    for t in total {
        let s = t + carry;
        m.push((s % W) as u32);
        carry = s / W;
    }
    while carry > 0 {
        m.push((carry % W) as u32);
        carry /= W;
    }
    o.m = mag::trim(m);
    o.neg = a.neg != b.neg;
    o.normalize();
    o.trim();
    o
}

// Division (14.2).

/// The truncated quotient |x| / (n × 10^(9 shift)) in `out`'s words
/// widened (12, 796, 4), rounded half up at out's held digits, display
/// precision restored.
fn divide_to(x: &Fx, n: u32, out: &Fx, shift: isize) -> Fx {
    let p0 = out.disp;
    let mut o = Fx::int_at(0, p0, out.extra);
    o.widen(12, 796, 4);
    let num = shift_words(&x.m, o.f as isize - x.f as isize - shift);
    o.m = mag::divmod_small(&num, n).0;
    o.round(u32::from(o.extra) + u32::from(p0));
    o.set_disp(clamp_p(i64::from(p0)));
    o
}

/// Division by a machine integer: rounded half away from zero at the
/// result's held digits; `?` with error 15 for 0.
pub(super) fn div_u(x: &Fx, n: u32, out: &Fx) -> Result<Fx, CalcError> {
    if x.nan {
        return Ok(Fx::nan_at(out.disp, out.extra));
    }
    if n == 0 {
        return Err(CalcError::DIVIDE_BY_ZERO);
    }
    let mut o = divide_to(x, n, out, 0);
    o.neg = x.neg;
    o.normalize();
    Ok(o)
}

/// `a / b` into a number of `out`'s precision. Divisors whose non-zero
/// words span one word, or two that fit in nine significant digits, divide
/// correctly rounded at the held digits; others go through Newton
/// division. Error 15 for a zero divisor, 14 for an operand of 90 or more
/// integer words.
pub(super) fn div(a: &Fx, b: &Fx, out: &Fx) -> Result<Fx, CalcError> {
    if a.nan || b.nan {
        return Ok(Fx::nan_at(out.disp, out.extra));
    }
    if a.int_words() >= OPERAND_WORDS || b.int_words() >= OPERAND_WORDS {
        return Err(CalcError::OUT_OF_RANGE);
    }
    // (word position relative to the units word, word), lowest first.
    let nz: Vec<(isize, u32)> =
        b.m.iter()
            .enumerate()
            .filter(|(_, &w)| w != 0)
            .map(|(i, &w)| (i as isize - b.f as isize, w))
            .collect();
    let (Some(&(lo_pos, low)), Some(&(hi_pos, top))) = (nz.first(), nz.last()) else {
        return Err(CalcError::DIVIDE_BY_ZERO);
    };
    if a.is_zero() {
        let mut o = out.clone();
        o.m.clear();
        o.neg = false;
        return Ok(o);
    }
    let sign = |mut o: Fx| {
        o.neg = a.neg != b.neg;
        o.normalize();
        o
    };
    match hi_pos - lo_pos {
        0 => return Ok(sign(divide_to(a, top, out, hi_pos))),
        1 => {
            for k in 1..9u32 {
                let room = 10u32.pow(9 - k);
                if top < room && low % room == 0 {
                    let d = top * 10u32.pow(k) + low / room;
                    let saved = out.disp;
                    let mut o = out.clone();
                    o.set_disp(clamp_p(i64::from(saved) + i64::from(k)));
                    let mut o = divide_to(a, d, &o, hi_pos);
                    o.mult10(k);
                    o.round(u32::from(o.extra) + u32::from(saved));
                    o.set_disp(clamp_p(i64::from(saved)));
                    return Ok(sign(o));
                }
                if top >= room {
                    break;
                }
            }
        }
        _ => {}
    }
    long_divide(a, b, out)
}

/// Newton division (`LongDivide`): y from 1 / double(b), refined by
/// y += y (1 - b y) until a step is below 10^-(Wd - 4); the quotient a y is
/// rounded at the held digits.
fn long_divide(a: &Fx, b: &Fx, out: &Fx) -> Result<Fx, CalcError> {
    let p = out.disp;
    let mut o = Fx::int_at(0, p, out.extra);
    o.widen(10, 794, 6);
    let mut d = as_float(b);
    let db = b.int_digits();
    let mut lb = 0;
    if db > 200 {
        let mut t = b.clone();
        t.set_disp(clamp_p(i64::from(t.disp) + i64::from(db)));
        t.div10(db);
        d = as_float(&t);
        lb = db;
    }
    let da = a.int_digits();
    let wd = clamp_p(i64::from(da) + i64::from(o.prec()));
    let one = Fx::int_at(1, wd, 2);
    let mut y = Fx::int_at(0, wd, 2);
    assign_double(&mut y, 1.0 / d);
    if lb > 0 {
        y.div10(lb);
    }
    let mut eps = Fx::int_at(1, wd, 2);
    eps.div10(da + o.prec() - 4);
    let mut f8 = Fx::int_at(1, wd, 2);
    let mut b8 = Fx::int_at(0, wd, 2);
    let mut t138 = Fx::int_at(1, wd, 2);
    let mut t1f8 = Fx::int_at(0, wd, 2);
    let mut iters = i64::from(da + o.prec()) + 201;
    let mut converged = false;
    while !converged {
        iters -= 1;
        if iters < 1 {
            break;
        }
        f8 = mul(b, &y, &f8);
        b8 = sub3(&one, &f8, &b8);
        t138 = mul(&y, &b8, &t138);
        let prev = y.clone();
        add_into(&mut y, &t138);
        t1f8 = sub3(&y, &prev, &t1f8);
        t1f8.neg = false;
        converged = lt(&t1f8, &eps);
        if t1f8.int_words() >= 400 {
            return Err(CalcError::OUT_OF_RANGE);
        }
    }
    let mut r = mul(a, &y, &o);
    r.round(u32::from(r.extra) + u32::from(p));
    r.set_disp(clamp_p(i64::from(p)));
    Ok(r)
}

// Doubles (14.8).

/// Decimal to double as `FixPt::AsFloat` converts: the fraction words from
/// the last to the first, each step (d + word) / 1e9; then the integer
/// words from the lowest, each times a running scale 1, 1e9, 1e18, ...
/// (infinite from word 35 on, so a zero word there gives NaN).
pub(super) fn as_float(x: &Fx) -> f64 {
    let mut d = 0.0f64;
    for i in 0..x.f {
        d = (d + f64::from(x.m.get(i).copied().unwrap_or(0))) / 1e9;
    }
    let int = x.int_part();
    let mut scale = 1.0f64;
    for i in 0..int.len().max(1) {
        d += f64::from(int.get(i).copied().unwrap_or(0)) * scale;
        scale *= 1e9;
    }
    if x.neg {
        -d
    } else {
        d
    }
}

/// The digits and decimal exponent of `%.20g`: the value is
/// `digits × 10^exponent`, trailing zeros removed; `None` for NaN and
/// infinities.
fn printf_20g(d: f64) -> Option<(bool, Vec<u32>, i64)> {
    if !d.is_finite() {
        return None;
    }
    let text = format!("{d:.19e}");
    let (mantissa, exponent) = text.split_once('e')?;
    let neg = mantissa.starts_with('-');
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let mut exponent = exponent.parse::<i64>().ok()? - 19;
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
        exponent += 1;
    }
    Some((neg, mag::from_digits(&digits), exponent))
}

/// `FixPt::operator=(double)`: the `%.20g` text parsed into `x`, the
/// display precision rising to the fractional digits written (never
/// falling). NaN and infinities have no digits and give 0.
pub(super) fn assign_double(x: &mut Fx, d: f64) {
    x.nan = false;
    let Some((neg, coef, exponent)) = printf_20g(d) else {
        x.m.clear();
        x.neg = false;
        return;
    };
    let frac = (-exponent).max(0);
    if frac > i64::from(x.disp) {
        x.set_disp(clamp_p(frac));
    }
    let sh = 9 * x.f as i64 + exponent;
    x.m = if sh >= 0 {
        mag::shl_digits(&coef, sh as u32)
    } else {
        mag::shr_digits(&coef, sh.unsigned_abs().min(u64::from(u32::MAX)) as u32).0
    };
    x.neg = neg;
    x.normalize();
    x.trim();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A literal at p = max(16, digits written), g 2.
    fn lit(s: &str) -> Fx {
        let (neg, s) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let p = clamp_p(frac.len() as i64);
        let mut v = Fx::int_at(0, p, 2);
        let digits = format!("{int}{frac}");
        v.m = mag::shl_digits(
            &mag::from_digits(digits.as_bytes()),
            9 * v.f as u32 - frac.len() as u32,
        );
        v.neg = neg && !v.m.is_empty();
        v
    }

    fn held(v: &Fx) -> String {
        let digits = mag::to_digits(&v.m);
        let scale = 9 * v.f;
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (i, f) = padded.split_at(padded.len() - scale);
        let i = if i == "0" { "" } else { i };
        let f = f.trim_end_matches('0');
        format!(
            "{}{i}{}{f}",
            if v.neg { "-" } else { "" },
            if f.is_empty() { "" } else { "." }
        )
    }

    fn quotient(a: &str, b: &str) -> Fx {
        div(&lit(a), &lit(b), &Fx::int(0)).unwrap()
    }

    #[test]
    fn rows_over_the_right_operand() {
        let third = quotient("1", "3");
        let two_thirds = quotient("2", "3");
        let p = |a: &Fx, b: &Fx| held(&mul(a, b, &Fx::int(0)));
        assert_eq!(p(&third, &two_thirds), ".222222222222222221");
        assert_eq!(p(&two_thirds, &third), ".222222222222222222");
        assert_eq!(p(&two_thirds, &quotient("1", "7")), ".095238095238095237");
        assert_eq!(p(&third, &third), ".111111111111111109");
    }

    #[test]
    fn opposite_signs_equal_at_the_display_precision() {
        let third = quotient("1", "3");
        let r = sub3(&third, &lit(".3333333333333333"), &Fx::int(0));
        assert!(r.m.is_empty());
        let r = sub3(&third, &lit(".333333333333333"), &Fx::int(0));
        assert_eq!(held(&r), ".000000000000000333");
    }

    #[test]
    fn division_rules() {
        assert_eq!(held(&quotient("1", "6")), ".166666666666666667");
        assert_eq!(held(&quotient("2", "3")), ".666666666666666667");
        // Two words merged into one divisor, and Newton division.
        assert_eq!(held(&quotient("1", "1.5")), ".666666666666666667");
        assert_eq!(held(&quotient("1", "1.1234567891")), ".890109890920770439");
        assert_eq!(
            div(&lit("1"), &lit("0"), &Fx::int(0)).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
    }

    #[test]
    fn doubles_through_20g() {
        let mut hi = Fx::int(0);
        assign_double(&mut hi, 1.4);
        assert_eq!((held(&hi).as_str(), hi.disp), ("1.3999999999999999112", 19));
        let mut lo = Fx::int(0);
        assign_double(&mut lo, 0.6);
        assert_eq!((held(&lo).as_str(), lo.disp), (".5999999999999999778", 19));
        let mut nan = Fx::int(5);
        assign_double(&mut nan, f64::NAN);
        assert!(nan.m.is_empty());
        assert_eq!(as_float(&lit("2.5")), 2.5);
        let mut big = Fx::int(1);
        big.mult10(324);
        assert!(as_float(&big).is_nan());
    }
}
