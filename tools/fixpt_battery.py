#!/usr/bin/env python3
"""Batteries for FileMaker's transcendental functions and their check
against the executable model (`tools/fixpt_model.py`, spec
docs/re/calc-values.md section 14).

Usage:
  python3 -I tools/fixpt_battery.py gen <dir>
      Write the batteries t01..t13 (deterministic) into <dir>. Run each with
      tools/fms-calc-harness/runbat.sh in the en-US Contacts file (t10 in
      the FileMaker Server sample, fms-sample-en).
  python3 -I tools/fixpt_battery.py check <results.tsv>... [--show N]
      Evaluate every expression of the harness results with the model and
      compare result text and error code; print match counts per function
      and precision and up to N mismatches per file. `--libm <file>` takes
      the trigonometric functions' double results from that file.
  python3 -I tools/fixpt_battery.py libm-calls <results.tsv>...
      Print the C library calls (function, double in hex) the model makes
      for the trigonometric expressions; feed them to
      tools/fms-calc-harness/libm.c in the harness image and pass its output
      to `check --libm <file>` to use glibc's results.
  python3 -I tools/fixpt_battery.py eval '<expression>' [...]
      Print the model's result for expressions (same evaluator).

The evaluator understands what the batteries use: number literals, unary
minus, ^ * / + -, parentheses, SetPrecision, Exp, Ln, Log, Lg, Sqrt, FV,
PMT, PV, NPV over the t10 stand-in field, Sin, Cos, Tan, Atan (the last
four through the host's libm unless `--libm` gives glibc's results).
"""

import math
import os
import random
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixpt_model as fm  # noqa: E402
from fixpt_model import Fx  # noqa: E402

# --------------------------------------------------------------------------
# Expression evaluator with the calculation-level precision rules
# (calc-values.md 2.3, 13, 14.2).

TOKEN = re.compile(r'\s*(?:((?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?)|([A-Za-z]+)|(.))')


def tokenize(s):
    out = []
    for num, name, op in TOKEN.findall(s):
        if num:
            out.append(('num', num))
        elif name:
            out.append(('name', name))
        elif op.strip():
            out.append(('op', op))
    out.append(('end', ''))
    return out


class Eval:
    def __init__(self, text):
        self.t = tokenize(text)
        self.i = 0
        self.error = 0

    def peek(self):
        return self.t[self.i]

    def take(self, kind=None, val=None):
        tok = self.t[self.i]
        if (kind and tok[0] != kind) or (val and tok[1] != val):
            raise SyntaxError('expected %s %s at %r' % (kind, val, tok))
        self.i += 1
        return tok

    def note(self, code):
        if code and not self.error:
            self.error = code

    # The grammar keeps only what FileMaker's precedence needs here:
    # unary minus binds tighter than ^ (-10^2 is (-10)^2).
    def expr(self, p):
        v = self.term(p)
        while self.peek() in (('op', '+'), ('op', '-')):
            op = self.take()[1]
            w = self.term(p)
            v = fm.add3(v, w) if op == '+' else fm.sub3(v, w, Fx(0, p, 2))
        return v

    def term(self, p):
        v = self.power(p)
        while self.peek() in (('op', '*'), ('op', '/')):
            op = self.take()[1]
            w = self.power(p)
            if op == '*':
                v = fm.mul(v, w, Fx(0, p, 2))
            else:
                # `/` gives its result the dividend's guard digits
                out = Fx(0, max(p, v.disp), max(2, v.extra))
                v, code = fm.div(v, w, out)
                self.note(code)
                if code:
                    v = Fx.nan_value(v.disp, v.extra)
        return v

    def power(self, p):
        v = self.unary(p)
        while self.peek() == ('op', '^'):
            self.take()
            w = self.unary(p)
            v, code = fm.fm_power(v, w)
            self.note(code)
            if code:
                v = Fx.nan_value(v.disp, v.extra)
        return v

    def unary(self, p):
        if self.peek() == ('op', '-'):
            self.take()
            return fm.neg_of(self.unary(p))
        if self.peek() == ('op', '+'):
            self.take()
            return self.unary(p)
        return self.primary(p)

    def args(self, p, first_p=None):
        self.take('op', '(')
        out = []
        while True:
            if self.peek()[0] == 'name' and self.peek()[1] in FIELDS:
                out.append(FIELDS[self.take()[1]])
            else:
                out.append(self.expr(first_p if first_p is not None and not out else p))
            if self.peek() == ('op', ';'):
                self.take()
                continue
            self.take('op', ')')
            return out

    def primary(self, p):
        kind, val = self.peek()
        if kind == 'num':
            self.take()
            return Fx.from_decimal(val, eval_p=p)
        if kind == 'op' and val == '(':
            self.take()
            v = self.expr(p)
            self.take('op', ')')
            return v
        if kind == 'name':
            self.take()
            name = val.lower()
            if name == 'setprecision':
                # second argument first, then the expression at that precision
                save = self.i
                self.take('op', '(')
                depth = 1
                j = self.i
                while depth:
                    tk = self.t[j]
                    if tk == ('op', '('):
                        depth += 1
                    elif tk == ('op', ')'):
                        depth -= 1
                    elif tk == ('op', ';') and depth == 1:
                        sep = j
                    j += 1
                self.i = sep + 1
                n = self.expr(p)
                end = self.i
                q = fm.clamp(n.int_part() * (-1 if n.neg else 1), 16, 400)
                self.i = save + 1
                v = self.expr(q)
                self.i = end
                self.take('op', ')')
                c = v.copy()
                c.round(q)
                return c
            a = self.args(p)
            fn = {
                'exp': fm.fm_exp, 'ln': fm.fm_ln, 'log': fm.fm_log, 'lg': fm.fm_lg,
                'sqrt': fm.fm_sqrt, 'fv': fm.fm_fv, 'pmt': fm.fm_pmt, 'pv': fm.fm_pv,
            }.get(name)
            if fn:
                v, code = fn(*a)
                self.note(code)
                return Fx.nan_value(v.disp, v.extra) if code else v
            if name in ('sin', 'cos', 'tan', 'atan'):
                return fm.fm_trig(name, a[0], libm)
            if name == 'npv':
                pays = [Fx.from_decimal(str(v), eval_p=p) for v in a[0]]
                v, code = fm.compute_npv(pays, a[1], p, 2)
                self.note(code)
                return Fx.nan_value(v.disp, v.extra) if code else v
            raise SyntaxError('unknown function ' + val)
        raise SyntaxError('unexpected %r' % (val,))


