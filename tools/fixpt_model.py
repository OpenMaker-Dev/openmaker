#!/usr/bin/env python3
"""Executable model of FileMaker's decimal transcendental functions.

Spec: docs/re/calc-values.md section 14 ("Transcendental algorithms"). The
model is written from that description, not from FileMaker code: every rule
here is one the spec states, and `tools/fixpt_battery.py` checks the model
against FileMaker Server's own engine (tools/fms-calc-harness).

A value is a sign-magnitude decimal held in base 10^9 words: `m` is the
magnitude scaled by 10^(9 f), `f` the fraction words in use, `disp` the
display precision p and `extra` the guard digits (held digits = p + extra).
Operations follow the spec's rules for which precision a result takes, where
digits are truncated and where they are rounded.

Usage:
  python3 -I tools/fixpt_model.py '<function>' <args...> [--p N]
      Evaluate one function at evaluation precision N (default 16) and print
      its display text and held digits, e.g. `exp 100 --p 30`,
      `power 1.5 100`, `fv 50 0.5 360`. Functions: exp ln log lg sqrt power
      fv pv pmt npv (npv takes the rate then the payments).
"""

import sys
from decimal import Decimal, getcontext, ROUND_FLOOR

W = 10 ** 9
MAX_P = 800


def clamp(v, lo, hi):
    return lo if v < lo else hi if v > hi else v


def words_for(digits):
    return (digits + 8) // 9


# --------------------------------------------------------------------------
# Constants (spec 14.1): mathematical constants held to fixed digit counts.

def _constants():
    getcontext().prec = 2100
    ln10 = Decimal(10).ln()
    ln2 = Decimal(2).ln()

    def atan_inv(x):
        x = Decimal(x)
        t = 1 / x
        s = t
        n = 1
        while True:
            t /= -(x * x)
            n += 2
            term = t / n
            if abs(term) < Decimal(10) ** -2000:
                return s
            s += term

    pi = 16 * atan_inv(5) - 4 * atan_inv(239)

    def cut(v, digits, rounded):
        q = Decimal(1).scaleb(-digits)
        mode = 'ROUND_HALF_UP' if rounded else ROUND_FLOOR
        return int((v.quantize(q, rounding=mode)).scaleb(digits))

    # value * 10^digits as an integer, and the digit count
    return {
        'ln10': (cut(ln10, 801, False), 801),
        'ln10inv': (cut(1 / ln10, 801, False), 801),
        'ln2': (cut(ln2, 1953, False), 1953),
        'ln2inv': (cut(1 / ln2, 801, True), 801),
        'pi': (cut(pi, 720, True), 720),
    }


CONST = _constants()


# --------------------------------------------------------------------------
# The value