# Results of the C library's functions for the doubles the model passes it
# (`libm-calls`, then tools/fms-calc-harness/libm.c in the harness image:
# glibc 2.39, what FileMaker Server's engine runs on); None: the host's.
LIBM = None
LIBM_CALLS = set()


def load_libm(path):
    table = {}
    for line in open(path):
        parts = line.split()
        if len(parts) >= 3:
            table[(parts[0], float.fromhex(parts[1]).hex())] = float.fromhex(parts[2]) \
                if 'nan' not in parts[2] and 'inf' not in parts[2] else float(parts[2])
    return table


def libm(name, d):
    """The C library function: glibc's result when loaded, else the host's."""
    LIBM_CALLS.add((name, d.hex()))
    if LIBM is not None and (name, d.hex()) in LIBM:
        return LIBM[(name, d.hex())]
    try:
        return getattr(math, name)(d)
    except ValueError:
        return float('nan')


# Stand-ins for the related field the NPV battery reads (t10): the
# FileMaker Server sample's Tasks, related through the self-join Related
# Tasks, in creation order (calc-values.md 12.1).
FIELDS = {'RELID': [7, 1, 3, 12, 5, 6, 8, 11, 9, 2, 10, 4]}
FIELD_TEXT = {'${Related Tasks}::${TASK ID MATCH FIELD}': 'RELID'}


def evaluate(text):
    for k, v in FIELD_TEXT.items():
        text = text.replace(k, v)
    e = Eval(text)
    v = e.expr(16)
    if e.peek()[0] != 'end':
        raise SyntaxError('trailing input at %r' % (e.peek(),))
    return fm.text(v), e.error


# --------------------------------------------------------------------------
# Battery generation

PRECISIONS = (16, 20, 30, 50, 100, 400)


def rnd_digits(rng, n):
    return ''.join(rng.choice('0123456789') for _ in range(n))


def rnd_number(rng, lo_exp, hi_exp, max_sig, max_frac):
    """A positive plain decimal m * 10^e with 1..max_sig significant digits,
    leading digit at 10^e (lo_exp <= e <= hi_exp), at most max_frac
    fractional digits."""
    while True:
        e = rng.randint(lo_exp, hi_exp)
        sig = rng.randint(1, max_sig)
        digits = rng.choice('123456789') + rnd_digits(rng, sig - 1)
        # value = 0.digits * 10^(e+1)
        point = e + 1
        if point >= len(digits):
            s = digits + '0' * (point - len(digits))
        elif point > 0:
            s = digits[:point] + '.' + digits[point:]
        else:
            s = '.' + '0' * (-point) + digits
        s = s.rstrip('0').rstrip('.') if '.' in s else s
        frac = len(s.split('.')[1]) if '.' in s else 0
        if frac <= max_frac and s not in ('', '.'):
            return s


def lines_for(fn_text, p, held=True):
    out = ['SetPrecision ( %s ; %d )' % (fn_text, p)]
    if held:
        out.append('SetPrecision ( %s * 10^6 ; %d )' % (fn_text, p))
    return out


def gen_exp(rng):
    rows = []
    for p in PRECISIONS:
        n = 30 if p == 400 else 60
        args = []
        for _ in range(n):
            bucket = rng.random()
            if bucket < 0.3:
                s = rnd_number(rng, -6, 0, 20, p)
            elif bucket < 0.6:
                s = rnd_number(rng, 0, 1, 25, p)
            elif bucket < 0.8:
                s = rnd_number(rng, 2, 2, 25, p)
            elif bucket < 0.95:
                s = '%d.%s' % (rng.randint(1000, 1840), rnd_digits(rng, rng.randint(1, 8)))
                s = s.rstrip('0').rstrip('.')
            else:
                s = str(rng.randint(1, 60))
            args.append(('-' if rng.random() < 0.4 else '') + s)
        args += ['1', '-1', '2.302585092994045684', '100', '-100', '1000', '-1000', '1844.37', '-1844.37', '-1844.38']
        for a in args:
            big = abs(float(a)) > 1790 and not a.startswith('-')
            rows += lines_for('Exp ( %s )' % a, p, held=not big)
    return rows


def gen_log(rng, fn):
    rows = []
    for p in PRECISIONS:
        n = 25 if p == 400 else 55
        args = []
        for _ in range(n):
            bucket = rng.random()
            if bucket < 0.25:
                s = rnd_number(rng, -min(p - 2, 60), -1, 20, p)
            elif bucket < 0.6:
                s = rnd_number(rng, 0, 3, 25, p)
            elif bucket < 0.85:
                s = rnd_number(rng, 4, 60, 30, p)
            else:
                s = rnd_number(rng, 60, 700, 20, p)
            args.append(s)
        args += ['2', '10', '1.4', '.6', '1.0000001', '.99999999', '1000000', '7']
        for a in args:
            rows += lines_for('%s ( %s )' % (fn, a), p)
    return rows


def gen_power(rng, kind):
    rows = []
    for p in PRECISIONS:
        n = 25 if p == 400 else 55
        for _ in range(n):
            if kind == 'int':
                base = rnd_number(rng, -1, 1, 18, p)
                if rng.random() < 0.2:
                    base = '-' + base
                e = rng.choice([rng.randint(2, 40), rng.randint(41, 400), rng.randint(401, 2000)])
                if rng.random() < 0.3:
                    e = -e
                expr = '( %s ) ^ ( %d )' % (base, e)
            else:
                base = rnd_number(rng, -3, 6, 20, p)
                e = rnd_number(rng, -2, 1, 12, p)
                if rng.random() < 0.3:
                    e = '-' + e
                expr = '( %s ) ^ ( %s )' % (base, e)
            rows += lines_for(expr, p)
        if kind == 'frac':
            for a in ('2', '10', '1.5', '1000000000000000000000000000000'):
                rows += lines_for('Sqrt ( %s )' % a, p)
    return rows


def gen_sqrt(rng):
    rows = []
    for p in PRECISIONS:
        n = 25 if p == 400 else 55
        for _ in range(n):
            bucket = rng.random()
            if bucket < 0.3:
                s = rnd_number(rng, -min(p - 2, 40), -1, 20, p)
            elif bucket < 0.7:
                s = rnd_number(rng, 0, 30, 25, p)
            else:
                s = rnd_number(rng, 30, 380, 20, p)
            rows += lines_for('Sqrt ( %s )' % s, p)
    return rows