class Fx:
    __slots__ = ('neg', 'm', 'f', 'disp', 'extra', 'nan')

    def __init__(self, value=0, disp=16, extra=2):
        """An integer value at display precision `disp` with `extra` guard
        digits (FileMaker's default for a fresh number is 16 and 2)."""
        self.nan = False
        self.extra = extra
        self.disp = disp
        self.f = words_for(disp + extra)
        self.neg = value < 0
        self.m = abs(value) * W ** self.f

    def copy(self):
        c = Fx.__new__(Fx)
        c.neg, c.m, c.f, c.disp, c.extra, c.nan = (
            self.neg, self.m, self.f, self.disp, self.extra, self.nan)
        return c

    def assign(self, other):
        """Copy value and precision; the display precision is clamped to
        [16, 800] and the word count follows it."""
        self.nan = other.nan
        self.neg = other.neg
        self.extra = other.extra
        self.m = other.m
        self.f = other.f
        self.set_disp(clamp(other.disp, 16, MAX_P))
        return self

    @staticmethod
    def nan_value(disp=16, extra=2):
        v = Fx(0, disp, extra)
        v.nan = True
        return v

    @staticmethod
    def from_decimal(text, disp=None, extra=2, eval_p=16):
        """A literal: p = max(eval precision, fractional digits written)."""
        d = Decimal(text)
        sign, digits, exp = d.as_tuple()
        nfrac = max(0, -exp)
        p = max(eval_p, nfrac) if disp is None else disp
        v = Fx(0, clamp(p, 16, MAX_P), extra)
        v.neg = bool(sign)
        coef = int(''.join(map(str, digits)) or '0')
        v.m = coef * 10 ** (9 * v.f + exp) if 9 * v.f + exp >= 0 else coef // 10 ** (-(9 * v.f + exp))
        if v.m == 0:
            v.neg = False
        v.trim()
        return v

    # ---- precision -------------------------------------------------------

    @property
    def prec(self):
        return self.disp + self.extra

    def set_words(self, f):
        """Change the fraction word count: dropping words truncates,
        new words are zero. Digits inside the last word are untouched."""
        if f < self.f:
            self.m //= W ** (self.f - f)
        else:
            self.m *= W ** (f - self.f)
        self.f = f

    def set_disp(self, d):
        """Set the display precision without clamping (words follow)."""
        self.disp = d
        self.set_words(words_for(d + self.extra))

    def set_precision(self, p):
        """`SetPrecision(p)`: clamp to [16, 800]; words follow."""
        self.set_disp(clamp(p, 16, MAX_P))

    def set_extra(self, e):
        """`SetExtraPrecision(e)`: only for e > 0."""
        if e > 0:
            d = clamp(self.disp, 16, MAX_P)
            self.extra = e
            self.set_disp(d)

    def widen(self, lo, hi, add):
        """The temporary working precision of a routine:
        clamp(p, lo, hi) + add (no [16, 800] clamp)."""
        self.set_disp(clamp(self.disp, lo, hi) + add)

    # ---- digits ------------------------------------------------------------

    def int_part(self):
        return self.m // W ** self.f

    def int_words(self):
        """Integer words in use (at least 1)."""
        n = self.int_part()
        k = 1
        while n >= W:
            n //= W
            k += 1
        return k

    def int_digits(self):
        n = self.int_part()
        return len(str(n)) if n else 0

    def normalize(self):
        if self.m == 0:
            self.neg = False

    def trim(self):
        """Truncate the digits of the word holding the last held digit
        (position p + extra) after that position."""
        P = self.prec
        last = words_for(P)
        if P <= 0 or P % 9 == 0 or last > self.f:
            return
        shift = W ** (self.f - last)
        w = (self.m // shift) % W
        keep = 10 ** (9 - P % 9)
        self.m -= (w % keep) * shift

    def round(self, n):
        """Round half away from zero at n fractional digits (when the words
        hold digits beyond n), clear the rest, then trim."""
        if self.nan:
            return
        if n < 9 * self.f:
            unit = 10 ** (9 * self.f - n)
            self.m = (self.m + unit // 2) // unit * unit
        self.normalize()
        self.trim()

    def truncate(self, n):
        if n < 9 * self.f:
            unit = 10 ** (9 * self.f - n)
            self.m = self.m // unit * unit
        self.normalize()

    def div10(self, k):
        """Shift right k digits inside the existing words (digits leaving
        the last word are lost)."""
        self.m //= 10 ** k

    def mult10(self, k):
        self.m *= 10 ** k

    def is_zero(self):
        """Zero in the held digits: every word above the last fraction word,
        and the leading (p + extra) mod 9 digits of the last word."""
        if self.nan:
            return False
        if self.f == 0:
            return self.m == 0
        if self.m // W:
            return False
        r = self.prec % 9
        w = self.m % W
        return w == 0 if r == 0 else w < 10 ** (9 - r)

    def first_frac_digit(self):
        """Position of the first non-zero fractional digit (1-based)."""
        frac = self.m % W ** self.f
        if frac == 0:
            return 0
        return 9 * self.f - len(str(frac)) + 1

    def value(self):
        """Exact held value as a Decimal."""
        getcontext().prec = 4000
        v = Decimal(self.m).scaleb(-9 * self.f)
        return -v if self.neg else v

    def __repr__(self):
        if self.nan:
            return 'Fx(nan)'
        return 'Fx(%s p=%d+%d f=%d)' % (self.value(), self.disp, self.extra, self.f)


def table_constant(name, like, whole_and_words=None):
    """A table constant held in a number of the precision of `like`
    (display clamp(p, 16, 800), like's guard digits or 2): the table fills
    every fraction word in use, so it carries 9 * words digits, truncated."""
    v = Fx(0, clamp(like.disp, 16, MAX_P), like.extra if like.extra > 0 else 2)
    coef, digits = CONST[name]
    keep = 9 * v.f
    v.m = coef // 10 ** (digits - keep) if keep <= digits else coef * 10 ** (keep - digits)
    return v


def like(v, like_):
    """FixPt(int, example): an integer at the example's display precision
    (clamped to [16, 800]) and guard digits (2 when the example has none)."""
    return Fx(v, clamp(like_.disp, 16, MAX_P), like_.extra if like_.extra > 0 else 2)


# --------------------------------------------------------------------------
# Comparison (spec 14.2): both sides rounded to the larger display precision.

def _rounded_mag(a, D):
    """|a| rounded half away at D digits, as an integer times 10^D."""
    c = a.copy()
    c.set_disp(D)
    c.round(D)
    return c.m // 10 ** (9 * c.f - D)


def cmp(a, b):
    """-1, 0 or 1 under FileMaker's comparison (NaN handled by callers)."""
    if a.neg != b.neg:
        return -1 if a.neg else 1
    D = clamp(max(a.disp, b.disp), 16, MAX_P)
    ma, mb = _rounded_mag(a, D), _rounded_mag(b, D)
    c = (ma > mb) - (ma < mb)
    return -c if a.neg else c


def lt(a, b):
    if a.nan or b.nan or a is b:
        return False
    return cmp(a, b) < 0


def eq(a, b):
    if a.nan or b.nan:
        return False
    if a.neg != b.neg:
        return False
    return cmp(a, b) == 0


# --------------------------------------------------------------------------
# Addition (spec 14.2)

def _align(a, f):
    return a.m * W ** (f - a.f) if f >= a.f else a.m // W ** (a.f - f)


def _widen_to(this, that):
    """Raise `this` to the larger guard digits and display precision."""
    e = max(this.extra, that.extra)
    d0 = this.disp
    if e > 0:
        this.extra = e
        this.set_disp(clamp(d0, 16, MAX_P))
    d = clamp(max(this.disp, that.disp, d0), 16, MAX_P)
    this.set_disp(d)


def _add_mag(this, that):
    _widen_to(this, that)
    this.m += _align(that, this.f)


def _sub_mag(this, that):
    _widen_to(this, that)
    this.m -= _align(that, this.f)


def add_into(this, that):
    """this += that. Same signs: exact sum, not trimmed. Different signs:
    magnitudes compared at the larger display precision; equal there gives
    0, otherwise the exact difference, trimmed."""
    if this.nan or that.nan:
        this.nan = True
        return this
    if this.neg == that.neg:
        _add_mag(this, that)
        return this
    c = cmp(_abs(this), _abs(that))
    if c > 0:
        _sub_mag(this, that)
    elif c == 0:
        this.m = 0
        this.neg = False
    else:
        old = this.copy()
        this.assign(that)
        _sub_mag(this, old)
    this.normalize()
    this.trim()
    return this


def _abs(a):
    c = a.copy()
    c.neg = False
    return c


def add3(a, b):
    """FixPt::Add(a, b, out): out takes a's precision, then out += b."""
    return add_into(a.copy(), b)


def neg_of(a):
    c = a.copy()
    if c.m:
        c.neg = not c.neg
    return c


def sub3(a, b, out):
    """FixPt::Subtract(a, b, out): out = -b at the larger precision of a, b
    and out's previous display precision, then out += a."""
    prev = out.disp
    o = b.copy()
    e = max(a.extra, b.extra, o.extra)
    if e > 0:
        o.extra = e
        o.set_disp(clamp(o.disp, 16, MAX_P))
    d = clamp(max(a.disp, b.disp, o.disp), 16, MAX_P)
    o.set_disp(d)
    if o.disp < prev:
        o.set_disp(clamp(prev, 16, MAX_P))
    if o.m:
        o.neg = not o.neg
    return add_into(o, a)


def add_int(this, n):
    return add_into(this, Fx(n))


# --------------------------------------------------------------------------
# Multiplication (spec 14.2)

def mul(a, b, out):
    """Product at display max(a, b, out) and guard digits max(a, b, out):
    each row (one word of b times all of a) is formed exactly and cut at the
    result's last fraction word, the rows are summed, the sum trimmed."""
    if a.nan or b.nan or a.int_words() > 89 or b.int_words() > 89:
        return Fx.nan_value(out.disp, out.extra)
    o = Fx(0, out.disp, out.extra)
    e = max(a.extra, b.extra, out.extra)
    if e > 0:
        o.extra = e
        o.set_disp(clamp(out.disp, 16, MAX_P))
    d = clamp(max(a.disp, b.disp, out.disp), 16, MAX_P)
    o.set_disp(d)
    F = o.f
    total = 0
    bm = b.m
    pos = -b.f
    while bm:
        wj = bm % W
        bm //= W
        if wj:
            sh = F - a.f + pos
            row = a.m * wj
            total += row * W ** sh if sh >= 0 else row // W ** (-sh)
        pos += 1
    o.m = total
    o.neg = (a.neg != b.neg)
    o.normalize()
    o.trim()
    return o


# --------------------------------------------------------------------------
# Division (spec 14.2)

def _divide_to(x_mag, x_f, n, out, shift_words=0):
    """Truncated quotient |x| / (n * 10^(9 shift)) in out's widened words
    (clamp(p, 12, 796) + 4), rounded half up at out's held digits, display
    restored."""
    P0 = out.disp
    o = Fx(0, P0, out.extra)
    o.f = out.f
    o.widen(12, 796, 4)
    F = o.f
    sh = F - x_f - shift_words
    num = x_mag * W ** sh if sh >= 0 else x_mag // W ** (-sh)
    o.m = num // n
    o.round(o.extra + P0)
    o.set_disp(clamp(P0, 16, MAX_P))
    return o


def div_u(x, n, out):
    """Division by a machine integer: rounded half away from zero at the
    result's held digits."""
    if x.nan:
        return Fx.nan_value(out.disp, out.extra), 0
    if n == 0:
        return Fx.nan_value(out.disp, out.extra), 15
    o = _divide_to(x.m, x.f, n, out)
    o.neg = x.neg
    o.normalize()
    return o, 0


def as_float(x):
    """Decimal to double as FileMaker converts it: fraction words from the
    last to the first, each step (d + word) / 1e9; then integer words from
    the lowest, each times a running scale 1, 1e9, 1e18, ... (which becomes
    infinite at word 35, so a zero word there gives NaN)."""
    d = 0.0
    frac = x.m % W ** x.f
    words = []
    for _ in range(x.f):
        words.append(frac % W)
        frac //= W
    for w in words:                      # lowest first
        d = (d + w) / 1e9
    n = x.int_part()
    scale = 1.0
    iw = []
    while True:
        iw.append(n % W)
        n //= W
        if not n:
            break
    for w in iw:
        d = d + float(w) * scale
        scale = scale * 1e9
    return -d if x.neg else d


def assign_double(x, d):
    """`%.20g` of the double parsed back into x: the display precision rises
    to the number of fractional digits written (never falls)."""
    s = '%.20g' % d
    dec = Decimal(s)
    sign, digits, exp = dec.as_tuple()
    nfrac = max(0, -exp)
    if nfrac > x.disp:
        x.set_disp(clamp(nfrac, 16, MAX_P))
    coef = int(''.join(map(str, digits)))
    sh = 9 * x.f + exp
    x.m = coef * 10 ** sh if sh >= 0 else coef // 10 ** (-sh)
    x.neg = bool(sign)
    x.nan = False
    x.normalize()
    x.trim()
    return x


def _divisor_words(b):
    """(position, word) of b's non-zero words, lowest first; position 0 is
    the units word, -1 the first fraction word."""
    res = []
    m = b.m
    pos = -b.f
    while m:
        w = m % W
        if w:
            res.append((pos, w))
        m //= W
        pos += 1
    return res


def div(a, b, out):
    """a / b at out's precision. Divisors whose non-zero words span one word,
    or two words that fit in nine significant digits, divide exactly
    (rounded half away at the held digits); others go through long_divide."""
    if a.nan or b.nan:
        return Fx.nan_value(out.disp, out.extra), 0
    if a.int_words() >= 90 or b.int_words() >= 90:
        return Fx.nan_value(out.disp, out.extra), 14
    nz = _divisor_words(b)
    if not nz:
        return Fx.nan_value(out.disp, out.extra), 15
    if a.is_zero():
        o = out.copy()
        o.m = 0
        o.neg = False
        return o, 0
    lo_pos, hi_pos = nz[0][0], nz[-1][0]
    span = hi_pos - lo_pos + 1
    if span == 1:
        o = _divide_to(a.m, a.f, nz[-1][1], out, hi_pos)
        o.neg = a.neg != b.neg
        o.normalize()
        return o, 0
    if span == 2:
        top = nz[-1][1]
        low = nz[0][1]
        for k in range(1, 9):
            if top < 10 ** (9 - k) and low % 10 ** (9 - k) == 0:
                D = top * 10 ** k + low // 10 ** (9 - k)
                saved = out.disp
                o = out.copy()
                o.set_disp(clamp(saved + k, 16, MAX_P))
                o = _divide_to(a.m, a.f, D, o, hi_pos)
                o.mult10(k)
                o.round(o.extra + saved)
                o.set_disp(clamp(saved, 16, MAX_P))
                o.neg = a.neg != b.neg
                o.normalize()
                return o, 0
            if top >= 10 ** (9 - k):
                break
    return long_divide(a, b, out)


def long_divide(a, b, out):
    """Newton reciprocal: y from 1/double(b), refined by y += y (1 - b y)
    at Wd = int digits(a) + clamp(p, 10, 794) + 6 + extra digits until the
    step is below 10^-(Wd - 4); the quotient a y is rounded at the held
    digits."""
    P = out.disp
    o = Fx(0, P, out.extra)
    o.f = out.f
    o.widen(10, 794, 6)
    d = as_float(b)
    Lb = 0
    Db = b.int_digits()
    if Db > 200:
        t = b.copy()
        t.set_disp(clamp(t.disp + Db, 16, MAX_P))
        t.div10(Db)
        d = as_float(t)
        Lb = Db
    Da = a.int_digits()
    Wd = clamp(Da + o.prec, 16, MAX_P)
    one = Fx(1, Wd, 2)
    y = Fx(0, Wd, 2)
    assign_double(y, 1.0 / d)
    if Lb:
        y.div10(Lb)
    eps = Fx(1, Wd, 2)
    eps.div10(Da + o.prec - 4)
    f8 = Fx(1, Wd, 2)
    b8 = Fx(0, Wd, 2)
    t138 = Fx(1, Wd, 2)
    t1f8 = Fx(0, Wd, 2)
    iters = Da + o.prec + 201
    converged = False
    while True:
        if converged:
            break
        iters -= 1
        if iters < 1:
            break
        f8 = mul(b, y, f8)
        b8 = sub3(one, f8, b8)
        t138 = mul(y, b8, t138)
        prev = y.copy()
        add_into(y, t138)
        t1f8 = sub3(y, prev, t1f8)
        t1f8.neg = False
        converged = lt(t1f8, eps)
        if t1f8.int_words() >= 400:
            return Fx.nan_value(P, out.extra), 14
    r = mul(a, y, o)
    r.round(r.extra + P)
    r.set_disp(clamp(P, 16, MAX_P))
    return r, 0


def div_floor(a, b, out):
    """FixPt::Div for the integer exponent loop: floor(a / b)."""
    q = (a.m * W ** b.f) // (b.m * W ** a.f)
    o = Fx(0, out.disp, out.extra)
    o.m = q * W ** o.f
    return o


def split(y):
    """Integer part (toward zero) and the signed fraction, both at y's
    precision."""
    yi = y.copy()
    yi.truncate(0)
    yf = y.copy()
    yf.m = y.m % W ** y.f
    yf.normalize()
    return yi, yf


# --------------------------------------------------------------------------
# Exp (spec 14.3)

def compute_exp(x, rdisp, rextra):
    """e^x into a result of display rdisp and guard digits rextra."""
    if x.nan:
        return Fx.nan_value(rdisp, rextra), 0
    if lt(x, Fx(-5000)) or eq(x, Fx(-5000)):
        r = Fx(0, rdisp, rextra)
        return r, 0
    if not lt(x, Fx(5000)) and not eq(x, Fx(5000)):
        return Fx.nan_value(rdisp, rextra), 14
    neg = x.neg
    result = Fx(1, rdisp, rextra)
    P = rdisp
    X = _abs(x)
    result.widen(12, 796, 4)
    if not x.is_zero():
        inv = Fx(0, 45, 2)
        inv.f = 5
        inv.m = CONST['ln10inv'][0] // 10 ** (CONST['ln10inv'][1] - 45)
        power = mul(inv, X, Fx(0, 16, 2))
        power.round(0)
        k = power.int_part()
        Wp = clamp(result.disp + k, 16, MAX_P)
        result.set_disp(Wp)
        X.set_disp(clamp(Wp, 16, MAX_P))
        X.set_extra(result.extra)
        temp = X.copy()
        ln10 = Fx(0, 45, 2)
        ln10.f = 5
        ln10.m = CONST['ln10'][0] // 10 ** (CONST['ln10'][1] - 45)
        temp2 = mul(power, ln10, Fx(0, Wp, result.extra))
        X = sub3(temp, temp2, Fx(0, Wp, result.extra))
        term = X.copy()
        n = 2
        while True:
            add_into(result, term)
            temp2, _ = div_u(X, n, temp2)
            temp = mul(term, temp2, temp)
            term = temp.copy()
            if term.is_zero():
                break
            n += 1
        result.mult10(k)
    code = 0
    if neg:
        t = temp.copy()
        e = max(t.extra, result.extra)
        d0 = t.disp
        if e > 0:
            t.extra = e
            t.set_disp(clamp(d0, 16, MAX_P))
        d = clamp(max(result.disp, t.disp, d0), 16, MAX_P)
        t.set_disp(d)
        q, code = div(Fx(1), result, t)
        if code == 0:
            result = q.copy()
            result.set_disp(clamp(q.disp, 16, MAX_P))
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, code


# --------------------------------------------------------------------------
# Ln, Log, Lg (spec 14.4)

def compute_ln(x, rdisp, rextra):
    if x.nan:
        return Fx.nan_value(rdisp, rextra), 0
    if x.is_zero() or x.neg:
        return Fx.nan_value(rdisp, rextra), 14
    P = rdisp
    result = Fx(0, rdisp, rextra)
    result.widen(12, 796, 4)
    X = x.copy()
    Q = clamp(result.disp, 16, MAX_P)
    X.set_disp(Q)
    X.set_extra(result.extra)
    f8 = like(0, result)
    shift10 = 0
    if not lt(X, Fx(1)):
        if not lt(X, Fx(2)):
            D = X.int_digits()
            X.div10(D)
            shift10 = D
    else:
        fd = X.first_frac_digit()
        if fd - 1:
            X.mult10(fd - 1)
            shift10 = 1 - fd
    hi = assign_double(Fx(0), 1.4)
    lo = assign_double(Fx(0), 0.6)
    mult2 = 0
    if lt(X, hi) or eq(X, hi):
        if lt(X, lo):
            while True:
                add_into(X, X.copy())
                mult2 -= 1
                if not lt(X, lo):
                    break
    else:
        count = 1
        while True:
            mult2 = count
            q = _divide_to(X.m, X.f, 2, f8)
            q.neg = False
            f8 = q
            X = f8.copy()
            X.set_disp(clamp(X.disp, 16, MAX_P))
            if lt(X, hi) or eq(X, hi):
                break
            count += 1
    add_into(X, Fx(-1))
    numer = X.copy()
    n = 1
    limit = 2 * result.disp + 200
    while True:
        f8, _ = div_u(numer, n, f8)
        if n % 2 == 0:
            add_into(result, neg_of(f8))
        else:
            add_into(result, f8)
        prev = numer.copy()
        numer = mul(prev, X, numer)
        if f8.is_zero():
            break
        n += 1
        limit -= 1
        if limit < 1:
            break
    for count, name in ((shift10, 'ln10'), (mult2, 'ln2')):
        if count:
            c = table_constant(name, result)
            k = Fx(0, numer.disp, numer.extra)
            k.m = abs(count) * W ** k.f
            k.neg = count < 0
            f8 = mul(c, k, f8)
            add_into(result, f8)
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, 0


def compute_log(x, rdisp, rextra, table):
    """Log (table 'ln10inv') and Lg ('ln2inv'): Ln four digits wider, times
    the reciprocal constant cut to whole words."""
    if x.nan:
        return Fx.nan_value(rdisp, rextra), 0
    P = rdisp
    result = Fx(0, rdisp, rextra)
    result.widen(12, 796, 4)
    lnr = like(0, result)
    lnr, code = compute_ln(x, lnr.disp, lnr.extra)
    if code == 0:
        inv = table_constant(table, result)
        result = mul(inv, lnr, result)
    else:
        result = lnr
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, code


# --------------------------------------------------------------------------
# Power (spec 14.5)

def compute_power(x, y, rdisp, rextra):
    if x.nan or y.nan:
        return Fx.nan_value(rdisp, rextra), 0
    if x.int_words() >= 46 or y.int_words() >= 46:
        return Fx.nan_value(rdisp, rextra), 14
    P = rdisp
    result = Fx(0, rdisp, rextra)
    result.widen(6, 790, 10)
    if x.neg and y.m % W ** y.f:
        r = Fx.nan_value(rdisp, rextra)
        return r, 14
    result.m = W ** result.f
    result.neg = False
    yi, yf = split(y)
    code = 0
    if not yi.is_zero():
        two = Fx(2)
        base = x.copy()
        n = _abs(yi)
        base.set_disp(clamp(result.disp, 16, MAX_P))
        base.set_extra(result.extra)
        sq = base.copy()
        acc = result.copy()
        overflow = False
        while True:
            if n.is_zero():
                if y.neg:
                    one = like(1, result)
                    t = like(0, result)
                    q, code = div(one, result, t)
                    if code == 0:
                        result = q.copy()
                        result.set_disp(clamp(q.disp, 16, MAX_P))
                break
            if n.int_part() % 2:
                result = mul(acc, sq, result)
                acc = result.copy()
                if result.int_words() > 45:
                    overflow = True
                    break
            n = div_floor(n, two, like(0, result))
            base = mul(sq, sq, base)
            sq = base.copy()
            if not n.is_zero() and base.int_words() >= 46:
                overflow = True
                break
        if overflow:
            if not y.neg:
                return Fx.nan_value(rdisp, rextra), 14
            r = Fx(0, rdisp, rextra)
            return r, 0
    if not yf.is_zero():
        rc = result.copy()
        if x.is_zero():
            rc.m = 0
            rc.neg = False
        else:
            lnr = like(0, rc)
            lnr, code = compute_ln(x, lnr.disp, lnr.extra)
            if code == 0:
                mr = mul(yf, lnr, like(0, rc))
                rc, code = compute_exp(mr, rc.disp, rc.extra)
        result = mul(result.copy(), rc, result)
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, code


# --------------------------------------------------------------------------
# Financial (spec 14.6)

def _annuity(r, n, out):
    """(1 - (1 + r)^-n) / r at out's precision; n itself when r is 0."""
    if r.is_zero():
        return n.copy(), 0
    b0 = add3(r, Fx(1))
    f0 = like(0, out)
    f0, code = compute_power(b0, neg_of(n), f0.disp, f0.extra)
    if code:
        return f0, code
    t = sub3(Fx(1), f0, Fx(0))
    return div(t, r, out)


def compute_fv(pmt, r, n, rdisp, rextra):
    P = rdisp
    result = Fx(0, rdisp, rextra)
    result.widen(12, 796, 4)
    if not r.is_zero():
        b = add3(r, Fx(1))
        f8 = like(0, result)
        f8, code = compute_power(b, n, f8.disp, f8.extra)
        if code:
            return f8, code
        t = sub3(f8, Fx(1), like(0, result))
        t = mul(pmt, t, like(0, result))
        result, code = div(t, r, result)
    else:
        result, code = mul(pmt, n, result), 0
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, code


def compute_pmt(pmt, r, n, rdisp, rextra, present=False):
    P = rdisp
    result = Fx(0, rdisp, rextra)
    result.widen(12, 796, 4)
    a, code = _annuity(r, n, like(0, result))
    if code == 0:
        if present:
            result = mul(pmt, a, result)
        else:
            result, code = div(pmt, a, result)
    result.round(result.extra + P)
    result.set_disp(clamp(P, 16, MAX_P))
    return result, code


def compute_npv(payments, r, pdisp=16, pextra=2):
    """Sum of v / (1 + r)^k, k = 1, 2, ...; each power and quotient at the
    payment argument's precision, the sum exact."""
    base = r.copy()
    add_into(base, Fx(1))
    total = Fx(0)
    k = Fx(0)
    for v in payments:
        add_into(k, Fx(1))
        pw, code = compute_power(base, k, pdisp, pextra)
        if code:
            return pw, code
        term, code = div(v, pw, Fx(0, pdisp, pextra))
        if code:
            return term, code
        add_into(total, term)
    return total, 0


# --------------------------------------------------------------------------
# Function entry points as the calculation functions call them (spec 14.7):
# the result takes the argument's display precision and guard digits, then
# a function-specific number of guard digits.

def fm_exp(x):
    return compute_exp(x, x.disp, 4)


def fm_ln(x):
    return compute_ln(x, x.disp, 3)


def fm_log(x):
    return compute_log(x, x.disp, 5, 'ln10inv')


def fm_lg(x):
    return compute_log(x, x.disp, 4, 'ln2inv')


def fm_power(x, y):
    return compute_power(x, y, x.disp, x.extra)


def fm_sqrt(x):
    half = assign_double(Fx(0), 0.5)
    return compute_power(x, half, x.disp, x.extra)


def fm_fv(pmt, r, n):
    return compute_fv(pmt, r, n, pmt.disp, pmt.extra)


def fm_pmt(pmt, r, n):
    return compute_pmt(pmt, r, n, pmt.disp, pmt.extra)


def fm_pv(pmt, r, n):
    return compute_pmt(pmt, r, n, pmt.disp, pmt.extra, present=True)


# --------------------------------------------------------------------------
# The binary family (spec 14.8)

def mod2pi(a):
    """Sin, Cos and Tan reduce arguments outside [-1000, 1000] modulo 2 pi
    (pi as a table constant at a's precision, rounded at its held digits;
    the quotient rounded at a's held digits and floored)."""
    if lt(a, Fx(-1000)) or (not lt(a, Fx(1000)) and not eq(a, Fx(1000))):
        pi = table_constant('pi', a)
        pi.round(pi.prec)
        two_pi = mul(pi, Fx(2), like(0, a))
        q, _ = div(a, two_pi, like(0, pi))
        k = q.int_part()
        if q.neg:
            k = -k - (1 if q.m % W ** q.f else 0)
        t = mul(Fx(k), two_pi, like(0, pi))
        a = sub3(a, t, like(0, pi))
    return as_float(a)


def fm_trig(name, x, libm):
    """Sin, Cos, Tan, Atan: the argument cut to p 16 (whole words), reduced
    for sin/cos/tan, converted to a double, passed to `libm(name, double)`
    (glibc's results reproduce FileMaker Server), and the double brought
    back through %.20g at the argument's precision; NaN and infinity give 0."""
    disp = x.disp
    a = x.copy()
    a.set_precision(16)
    d = as_float(a) if name == 'atan' else mod2pi(a)
    r = libm(name, d)
    out = Fx(0, 16, 2)
    if r == r and r not in (float('inf'), float('-inf')):
        assign_double(out, r)
    out.set_precision(disp)
    return out


# --------------------------------------------------------------------------
# Display (calc-values.md 2.7)

def text(x):
    """Number to text in en-US (calc-values.md 2.7): round half away at the
    display precision, then plain or scientific."""
    if x.nan:
        return '?'
    c = x.copy()
    c.round(c.disp)
    if c.m == 0:
        return '0'
    sign = '-' if c.neg else ''
    ip = c.int_part()
    fd = str(c.m % W ** c.f).zfill(9 * c.f)[:c.disp].rstrip('0')
    tiny = ip == 0 and fd[:9] == '000000000'
    if not (ip >= W and ip % W == 0) and not tiny:
        return sign + (str(ip) if ip else '') + ('.' + fd if fd else '')
    if ip:
        exp = len(str(ip)) - 1
        # The mantissa is held in clamp(exp + p, 16, 800) digits without
        # guard digits, whole words, so beyond 800 it keeps 801 fractional
        # digits (spec 14.8).
        keep = 1 + 9 * words_for(clamp(exp + c.disp, 16, MAX_P))
        sig = (str(ip) + fd)[:keep].rstrip('0')
    else:
        exp = -(len(fd) - len(fd.lstrip('0')) + 1)
        sig = fd.lstrip('0')
    return '%s%s.%sE%s%d' % (sign, sig[0], sig[1:] or '0', '+' if exp >= 0 else '-', abs(exp))


# --------------------------------------------------------------------------

def main(argv):
    p = 16
    if '--p' in argv:
        i = argv.index('--p')
        p = int(argv[i + 1])
        argv = argv[:i] + argv[i + 2:]
    if not argv:
        print(__doc__)
        return 2
    fn, args = argv[0].lower(), [Fx.from_decimal(a, eval_p=p) for a in argv[1:]]
    table = {
        'exp': fm_exp, 'ln': fm_ln, 'log': fm_log, 'lg': fm_lg, 'sqrt': fm_sqrt,
        'power': fm_power, 'fv': fm_fv, 'pv': fm_pv, 'pmt': fm_pmt,
    }
    if fn == 'npv':
        r, code = compute_npv(args[1:], args[0])
    else:
        r, code = table[fn](*args)
    print('text   ', text(r))
    print('held   ', r.value() if not r.nan else '?')
    print('error  ', code)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