def gen_fin(rng):
    rows = []
    for p in PRECISIONS[:5]:
        for _ in range(45):
            fn = rng.choice(['FV', 'PMT', 'PV'])
            pmt = rnd_number(rng, 0, 5, 9, 2)
            if rng.random() < 0.2:
                pmt = '-' + pmt
            rate = rnd_number(rng, -4, -1, 8, p)
            n = rng.choice([str(rng.randint(1, 60)), str(rng.randint(61, 400)),
                            rnd_number(rng, 0, 1, 4, 3)])
            rows += lines_for('%s ( %s ; %s ; %s )' % (fn, pmt, rate, n), p)
    return rows


EDGE_ARGS = {
    # Ln: the 1.4 / 0.6 thresholds (doubles), near 1, the iteration cap
    'Ln': ['1.3999', '1.39999999999999991', '1.3999999999999999112', '1.39999999999999991121',
           '1.4000000000000001', '.6', '.59999999999999997', '.5999999999999999778',
           '.6000000000000001', '.61', '1.39', '.3', '.2', '1.0000000000000000000001',
           '.9999999999999999', '1.00000000000000000001', '123456789.123456789012345678',
           '1.23456789012345678901234567890123456789'],
    'Exp': ['0', '1.151292546497022842', '3.4538776394910684', '-1.151292546497022842',
            '4999.9', '5000', '5000.0000000000001', '-5000', '-4999.99', '1841', '-1840',
            '.00000000000000000001', '-.00000000000000000001', '2.302585092994045684017991454684364208',
            '23.02585092994045684017991454684364207601101488628772976033'],
}
EDGE_EXPRS = [
    'Exp ( Ln ( 5 ) )', 'Exp ( Exp ( 1 ) )', 'Ln ( Exp ( 2 ) )', 'Sqrt ( Exp ( 3 ) )',
    'Log ( Exp ( 1 ) )', 'Exp ( Log ( 2 ) )', 'Lg ( Exp ( 1 ) )', 'Exp ( 1 ) ^ 2',
    '2 ^ Exp ( 1 )', 'Exp ( 1 ) ^ .5', 'Ln ( 2 ) * Exp ( 1 )', 'Exp ( 1 ) / 3',
    '0 ^ 0', '0 ^ .5', '0 ^ -.5', '0 ^ -1', '0 ^ 2', '1 ^ 1000000', '( -2 ) ^ 3',
    '( -2 ) ^ -3', '( -2 ) ^ .5', '( -8 ) ^ ( 1 / 3 )', '10 ^ 404', '10 ^ 405',
    '.1 ^ -404', '.1 ^ 404', '2 ^ 1350', '2 ^ -1350', '1.0000000001 ^ 1000000000',
    '10 ^ -400', '7 ^ 25', '3 ^ 40', '1.5 ^ -100', '2 ^ 62.5', '2 ^ 200.5', '10 ^ 15.5',
    '1' + '0' * 405 + ' ^ 1', '2 ^ 1' + '0' * 405, '.5 ^ 1' + '0' * 300, '1.5 ^ 1000',
    '1.5 ^ -1000', '( 1 / 3 ) ^ 7', '( 2 / 3 ) ^ 1.5', '1.000001 ^ 123456.789',
    'FV ( 100 ; 0 ; 10 )', 'PMT ( 1000 ; 0 ; 12 )', 'PV ( 100 ; 0 ; 10 )', 'FV ( 100 ; .1 ; -2 )',
    'FV ( 100 ; .1 ; 2.5 )', 'PMT ( 100 ; .1 ; 0 )', 'PV ( 100 ; -.5 ; 500 )', 'FV ( 100 ; -1 ; 10 )',
    'FV ( 100 ; -1.5 ; 2.5 )', 'FV ( 100 ; 1 ; 2000 )', 'PV ( 100 ; 1 ; 2000 )', 'PMT ( 100 ; -1 ; 10 )',
    'FV ( 1 ; 1 / 3 ; 360 )', 'FV ( 1000 ; 2 / 7 ; 360 )', 'FV ( 50 ; .5 ; 360 )',
    'PMT ( 12345.67 ; .11 / 12 ; 7.5 )', 'PMT ( -250 ; .07 / 12 ; 7.5 )', 'PV ( 1 ; 1 / 3 ; 360 )',
    'PMT ( 1.0000000000000000001 ; .1 ; 3 )', 'FV ( 50 ; .11 / 12 ; 60 )',
]


def gen_edges(rng):
    rows = []
    for p in (16, 30, 50, 400):
        for fn, args in EDGE_ARGS.items():
            for a in args:
                rows += lines_for('%s ( %s )' % (fn, a), p)
        for e in EDGE_EXPRS:
            rows += lines_for(e, p)
    return rows


def gen_npv(rng):
    rows = ['# Record context: FileMaker Server sample, Tasks record 30 (see x03-related);',
            '# NPV over the 12 related TASK ID MATCH FIELD values 7 1 3 12 5 6 8 11 9 2 10 4.',
            '@ 03d00001 30']
    rates = ['.1', '1 / 3', '.05 / 12', '2', '.0001', '.987654321', '.5', '1.25']
    rates += [rnd_number(rng, -3, 0, 12, 16) for _ in range(12)]
    for p in PRECISIONS:
        for r in rates:
            rows += lines_for('NPV ( ${Related Tasks}::${TASK ID MATCH FIELD} ; %s )' % r, p)
    return rows


def gen_trig(rng):
    rows = []
    for e in ['Atan ( 10^308 )', 'Atan ( 10^309 )', 'Atan ( 10^314 )', 'Atan ( 10^315 )',
              'Atan ( 10^316 )', 'Atan ( 10^323 )', 'Atan ( 9 * 10^323 )', 'Atan ( 10^324 )',
              'Atan ( 10^324 + 10^315 )', 'Atan ( 10^324 + 1 )', 'Atan ( 10^330 )',
              'Atan ( 10^333 + 10^324 + 10^315 )', 'Atan ( 10^400 )', 'Atan ( -(10^400) )',
              'Atan ( -(10^320) )', 'Cos ( 10^400 )',
              'Sin ( .000000000000000001 )', 'Sin ( .0000000000000000015 )',
              'Sin ( .0000000000000000001 )', 'Sin ( .00000000000000000001 )',
              'Sin ( 1.0000000000000000009 )', 'Sin ( 1.000000000000000001 )',
              'Sin ( 1529 )', 'Sin ( 1529.0000000000000001 )', 'Sin ( 200 )', 'Sin ( 21 )']:
        rows.append(e)
        rows.append('SetPrecision ( %s ; 30 )' % e)
    return rows


def gen_trig_reduce(rng):
    rows = ['Sin ( %d )' % n for n in range(1001, 2101)]
    for _ in range(150):
        fn = rng.choice(['Sin', 'Cos', 'Tan'])
        a = rnd_number(rng, 3, 20, 18, 12)
        rows.append('%s ( %s%s )' % (fn, '-' if rng.random() < 0.3 else '', a))
    return rows


ARITH = [
    # opposite signs equal at the larger display precision give 0 (14.2)
    '(1/3 - .3333333333333333)*10^20', '(.3333333333333333 - 1/3)*10^20',
    '(1/3 + -.3333333333333333)*10^20', '(1/3 - .33333333333333333)*10^20',
    '(Exp(1) - 2.718281828459045235)*10^22', '(Exp(1) - 2.7182818284590452)*10^22',
    '(2/3 - .6666666666666667)*10^20', '((1/3)*3 - 1)*10^20', '(1 - (1/3)*3)*10^20',
    '(1/7 - .1428571428571429)*10^20', '(1/3 - .333333333333333)*10^20',
    # products: rows over the right operand, carries from cut words (14.2)
    '(2/3)*(1/7)*10^18', '(2/3)*(2/7)*10^18', '(2/3)*(4/7)*10^18', '(2/3)*(1/9)*10^18',
    '(2/3)*(2/9)*10^18', '(2/3)*(3/9)*10^18', '(1/7)*(2/3)*10^18',
    '(1/3)*(2/3)*10^18', '(2/3)*(1/3)*10^18', '(1/3)*(2/7)*10^18', '(2/7)*(1/3)*10^18',
    '(1/3)*(5/9)*10^18', '(5/9)*(1/3)*10^18',
    # quotients keep the dividend's guard digits
    'Exp(1)/3*10^18', 'Ln(2)/3*10^18', 'Log(2)/7*10^18', '1/Exp(1)*10^18',
]


def gen(outdir):
    rng = random.Random(20261028)
    os.makedirs(outdir, exist_ok=True)
    batteries = [
        ('t01-exp', gen_exp(rng)),
        ('t02-ln', gen_log(rng, 'Ln')),
        ('t03-log', gen_log(rng, 'Log')),
        ('t04-lg', gen_log(rng, 'Lg')),
        ('t05-sqrt', gen_sqrt(rng)),
        ('t06-power-int', gen_power(rng, 'int')),
        ('t07-power-frac', gen_power(rng, 'frac')),
        ('t08-financial', gen_fin(rng)),
        ('t09-edges', gen_edges(rng)),
        ('t10-npv', gen_npv(rng)),
        ('t11-trig', gen_trig(rng)),
        ('t12-trig-reduce', gen_trig_reduce(rng)),
        ('t13-arith', list(ARITH)),
    ]
    for name, rows in batteries:
        with open(os.path.join(outdir, name + '.txt'), 'w') as f:
            f.write('# %s: generated by tools/fixpt_battery.py gen (seed 20261028)\n' % name)
            for r in rows:
                f.write(r + '\n')
        print(name, len(rows))


# --------------------------------------------------------------------------
# Checking

FN = re.compile(r'(Exp|Ln|Log|Lg|Sqrt|FV|PMT|PV|NPV|Sin|Cos|Tan|Atan|\^)')


def classify(expr):
    m = re.match(r'SetPrecision \( (.*) ; (\d+) \)$', expr)
    if m:
        body, p = m.group(1), int(m.group(2))
    else:
        body, p = expr, 16
    held = body.endswith('* 10^6')
    f = FN.search(body)
    name = f.group(1) if f else 'other'
    if name == '^':
        ex = body.split('^')[1].replace('* 10', '')
        name = '^ int' if re.fullmatch(r'\s*\( -?\d+ \)\s*', ex) else '^ frac'
    return name, p, held


def check(paths, show):
    total = {}
    shown = 0
    for path in paths:
        for line in open(path, encoding='utf-8'):
            line = line.rstrip('\n')
            if not line or line.startswith('#') or line.startswith('@'):
                continue
            cols = line.split('\t')
            if len(cols) < 4:
                continue
            expr, err, _types, want = cols[0], cols[1], cols[2], cols[3]
            try:
                got, gerr = evaluate(expr)
            except Exception as ex:  # noqa: BLE001
                got, gerr = 'model error: %s' % ex, -1
            name, p, held = classify(expr)
            if 't13-arith' in path:
                name = 'arith'
            key = (name, p)
            t = total.setdefault(key, [0, 0])
            t[0] += 1
            ok = got == want and str(gerr) == err
            if ok:
                t[1] += 1
            elif shown < show:
                shown += 1
                print('MISMATCH %s\n   engine %s (error %s)\n   model  %s (error %s)' % (expr, want, err, got, gerr))
    names = sorted({k[0] for k in total})
    print('%-10s %s %s' % ('function', ' '.join('%11s' % ('p=%d' % p) for p in PRECISIONS), '      all'))
    grand = [0, 0]
    for n in names:
        cells = []
        tot = [0, 0]
        for p in PRECISIONS:
            c = total.get((n, p))
            if c:
                cells.append('%5d/%-5d' % (c[1], c[0]))
                tot[0] += c[0]
                tot[1] += c[1]
            else:
                cells.append('%11s' % '-')
        grand[0] += tot[0]
        grand[1] += tot[1]
        print('%-10s %s %5d/%-5d' % (n, ' '.join(cells), tot[1], tot[0]))
    print('total %d of %d match (%.2f%%)' % (grand[1], grand[0], 100.0 * grand[1] / max(1, grand[0])))
    return grand


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    cmd = argv[0]
    if cmd == 'gen':
        gen(argv[1])
    elif cmd == 'libm-calls':
        for path in argv[1:]:
            for line in open(path, encoding='utf-8'):
                if line[:1] not in '#@' and '\t' in line:
                    try:
                        evaluate(line.split('\t')[0])
                    except Exception:  # noqa: BLE001
                        pass
        for name, h in sorted(LIBM_CALLS):
            print(name, h)
    elif cmd == 'check':
        global LIBM
        if '--libm' in argv:
            i = argv.index('--libm')
            LIBM = load_libm(argv[i + 1])
            argv = argv[:i] + argv[i + 2:]
        show = 20
        if '--show' in argv:
            i = argv.index('--show')
            show = int(argv[i + 1])
            argv = argv[:i] + argv[i + 2:]
        check(argv[1:], show)
    elif cmd == 'eval':
        for e in argv[1:]:
            print('%s\t%s\t%s' % ((e,) + evaluate(e)))
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
