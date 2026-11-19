#!/usr/bin/env python3
"""Decode, print, compile and re-encode FileMaker stored calculations.

Spec: docs/re/calc-encoding.md. Input rows come from the read-only example
`cargo run -p om-hbam --example dump_calcs -- <file.fmp12>...`.

Usage:
  python3 -I tools/calc_polish.py check <calcs.tsv>...
      Decode every formula, re-encode both streams byte for byte, compile the
      formatted (source-order) token stream into Polish code and compare it
      with the stored Polish stream. Prints coverage and every failure.
  python3 -I tools/calc_polish.py print <calcs.tsv> [path-substring]
      Print formulas as text, from the formatted stream and from the Polish
      stream (the latter regenerates source from code alone).
  python3 -I tools/calc_polish.py diff <calcs.tsv> <file-and-path-substring>
      Compare the stored Polish tree with the one compiled from the
      formatted stream, side by side.
  python3 -I tools/calc_polish.py hex <polish-hex> [<formatted-hex>]
      Decode one formula given as hex.

Read-only. Strings in the input are untrusted data and are only printed.
"""
import csv
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CATALOGS = os.path.join(HERE, '..', 'docs', 're', 'catalogs')

# --------------------------------------------------------------------------
# Opcode catalog (docs/re/catalogs/calc-tokens.csv)

OPS = {}        # opcode -> row dict
GET = {}        # selector -> name
BUILTIN = {}    # (plugin id, function id) -> name of a built-in "plugin" function


def load_catalogs():
    with open(os.path.join(CATALOGS, 'calc-tokens.csv'), newline='') as fh:
        for row in csv.DictReader(fh):
            if row['kind'] == 'builtin-plugin':
                quad, fid = row['stored'].split()[3:5]
                BUILTIN[(bytes.fromhex(quad), int(fid, 16))] = row['source_name']
                continue
            OPS[int(row['opcode'])] = row
    with open(os.path.join(CATALOGS, 'get-selectors.csv'), newline='') as fh:
        for row in csv.DictReader(fh):
            if row['selector']:
                GET[int(row['index'])] = row['selector']


def nargs(op):
    return OPS[op]['nargs'] if op in OPS else 'Unknown'


def arity(op):
    return int(OPS[op]['arity']) if op in OPS and OPS[op]['arity'] else 0


# Operators: opcode -> (display text, precedence level); larger binds tighter.
BINARY = {
    32: ('or', 1), 33: ('xor', 1),
    31: ('and', 2),
    65: ('<', 3), 66: ('<=', 3), 67: ('≤', 3), 68: ('=', 3), 69: ('<>', 3),
    70: ('≠', 3), 71: ('>=', 3), 72: ('≥', 3), 73: ('>', 3),
    80: ('&', 4),
    37: ('+', 5), 38: ('-', 5),
    39: ('*', 6), 40: ('/', 6),
    41: ('^', 7),
}
UNARY = {30: 'not ', 44: '-', 104: '+'}   # 104 GetAsNumber is how unary + compiles
UNARY_TOKEN = {30: 30, 38: 44, 37: 104}  # source token -> node opcode
PUNCT = {3: ';', 4: '(', 5: ')', 6: '[', 7: ']', 36: '¶'}

EXT = {0xf8: 2000, 0xf9: 2300, 0xfa: 2900, 0xfb: 3200, 0xfc: 3500, 0xfd: 3800, 0xfe: 4100}


class Bad(Exception):
    pass


# --------------------------------------------------------------------------
# Byte reader and SCSU (UTS #6, every byte XOR 0x5A)

class Reader:
    def __init__(self, data):
        self.b = data
        self.i = 0

    def eof(self):
        return self.i >= len(self.b)

    def u8(self):
        if self.i >= len(self.b):
            raise Bad('truncated')
        self.i += 1
        return self.b[self.i - 1]

    def take(self, n):
        if self.i + n > len(self.b):
            raise Bad('truncated')
        self.i += n
        return self.b[self.i - n:self.i]

    def u16(self):
        return int.from_bytes(self.take(2), 'big')

    def peek(self):
        return self.b[self.i] if self.i < len(self.b) else None


def read_opcode(r):
    b = r.u8()
    if b >= 0xf8:
        c = r.u8()
        if b == 0xff:
            raise Bad('opcode prefix ff')
        return EXT[b] + c
    return b


def write_opcode(op):
    if op < 1000:
        return bytes([op])
    for prefix, base in EXT.items():
        if base <= op < base + 256:
            return bytes([prefix, op - base])
    if 1000 <= op < 1256:
        return bytes([0x9b, op - 1000])
    raise Bad('opcode %d has no encoding' % op)


def read_count(r):
    """Character count before an SCSU run (SwapIStream::InternalSCSUExpand)."""
    b = r.u8()
    if b < 0x80:
        return b
    if b <= 0xbf:
        return ((b & 0x3f) << 8) | r.u8()
    if b == 0xc2:
        return r.u16()
    if b == 0xc4:
        return int.from_bytes(r.take(4), 'big')
    raise Bad('bad length prefix %02x' % b)


def write_count(n):
    if n <= 0x7f:
        return bytes([n])
    if n <= 0x3fff:
        return (0x8000 | n).to_bytes(2, 'big')
    if n <= 0xffff:
        return b'\xc2' + n.to_bytes(2, 'big')
    return b'\xc4' + n.to_bytes(4, 'big')


WINDOW = [0x0080, 0x00C0, 0x0400, 0x0600, 0x0900, 0x3040, 0x30A0, 0xFF00]
STATIC = [0x0000, 0x0080, 0x0100, 0x0300, 0x2000, 0x2080, 0x2100, 0x3000]
SPECIAL_OFFSETS = {0xF9: 0x00C0, 0xFA: 0x0250, 0xFB: 0x0370, 0xFC: 0x0530,
                   0xFD: 0x3040, 0xFE: 0x30A0, 0xFF: 0xFF60}


def window_offset(x):
    if 0x01 <= x <= 0x67:
        return x * 0x80
    if 0x68 <= x <= 0xA7:
        return x * 0x80 + 0xAC00
    if x in SPECIAL_OFFSETS:
        return SPECIAL_OFFSETS[x]
    raise Bad('reserved SCSU window %02x' % x)


def scsu_read(r, count):
    """Expand masked SCSU until `count` UTF-16 units; return (text, raw bytes)."""
    start = r.i
    units = []
    win = list(WINDOW)
    active = 0
    unicode_mode = False

    def nb():
        return r.u8() ^ 0x5A

    def emit(cp):
        if cp >= 0x10000:
            cp -= 0x10000
            units.extend([0xD800 + (cp >> 10), 0xDC00 + (cp & 0x3FF)])
        else:
            units.append(cp)

    while len(units) < count:
        c = nb()
        if not unicode_mode:
            if c >= 0x80:
                emit(win[active] + c - 0x80)
            elif c in (0x00, 0x09, 0x0A, 0x0D) or 0x20 <= c <= 0x7F:
                emit(c)
            elif 0x01 <= c <= 0x08:
                d = nb()
                emit(STATIC[c - 1] + d if d < 0x80 else win[c - 1] + d - 0x80)
            elif 0x10 <= c <= 0x17:
                active = c - 0x10
            elif 0x18 <= c <= 0x1F:
                active = c - 0x18
                win[active] = window_offset(nb())
            elif c == 0x0B:
                h, lo = nb(), nb()
                active = h >> 5
                win[active] = 0x10000 + ((((h & 0x1F) << 8) | lo) << 7)
            elif c == 0x0E:
                h, lo = nb(), nb()
                units.append((h << 8) | lo)
            elif c == 0x0F:
                unicode_mode = True
            else:
                raise Bad('SCSU tag %02x' % c)
        else:
            if 0xE0 <= c <= 0xE7:
                active = c - 0xE0
                unicode_mode = False
            elif 0xE8 <= c <= 0xEF:
                active = c - 0xE8
                win[active] = window_offset(nb())
                unicode_mode = False
            elif c == 0xF0:
                h, lo = nb(), nb()
                units.append((h << 8) | lo)
            elif c == 0xF1:
                h, lo = nb(), nb()
                active = h >> 5
                win[active] = 0x10000 + ((((h & 0x1F) << 8) | lo) << 7)
                unicode_mode = False
            else:
                units.append((c << 8) | nb())
    if len(units) != count:
        raise Bad('SCSU overran its count')
    text = b''.join(u.to_bytes(2, 'big') for u in units).decode('utf-16-be', 'surrogatepass')
    return text, bytes(r.b[start:r.i])


def compressible(u):
    """Units SCSU can carry in single-byte mode through some window."""
    return u < 0x3400 or u >= 0xE000


def scsu_encode(text):
    """FileMaker's SCSU compressor (Draco::SCSUCompress), masked with 0x5A.
    Returns (UTF-16 unit count, bytes). docs/re/calc-encoding.md 3.3."""
    raw = text.encode('utf-16-be', 'surrogatepass')
    units = [int.from_bytes(raw[k:k + 2], 'big') for k in range(0, len(raw), 2)]
    n = len(units)
    win = list(WINDOW)
    state = {'active': 0, 'next': 3}
    out = bytearray()

    def locate(cp):
        for w in range(8):
            if win[w] <= cp < win[w] + 0x80:
                return w
        return None

    def define(cp):
        """PositionWindow: define the next window (round robin from 3) for cp.
        Returns (operand bytes, extended, window) or None when cp has none."""
        fixed = ((0x250, 0xFA), (0x370, 0xFB), (0x530, 0xFC), (0x3040, 0xFD),
                 (0x30A0, 0xFE), (0xFF60, 0xFF))
        ext = False
        if 0xC0 <= cp < 0x140:
            idx, off = cp >> 7, cp & 0xFF80
        elif any(lo <= cp < lo + 0x80 for lo, _ in fixed):
            off, idx = next((lo, t) for lo, t in fixed if lo <= cp < lo + 0x80)
        elif cp <= 0x33FF:
            idx, off = cp >> 7, cp & 0xFF80
        elif cp < 0xE000:
            return None
        elif cp <= 0xFFFF:
            idx, off = (cp - 0xAC00) >> 7, cp & 0xFF80
        else:
            off, ext = cp & 0x1FFF80, True
        w = state['next']
        win[w] = off
        state['active'] = w
        state['next'] = (w + 1) & 7
        if ext:
            v = (w << 13) | ((cp - 0x10000) >> 7)
            return bytes([v >> 8, v & 0xFF]), True, w
        return bytes([idx]), False, w

    i = 0
    unicode_mode = False
    while i < n:
        u = units[i]
        if not unicode_mode:
            cp, size = u, 1
            if 0xD800 <= u < 0xDC00 and i + 1 < n and 0xDC00 <= units[i + 1] < 0xE000:
                cp, size = 0x10000 + ((u - 0xD800) << 10) + (units[i + 1] - 0xDC00), 2
            active = state['active']
            if 1 <= cp <= 0x1F and cp not in (9, 10, 13):
                out += bytes([0x01, cp])
            elif cp < 0x80:
                out.append(cp)
            elif win[active] <= cp < win[active] + 0x80:
                out.append(0x80 + cp - win[active])
            elif locate(cp) is not None:
                w = locate(cp)
                nxt = units[i + size] if i + size < n else None
                if nxt is not None and win[active] <= nxt < win[active] + 0x80:
                    out += bytes([0x01 + w, 0x80 + cp - win[w]])      # SQn: quote
                else:
                    out += bytes([0x10 + w, 0x80 + cp - win[w]])      # SCn: switch
                    state['active'] = w
            elif cp < 0x100:
                out += bytes([0x02, cp - 0x80])
            elif cp < 0x180:
                out += bytes([0x03, cp - 0x100])
            elif (cp & ~0x7F) in (0x300, 0x2000, 0x2080, 0x2100, 0x3000):
                tag = {0x300: 4, 0x2000: 5, 0x2080: 6, 0x2100: 7, 0x3000: 8}[cp & ~0x7F]
                out += bytes([tag, cp & 0x7F])
            else:
                d = define(cp)
                if d is not None:
                    operand, ext, w = d
                    out += (bytes([0x0B]) if ext else bytes([0x18 + w])) + operand
                    continue                                          # emit cp next round
                if (i + size + 1 < n and compressible(units[i + size])
                        and compressible(units[i + size + 1])) or i + size >= n:
                    out += bytes([0x0E, cp >> 8, cp & 0xFF])          # SQU
                else:
                    out.append(0x0F)                                  # SCU
                    unicode_mode = True
                    continue
            i += size
        else:
            if i + 1 < n and compressible(u) and compressible(units[i + 1]):
                target = u
                if u <= 0x7F:
                    j = i
                    while j < n and compressible(units[j]):
                        target = units[j]
                        if target > 0x7F:
                            break
                        j += 1
                if target <= 0x7F:
                    out.append(0xE0 + state['active'])
                elif locate(target) is not None:
                    w = locate(target)
                    state['active'] = w
                    out.append(0xE0 + w)
                else:
                    operand, ext, w = define(target)
                    out += (bytes([0xF1]) if ext else bytes([0xE8 + w])) + operand
                unicode_mode = False
                continue
            if 0xE000 <= u <= 0xF2FF and (i + 1 >= n or not compressible(units[i + 1])):
                out += bytes([0xF0, u >> 8, u & 0xFF])                # UQU
            else:
                out += bytes([u >> 8, u & 0xFF])
            i += 1
    return n, bytes(b ^ 0x5A for b in out)


# --------------------------------------------------------------------------
# Operands shared by both streams

class Text:
    """A text literal or name: count, raw masked SCSU and optional style data."""

    def __init__(self, text, count, scsu, style=None):
        self.text, self.count, self.scsu, self.style = text, count, scsu, style

    def encode(self, with_style=True):
        out = write_count(self.count) + self.scsu
        if with_style:
            out += self.style if self.style else b'\x00'
        return out


def read_text(r, with_style=True):
    count = read_count(r)
    text, scsu = scsu_read(r, count)
    style = None
    if with_style:
        flag = r.u8()
        if flag:
            n = int.from_bytes(r.take(4), 'big')
            style = bytes([flag]) + n.to_bytes(4, 'big') + r.take(n)
    return Text(text, count, scsu, style)


class Number:
    """Draco::FixPt in Polish form: 0 empty, 1 NaN, 2 value."""

    def __init__(self, tag, whole=0, prec=0, limbs=()):
        self.tag, self.whole, self.prec, self.limbs = tag, whole, prec, list(limbs)

    def encode(self):
        if self.tag != 2:
            return bytes([self.tag])
        out = b'\x02' + self.whole.to_bytes(2, 'big', signed=True) + self.prec.to_bytes(2, 'big')
        return out + b''.join(x.to_bytes(4, 'big') for x in self.limbs)

    def text(self):
        if self.tag == 0:
            return ''
        if self.tag == 1:
            return '?'
        n = abs(self.whole)
        whole = self.limbs[:n]
        frac = self.limbs[n:]
        w = ''.join('%09d' % x for x in whole).lstrip('0') or '0'
        f = ''.join('%09d' % x for x in frac).rstrip('0')
        s = w + ('.' + f if f else '')
        return ('-' if self.whole < 0 else '') + s


def read_number(r):
    tag = r.u8()
    if tag != 2:
        if tag not in (0, 1):
            raise Bad('FixPt tag %d' % tag)
        return Number(tag)
    whole = int.from_bytes(r.take(2), 'big', signed=True)
    prec = r.u16()
    nfrac = (prec + 8) // 9
    limbs = [int.from_bytes(r.take(4), 'big') for _ in range(abs(whole) + nfrac)]
    return Number(2, whole, prec, limbs)


def read_key(r):
    """A key string: length byte then that many bytes (HBKey, at most 5)."""
    n = r.u8()
    if n > 5:
        raise Bad('key string of %d bytes' % n)
    return r.take(n)


def key_id(key):
    """Numeric id of a one-segment key string (`01 0a` -> 10)."""
    if not key or key[0] != len(key) - 1:
        return None
    return int.from_bytes(key[1:], 'big') if len(key) > 1 else 0


class Field:
    def __init__(self, op, flags, to, field, rep):
        self.op, self.flags, self.to, self.field, self.rep = op, flags, to, field, rep

    def encode(self):
        return (bytes([self.op, self.flags, len(self.to)]) + self.to + bytes([len(self.field)])
                + self.field + self.rep.to_bytes(2, 'big'))


def read_field(r, op):
    flags = r.u8()
    to = read_key(r)
    field = read_key(r)
    rep = r.u16()
    return Field(op, flags, to, field, rep)


# --------------------------------------------------------------------------
# Unformatted (Polish) stream: tree nodes
#
# Node(kind, op, args, ...). `args` are always in source order; the encoder
# reverses where FileMaker stores them reversed.

class Node:
    def __init__(self, kind, op=None, args=None, **kw):
        self.kind, self.op, self.args = kind, op, args or []
        self.__dict__.update(kw)

    def __repr__(self):
        return 'Node(%s,%s,%r)' % (self.kind, self.op, self.args)


def decode_polish(data):
    r = Reader(data)
    node = read_expr(r)
    if not r.eof():
        raise Bad('trailing bytes after expression at %d of %d' % (r.i, len(data)))
    return node


def read_n(r, n):
    return [read_expr(r) for _ in range(n)]


def read_expr(r, depth=0):
    if depth > 2000:
        raise Bad('nesting too deep')
    at = r.i
    op = read_opcode(r)
    if op == 0:
        return Node('empty', 0)
    if op in (16, 17):
        return Node('num', op, num=read_number(r))
    if op == 19:
        return Node('text', op, text=read_text(r))
    if op == 11:
        t = read_text(r)
        return Node('symbol', op, text=t, flag=r.u8())
    if op in (22, 23):
        return Node('field', op, field=read_field(r, op))
    if op == 20:
        return Node('key', op, key=read_key(r))
    if op == 21:
        return Node('plugin_id', op, plugin=r.take(4), fid=r.u16())
    if op == 24:
        return Node('var', op, index=r.u16())
    if op == 26:
        return Node('svar', op, text=read_text(r, with_style=False))
    if op == 155:
        return Node('get', op, sel=r.u8())
    if op == 25:
        n = r.u16()
        return Node('list', op, list(reversed(read_n(r, n))))
    if op == 75:
        return Node('if', op, read_n(r, 3))
    if op == 76:
        n = r.u16()
        return Node('case', op, read_n(r, 2 * n))
    if op == 77:
        n = r.u16()
        return Node('choose', op, read_n(r, n + 1))
    if op == 223:
        n = r.u16()
        return Node('let', op, read_n(r, n + 1))
    if op == 2000:
        a = r.u16()
        init = read_n(r, a + 1)
        b = r.u16()
        logic = read_n(r, b + 1)
        return Node('while', op, init + logic, split=a)
    if op == 220:
        n = r.u16()
        x = r.u16()
        return Node('eval', op, read_n(r, n), x=x)
    if op == 221:
        x = r.u16()
        return Node('eval', op, read_n(r, 1), x=x)
    if op in (27, 28):
        n = r.u16()
        fn = read_expr(r, depth + 1)
        return Node('call', op, list(reversed(read_n(r, n))), fn=fn)
    if op == 29:
        n = r.u16()
        fn = read_expr(r, depth + 1)
        return Node('plugin', op, list(reversed(read_n(r, n))), fn=fn)
    if op in (61, 64):
        a = read_n(r, 2)
        return Node('op', op, [a[1], a[0]])
    if op in (227, 228):
        return Node('op', op, read_n(r, arity(op)))
    if op in (74, 79, 222):
        return Node('op', op, read_n(r, 1))
    kind = nargs(op)
    if kind == 'VarArgs' or 2300 <= op < 2556:
        n = r.u16()
        return Node('varargs', op, list(reversed(read_n(r, n))))
    if kind in ('Special', 'Unknown') or op not in OPS:
        raise Bad('opcode %d (%s) not valid in Polish code at %d' % (op, OPS.get(op, {}).get('source_name', '?'), at))
    return Node('op', op, read_n(r, arity(op)))


def encode_polish(n):
    k = n.kind
    if k == 'empty':
        return b'\x00'
    if k == 'num':
        return bytes([n.op]) + n.num.encode()
    if k == 'text':
        return b'\x13' + n.text.encode()
    if k == 'symbol':
        return b'\x0b' + n.text.encode() + bytes([n.flag])
    if k == 'field':
        return n.field.encode()
    if k == 'key':
        return b'\x14' + bytes([len(n.key)]) + n.key
    if k == 'plugin_id':
        return b'\x15' + n.plugin + n.fid.to_bytes(2, 'big')
    if k == 'var':
        return b'\x18' + n.index.to_bytes(2, 'big')
    if k == 'svar':
        return b'\x1a' + n.text.encode(with_style=False)
    if k == 'get':
        return bytes([0x9b, n.sel])
    enc = encode_polish
    if k == 'list':
        return b'\x19' + len(n.args).to_bytes(2, 'big') + b''.join(enc(a) for a in reversed(n.args))
    if k == 'if':
        return b'\x4b' + b''.join(enc(a) for a in n.args)
    if k == 'case':
        return b'\x4c' + (len(n.args) // 2).to_bytes(2, 'big') + b''.join(enc(a) for a in n.args)
    if k == 'choose':
        return b'\x4d' + (len(n.args) - 1).to_bytes(2, 'big') + b''.join(enc(a) for a in n.args)
    if k == 'let':
        return b'\xdf' + (len(n.args) - 1).to_bytes(2, 'big') + b''.join(enc(a) for a in n.args)
    if k == 'while':
        a = n.split
        init, logic = n.args[:a + 1], n.args[a + 1:]
        return (b'\xf8\x00' + a.to_bytes(2, 'big') + b''.join(enc(x) for x in init)
                + (len(logic) - 1).to_bytes(2, 'big') + b''.join(enc(x) for x in logic))
    if k == 'eval':
        if n.op == 220:
            head = b'\xdc' + len(n.args).to_bytes(2, 'big') + n.x.to_bytes(2, 'big')
        else:
            head = b'\xdd' + n.x.to_bytes(2, 'big')
        return head + b''.join(enc(a) for a in n.args)
    if k in ('call', 'plugin'):
        return (bytes([n.op]) + len(n.args).to_bytes(2, 'big') + enc(n.fn)
                + b''.join(enc(a) for a in reversed(n.args)))
    if k == 'varargs':
        return write_opcode(n.op) + len(n.args).to_bytes(2, 'big') + b''.join(enc(a) for a in reversed(n.args))
    if k == 'op':
        args = n.args
        if n.op in (61, 64):
            args = [args[1], args[0]]
        return write_opcode(n.op) + b''.join(enc(a) for a in args)
    raise Bad('cannot encode %s' % k)


# --------------------------------------------------------------------------
# Formatted stream: tokens in source order

class Tok:
    def __init__(self, kind, op, **kw):
        self.kind, self.op = kind, op
        self.__dict__.update(kw)

    def __repr__(self):
        return 'Tok(%s,%s)' % (self.kind, self.op)


def decode_formatted(data):
    r = Reader(data)
    toks = []
    while not r.eof():
        toks.append(read_token(r))
    return toks


def read_token(r):
    op = read_opcode(r)
    if op in (12, 13, 14):          # whitespace, /* */ comment, // comment
        inner = read_opcode(r)
        if inner != 19:
            raise Bad('layout token %d not followed by text' % op)
        return Tok('layout', op, text=read_text(r))
    if op in (16, 17):
        num = read_number(r)
        return Tok('num', op, num=num, fmt=r.u16())
    if op == 19:
        return Tok('text', op, text=read_text(r))
    if op == 11:
        t = read_text(r)
        return Tok('symbol', op, text=t, flag=r.u8())
    if op in (22, 23):
        return Tok('field', op, field=read_field(r, op))
    if op == 20:
        return Tok('key', op, key=read_key(r))
    if op == 21:
        return Tok('plugin_id', op, plugin=r.take(4), fn=r.u16())
    if op == 24:
        return Tok('var', op, index=r.u16())
    if op == 26:
        return Tok('svar', op, text=read_text(r, with_style=False))
    if op == 156:
        return Tok('getsel', op, sel=r.u8())
    if op == 27:
        inner = read_token(r)
        if inner.kind != 'key':
            raise Bad('function call token without key')
        return Tok('call', op, key=inner.key)
    if op == 29:
        count = r.u16()
        inner = read_token(r)
        if inner.kind != 'plugin_id':
            raise Bad('plugin token without plugin id')
        return Tok('plugin', op, count=count, plugin=inner.plugin, fn=inner.fn)
    return Tok('op', op)


def encode_token(t):
    k = t.kind
    if k == 'layout':
        return bytes([t.op, 0x13]) + t.text.encode()
    if k == 'num':
        return bytes([t.op]) + t.num.encode() + t.fmt.to_bytes(2, 'big')
    if k == 'text':
        return b'\x13' + t.text.encode()
    if k == 'symbol':
        return b'\x0b' + t.text.encode() + bytes([t.flag])
    if k == 'field':
        return t.field.encode()
    if k == 'key':
        return b'\x14' + bytes([len(t.key)]) + t.key
    if k == 'plugin_id':
        return b'\x15' + t.plugin + t.fn.to_bytes(2, 'big')
    if k == 'var':
        return b'\x18' + t.index.to_bytes(2, 'big')
    if k == 'svar':
        return b'\x1a' + t.text.encode(with_style=False)
    if k == 'getsel':
        return bytes([0x9c, t.sel])
    if k == 'call':
        return b'\x1b\x14' + bytes([len(t.key)]) + t.key
    if k == 'plugin':
        return b'\x1d' + t.count.to_bytes(2, 'big') + b'\x15' + t.plugin + t.fn.to_bytes(2, 'big')
    return write_opcode(t.op)


# --------------------------------------------------------------------------
# Names for printing

class Names:
    def __init__(self):
        self.tos = {}      # (file, to id) -> (name, base table)
        self.fields = {}   # (file, table, field) -> name
        self.cfs = {}      # (file, id) -> (name, params)

    def add(self, row):
        kind, f = row[0], row[1]
        if kind == 'to':
            self.tos[(f, int(row[2]))] = (unescape(row[4]), int(row[3]) if row[3] != '-' else None)
        elif kind == 'field':
            self.fields[(f, int(row[2]), int(row[3]))] = unescape(row[4])
        elif kind == 'cf':
            self.cfs[(f, int(row[2]))] = (unescape(row[3]), unescape(row[4]))


def unescape(s):
    out, i = [], 0
    while i < len(s):
        if s[i] == '\\' and i + 1 < len(s):
            out.append({'t': '\t', 'n': '\n', 'r': '\r', '\\': '\\'}.get(s[i + 1], s[i + 1]))
            i += 2
        else:
            out.append(s[i])
            i += 1
    return ''.join(out)


def plugin_name(plugin, fn):
    if (plugin, fn) in BUILTIN:
        return BUILTIN[(plugin, fn)]
    return '<plugin %s:%d>' % (plugin.decode('latin-1'), fn)


def quote_text(s):
    """A text literal as FileMaker displays it (Text::MakeLiteral)."""
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'


class Printer:
    def __init__(self, names=None, file=None, params=()):
        self.names, self.file = names, file
        self.params = list(params)

    def field_text(self, fld):
        to_id, field_id = key_id(fld.to), key_id(fld.field)
        to_name, table = ('<TO %s>' % fld.to.hex(), None)
        if self.names and (self.file, to_id) in self.names.tos:
            to_name, table = self.names.tos[(self.file, to_id)]
        fname = '<field %s>' % fld.field.hex()
        if self.names and table is not None and (self.file, table, field_id) in self.names.fields:
            fname = self.names.fields[(self.file, table, field_id)]
        s = (to_name + '::' + fname) if fld.flags & 4 else fname
        if fld.rep != 1:
            s += '[%d]' % fld.rep
        return s

    def func_name(self, op):
        row = OPS.get(op)
        if row and row['source_name']:
            return row['source_name']
        return '<op %d>' % op

    def cf_name(self, key):
        fid = key_id(key)
        if self.names and (self.file, fid) in self.names.cfs:
            return self.names.cfs[(self.file, fid)][0]
        return '<function %s>' % key.hex()

    # formatted stream -> display text
    def tokens(self, toks):
        out = []
        for t in toks:
            k = t.kind
            if k == 'layout':
                if t.op == 12:
                    out.append(t.text.text)
                elif t.op == 13:
                    out.append('/*' + t.text.text + '*/')
                else:
                    out.append('//' + t.text.text)
            elif k == 'num':
                out.append(t.num.text())
            elif k == 'text':
                out.append(quote_text(t.text.text))
            elif k in ('symbol', 'svar'):
                out.append(t.text.text)
            elif k == 'field':
                out.append(self.field_text(t.field))
            elif k == 'getsel':
                out.append(GET.get(t.sel, '<selector %d>' % t.sel))
            elif k == 'call':
                out.append(self.cf_name(t.key))
            elif k == 'plugin':
                out.append(plugin_name(t.plugin, t.fn))
            elif k == 'var':
                out.append('<var %d>' % t.index)
            elif t.op in PUNCT:
                out.append(PUNCT[t.op])
            elif t.op in BINARY:
                out.append(BINARY[t.op][0])
            elif t.op == 44:
                out.append('-')
            else:
                out.append(self.func_name(t.op))
        return ''.join(out)

    # Polish tree -> source text
    def expr(self, n, prec=0, scope=None):
        if scope is None:
            scope = list(self.params)
        k = n.kind
        e = lambda x, p=0: self.expr(x, p, scope)
        if k == 'empty':
            return ''
        if k == 'num':
            return n.num.text()
        if k == 'text':
            return quote_text(n.text.text)
        if k in ('symbol', 'svar'):
            return n.text.text
        if k == 'field':
            return self.field_text(n.field)
        if k == 'var':
            return scope[n.index] if n.index < len(scope) and scope[n.index] else '_v%d' % n.index
        if k == 'get':
            return 'Get ( %s )' % GET.get(n.sel, '<selector %d>' % n.sel)
        if k == 'key':
            return self.cf_name(n.key)
        if k == 'list':
            return '[ ' + ' ; '.join(e(a) for a in n.args) + ' ]'
        if k == 'let':
            return self.let_text(n, scope)
        if k == 'while':
            return self.while_text(n, scope)
        if k == 'call':
            return self.cf_name(n.fn.key) + ' ( ' + ' ; '.join(e(a) for a in n.args) + ' )'
        if k == 'plugin':
            name = plugin_name(n.fn.plugin, n.fn.fid)
            return name + (' ( ' + ' ; '.join(e(a) for a in n.args) + ' )' if n.args else '')
        if k == 'op' and n.op in BINARY:
            sym, p = BINARY[n.op]
            s = e(n.args[0], p) + ' ' + sym + ' ' + e(n.args[1], p + 1)
            return '( ' + s + ' )' if p < prec else s
        if k == 'op' and n.op in UNARY:
            s = UNARY[n.op] + e(n.args[0], 8)
            return '( ' + s + ' )' if prec > 8 else s
        if k == 'op' and n.op == 228:
            return e(n.args[1], 9) + '[' + e(n.args[0]) + ']'
        name = self.func_name(n.op)
        if k == 'op' and not n.args:
            return name
        return name + ' ( ' + ' ; '.join(e(a) for a in n.args) + ' )'

    def bindings(self, values, scope):
        """Let/While bindings. Every value takes the next local slot; a
        SetVariable(value; rep; target) reassigns `target` (a $variable or a
        name declared earlier) and leaves its own slot unnamed."""
        parts = []
        for v in values:
            if v.kind == 'op' and v.op == 227:
                value, rep, target = v.args
                t = self.expr(target, 0, scope)
                if not (rep.kind == 'num' and rep.num.text() == '1'):
                    t += '[' + self.expr(rep, 0, scope) + ']'
                parts.append(t + ' = ' + self.expr(value, 0, scope))
                scope.append(None)
            else:
                name = '_v%d' % len(scope)
                parts.append(name + ' = ' + self.expr(v, 0, scope))
                scope.append(name)
        return parts

    def let_text(self, n, scope):
        scope = list(scope)
        parts = self.bindings(n.args[:-1], scope)
        return 'Let ( [ ' + ' ; '.join(parts) + ' ] ; ' + self.expr(n.args[-1], 0, scope) + ' )'

    def while_text(self, n, scope):
        scope = list(scope)
        a = n.split
        init = self.bindings(n.args[:a], scope)
        cond = self.expr(n.args[a], 0, scope)
        logic = self.bindings(n.args[a + 1:-1], scope)
        res = self.expr(n.args[-1], 0, scope)
        return 'While ( [ %s ] ; %s ; [ %s ] ; %s )' % (' ; '.join(init), cond, ' ; '.join(logic), res)


# --------------------------------------------------------------------------
# Compiler: formatted tokens (source order) -> Polish tree

FIELD_ARG_KINDS = ('UnaryField', 'BinaryField')
# Aggregate functions whose direct field arguments are stored as field-type
# references (0x17): Average Count Max Min StDev StDevP Sum List Variance VarianceP.
AGGREGATES = {127, 128, 129, 130, 131, 132, 133, 135, 160, 161}
# Variable-argument functions whose first argument is a field reference:
# Lookup, LookupNext (FileMaker Server engine, compile mode).
FIELD_FIRST_VARARGS = {224, 225}


class Compiler:
    """Parses the token stream FileMaker stored for the source text and builds
    the expression tree DBCalcParse would produce. Names in the formatted
    stream are already resolved to ids, so no catalog lookups are needed."""

    def __init__(self, toks, params=(), resolve=None):
        self.resolve = resolve      # name -> Field in the context table, or None
        self.t = [x for x in toks if x.kind != 'layout']
        self.i = 0
        self.scope = list(params)   # local names, index = position
        self.in_plugin = 0          # inside plugin arguments fields stay 0x16

    def peek(self, k=0):
        return self.t[self.i + k] if self.i + k < len(self.t) else None

    def next(self):
        t = self.peek()
        if t is None:
            raise Bad('unexpected end of formula')
        self.i += 1
        return t

    def is_op(self, t, *ops):
        return t is not None and t.kind == 'op' and t.op in ops

    def expect(self, op):
        t = self.next()
        if not self.is_op(t, op):
            raise Bad('expected %s, got %r' % (PUNCT.get(op, op), t))

    def compile(self):
        if not self.t:
            return Node('empty', 0)     # a formula that is only comments
        n = self.expr()
        if self.peek() is not None:
            raise Bad('tokens left after expression: %r' % self.peek())
        mark_tail(n)
        return n

    def expr(self, level=1):
        if level > 7:
            return self.unary()
        left = self.expr(level + 1)
        while True:
            t = self.peek()
            if t is not None and t.kind == 'op' and t.op in BINARY and BINARY[t.op][1] == level:
                self.next()
                right = self.expr(level + 1)
                left = Node('op', t.op, [left, right])
            else:
                return left

    def unary(self):
        t = self.peek()
        if t is not None and t.kind == 'op' and t.op in UNARY_TOKEN:
            self.next()
            return Node('op', UNARY_TOKEN[t.op], [self.unary()])
        return self.postfix(self.primary())

    def postfix(self, n):
        # name[rep]: variable repetition (GetVarRep) or field repetition
        while self.is_op(self.peek(), 6) and n.kind in ('svar', 'var', 'field'):
            self.next()
            rep = self.expr()
            self.expect(7)
            if n.kind == 'field':        # field[n] -> GetRepetition ( field ; n )
                n = Node('op', 137, [self.field_arg(n), rep])
            else:                        # $var[n] -> GetVarRep
                n = Node('op', 228, [rep, n])
        return n

    def args(self):
        """( a ; b ; ... ) -> list; `( )` gives no arguments."""
        self.expect(4)
        out = []
        if self.is_op(self.peek(), 5):
            self.next()
            return out
        while True:
            out.append(self.expr())
            t = self.next()
            if self.is_op(t, 5):
                return out
            if not self.is_op(t, 3):
                raise Bad('expected ; or ) in arguments, got %r' % t)
            if self.is_op(self.peek(), 5):
                self.next()              # a trailing separator before ) is ignored
                return out

    def field_arg(self, n):
        """An argument in a field-reference position (aggregates, field-type
        parameters, field[rep]). A direct field becomes a FieldRef (0x17);
        the request passes to the results of If/Case/Choose, the body of Let,
        the result of While, and sets mNeedFieldRef on Evaluate/GetField."""
        if self.in_plugin:
            return n
        if n.kind == 'field' and n.op == 22:
            f = n.field
            return Node('field', 23, field=Field(23, f.flags, f.to, f.field, f.rep))
        if n.kind == 'if':
            n.args[1] = self.field_arg(n.args[1])
            n.args[2] = self.field_arg(n.args[2])
        elif n.kind == 'case':
            n.args[1::2] = [self.field_arg(a) for a in n.args[1::2]]
        elif n.kind == 'choose':
            n.args[1:] = [self.field_arg(a) for a in n.args[1:]]
        elif n.kind in ('let', 'while'):
            n.args[-1] = self.field_arg(n.args[-1])
        elif n.kind == 'eval':
            n.x = 1
        return n

    def primary(self):
        t = self.next()
        k = t.kind
        if k == 'num':
            return Node('num', 16, num=t.num)
        if k == 'text':
            text = t.text
            if '\ue004' in text.text:
                # DBDataArg::WriteAsPolish stores U+E004 as a space
                value = text.text.replace('\ue004', ' ')
                count, scsu = scsu_encode(value)
                text = Text(value, count, scsu, text.style)
            return Node('text', 19, text=text)
        if k == 'field':
            return Node('field', t.op, field=t.field)
        if k == 'svar':
            return Node('svar', 26, text=t.text)
        if k == 'symbol':
            name = t.text.text.lower()      # names match case-insensitively
            for idx in range(len(self.scope) - 1, -1, -1):
                if self.scope[idx] is not None and self.scope[idx].lower() == name:
                    return Node('var', 24, index=idx)
            field = self.resolve(t.text.text) if self.resolve else None
            if field is not None:           # a field of the context table
                return Node('field', 22, field=field)
            return Node('symbol', 11, text=t.text, flag=t.flag)
        if k == 'var':
            return Node('var', 24, index=t.index)
        if k == 'call':        # a function without parameters is written without ( )
            args = self.args() if self.is_op(self.peek(), 4) else []
            return Node('call', 27, args, fn=Node('key', 20, key=t.key))
        if k == 'plugin':
            self.in_plugin += 1
            args = self.args() if self.is_op(self.peek(), 4) else []
            self.in_plugin -= 1
            return Node('plugin', 29, args, fn=Node('plugin_id', 21, plugin=t.plugin, fid=t.fn))
        if k != 'op':
            raise Bad('unexpected token %r' % t)
        op = t.op
        if op == 4:
            n = self.expr()
            self.expect(5)
            return n
        if op == 6:                                   # [ a ; b ] list
            items = [self.expr()]
            while self.is_op(self.peek(), 3):
                self.next()
                items.append(self.expr())
            self.expect(7)
            return Node('list', 25, items)
        if op == 36:
            return Node('op', 36, [])
        if op == 155:                                 # Get ( selector )
            self.expect(4)
            s = self.next()
            if s.kind != 'getsel':
                raise Bad('Get without selector')
            self.expect(5)
            return Node('get', 155, sel=s.sel)
        if op == 75:
            a = self.args()
            while len(a) < 3:
                a.append(Node('empty', 0))
            return Node('if', 75, a)
        if op == 76:
            a = self.args()
            if len(a) % 2:
                a.insert(len(a) - 1, one())   # default result: test is the number 1
            return Node('case', 76, a)
        if op == 77:
            return Node('choose', 77, self.args())
        if op == 223:
            return self.let()
        if op == 2000:
            return self.loop()
        if op in (220, 221):
            a = self.args()
            if op == 220 and len(a) == 2 and a[1].kind == 'list':
                a = [a[0]] + a[1].args    # Evaluate ( t ; [ f1 ; f2 ] ): one argument per trigger field
            return Node('eval', op, a, x=0)
        if op in (61, 64):
            return Node('op', op, self.args())
        kind = nargs(op)
        if kind == 'NoArgs' and not self.is_op(self.peek(), 4):
            return Node('op', op, [])
        a = self.args()
        if kind in FIELD_ARG_KINDS:
            a[0] = self.field_arg(a[0])
        if kind == 'VarArgs' or 2300 <= op < 2556:
            if op in AGGREGATES:
                a = [self.field_arg(x) for x in a]
            elif op in FIELD_FIRST_VARARGS:
                a[0] = self.field_arg(a[0])
            return Node('varargs', op, a)
        return Node('op', op, a)

    def binding_list(self, declared=None):
        """[ name = value ; ... ] or a single name = value. Every binding
        takes the next local slot. The first binding of a name in the list
        declares it in that slot; a repeated name, and a $variable, compile to
        SetVariable(value; repetition; target) and leave their slot unused."""
        bracket = self.is_op(self.peek(), 6)
        if bracket:
            self.next()
        out = []
        declared = {} if declared is None else declared
        while True:
            name_tok = self.next()
            rep = None
            if self.is_op(self.peek(), 6):
                self.next()
                rep = self.expr()
                self.expect(7)
            self.expect(68)
            value = self.expr()
            if name_tok.kind == 'svar':
                target = Node('svar', 26, text=name_tok.text)
                out.append(Node('op', 227, [value, rep or one(), target]))
                self.scope.append(None)
            elif name_tok.kind == 'symbol' and name_tok.text.text.lower() in declared:
                idx = declared[name_tok.text.text.lower()]
                out.append(Node('op', 227, [value, rep or one(), Node('var', 24, index=idx)]))
                self.scope.append(None)
            elif name_tok.kind == 'symbol':
                out.append(value)
                declared[name_tok.text.text.lower()] = len(self.scope)
                self.scope.append(name_tok.text.text)
            else:
                raise Bad('bad binding name %r' % name_tok)
            if not bracket:
                self.declared = declared
                return out
            t = self.next()
            if self.is_op(t, 7):
                self.declared = declared
                return out
            if not self.is_op(t, 3):
                raise Bad('expected ; or ] in bindings')

    def let(self):
        self.expect(4)
        mark = len(self.scope)
        values = self.binding_list()
        self.expect(3)
        body = self.expr()
        self.expect(5)
        del self.scope[mark:]
        return Node('let', 223, values + [body])

    def loop(self):
        self.expect(4)
        mark = len(self.scope)
        init = self.binding_list()
        declared = self.declared
        self.expect(3)
        cond = self.expr()
        self.expect(3)
        logic = self.binding_list(dict(declared))   # logic reassigns init names
        self.expect(3)
        result = self.expr()
        self.expect(5)
        del self.scope[mark:]
        return Node('while', 2000, init + [cond] + logic + [result], split=len(init))


def mark_tail(n):
    """A custom function call whose value is the formula's value is stored as
    a tail call (0x1c). Tail positions: the root, the branches of If, the
    results of Case and Choose, and the body of Let."""
    if n.kind == 'call':
        n.op = 28
    elif n.kind == 'if':
        mark_tail(n.args[1])
        mark_tail(n.args[2])
    elif n.kind == 'case':
        for r in n.args[1::2]:
            mark_tail(r)
    elif n.kind == 'choose':
        for r in n.args[1:]:
            mark_tail(r)
    elif n.kind == 'let':
        mark_tail(n.args[-1])


def one():
    return Node('num', 16, num=Number(2, 1, 16, [1, 0, 0]))


# --------------------------------------------------------------------------
# Driver

def read_rows(paths):
    names = Names()
    calcs = []
    for p in paths:
        with open(p, encoding='utf-8') as fh:
            for line in fh:
                row = line.rstrip('\n').split('\t')
                if row[0] == 'calc':
                    calcs.append(row)
                elif row[0] in ('to', 'field', 'cf'):
                    names.add(row)
    return names, calcs


def resolver(names, file, ctx):
    """Look up an unqualified name among the fields of the context table."""
    try:
        to = bytes.fromhex(ctx)
    except ValueError:
        return None
    to_id = key_id(to)
    entry = names.tos.get((file, to_id))
    if entry is None or entry[1] is None:
        return None
    table = entry[1]

    def find(name):
        for (f, t, fid), fname in names.fields.items():
            if f == file and t == table and fname.lower() == name.lower():
                key = fid.to_bytes(max(1, (fid.bit_length() + 7) // 8), 'big')
                return Field(22, 0, to, bytes([len(key)]) + key, 1)
        return None
    return find


def cf_params(names, file, path):
    """Parameter names of a custom function body at 16/5/<id>/35."""
    parts = path.split('/')
    if parts[0] == '16' and len(parts) >= 3:
        p = names.cfs.get((file, int(parts[2])))
        if p and p[1]:
            return p[1].split(';')
    return []


def check(paths):
    load_catalogs()
    names, calcs = read_rows(paths)
    stats = dict(total=0, polish=0, polish_rt=0, fmt=0, fmt_rt=0, compiled=0, stripped=0)
    seen_ops = {}
    fails = {}

    def fail(kind, row, err):
        fails.setdefault(kind + ': ' + str(err)[:90], []).append(row)

    def walk(n):
        seen_ops[n.op] = seen_ops.get(n.op, 0) + 1
        for a in n.args:
            walk(a)
        if hasattr(n, 'fn'):
            walk(n.fn)

    for row in calcs:
        _, file, owner, path, ctx, fmt, unf = row
        stats['total'] += 1
        pol = bytes.fromhex(unf)
        tree = toks = None
        try:
            tree = decode_polish(pol)
            stats['polish'] += 1
            walk(tree)
            if encode_polish(tree) == pol:
                stats['polish_rt'] += 1
            else:
                fail('polish re-encode', row, 'differs')
        except Bad as e:
            fail('polish decode', row, e)
        if fmt == '-':
            stats['stripped'] += 1        # formula text removed (KFormula absent)
            continue
        try:
            raw = bytes.fromhex(fmt)
            toks = decode_formatted(raw)
            stats['fmt'] += 1
            if b''.join(encode_token(t) for t in toks) == raw:
                stats['fmt_rt'] += 1
            else:
                fail('formatted re-encode', row, 'differs')
        except Bad as e:
            fail('formatted decode', row, e)
        if toks is not None and tree is not None:
            try:
                comp = Compiler(toks, cf_params(names, file, path), resolver(names, file, ctx)).compile()
                if encode_polish(comp) == pol:
                    stats['compiled'] += 1
                else:
                    fail('compile mismatch', row, 'differs')
            except Bad as e:
                fail('compile', row, e)
            except RecursionError:
                fail('compile', row, 'recursion')
    print('formulas: %(total)d' % stats)
    print('polish decoded: %(polish)d, re-encoded identically: %(polish_rt)d' % stats)
    print('formatted decoded: %(fmt)d, re-encoded identically: %(fmt_rt)d, absent: %(stripped)d' % stats)
    print('formatted compiled to the stored polish: %(compiled)d' % stats)
    for k, rows in sorted(fails.items(), key=lambda kv: -len(kv[1])):
        r = rows[0]
        print('%6d %s\n       e.g. %s %s\n       fmt %s\n       unf %s' % (len(rows), k, r[1], r[3], r[5][:300], r[6][:300]))
    ops = sorted(seen_ops)
    print('opcodes seen in polish code (%d): %s' % (len(ops), ' '.join(str(o) for o in ops if o is not None)))
    return stats, fails


def show(paths, needle=None):
    load_catalogs()
    names, calcs = read_rows(paths)
    for row in calcs:
        _, file, owner, path, ctx, fmt, unf = row
        if needle and needle not in path and needle not in owner:
            continue
        params = cf_params(names, file, path)
        p = Printer(names, file, params)
        print('%s %s (%s) context %s' % (file, path, owner, ctx))
        try:
            print('  source :', p.tokens(decode_formatted(bytes.fromhex(fmt))).replace('\r', '\n          '))
        except Bad as e:
            print('  source : <%s>' % e)
        try:
            print('  polish :', p.expr(decode_polish(bytes.fromhex(unf))))
        except Bad as e:
            print('  polish : <%s>' % e)


def dump(n, indent=0):
    pad = '  ' * indent
    extra = ''
    for a in ('index', 'sel', 'x', 'split'):
        if hasattr(n, a):
            extra += ' %s=%s' % (a, getattr(n, a))
    if hasattr(n, 'text') and isinstance(getattr(n, 'text'), Text):
        extra += ' %r' % n.text.text
    if hasattr(n, 'num'):
        extra += ' ' + n.num.text()
    if hasattr(n, 'field'):
        extra += ' %s:%s rep %d flags %d' % (n.field.to.hex(), n.field.field.hex(), n.field.rep, n.field.flags)
    if hasattr(n, 'key'):
        extra += ' key ' + n.key.hex()
    out = ['%s%s %s%s' % (pad, n.kind, n.op, extra)]
    if hasattr(n, 'fn') and isinstance(n.fn, Node):
        out.append(dump(n.fn, indent + 1))
    for a in n.args:
        out.append(dump(a, indent + 1))
    return '\n'.join(out)


def diff(paths, needle):
    load_catalogs()
    names, calcs = read_rows(paths)
    for row in calcs:
        _, file, owner, path, ctx, fmt, unf = row
        if needle not in file + ' ' + path:
            continue
        stored = decode_polish(bytes.fromhex(unf))
        comp = Compiler(decode_formatted(bytes.fromhex(fmt)), cf_params(names, file, path)).compile()
        print(file, path)
        print(Printer(names, file, cf_params(names, file, path)).tokens(decode_formatted(bytes.fromhex(fmt))))
        a, b = dump(stored).split('\n'), dump(comp).split('\n')
        for i in range(max(len(a), len(b))):
            x = a[i] if i < len(a) else ''
            y = b[i] if i < len(b) else ''
            print(('   ' if x == y else '>> ') + x.ljust(60) + ' | ' + y)
        print(encode_polish(comp).hex())
        print(unf)


def main(argv):
    if len(argv) >= 3 and argv[1] == 'check':
        check(argv[2:])
    elif len(argv) >= 3 and argv[1] == 'print':
        show([argv[2]], argv[3] if len(argv) > 3 else None)
    elif len(argv) >= 4 and argv[1] == 'diff':
        diff([argv[2]], argv[3])
    elif len(argv) >= 3 and argv[1] == 'hex':
        load_catalogs()
        p = Printer()
        try:
            print(p.expr(decode_polish(bytes.fromhex(argv[2]))))
            if len(argv) > 3:
                print(p.tokens(decode_formatted(bytes.fromhex(argv[3]))))
        except (Bad, ValueError) as e:
            print('error: %s' % e)
            return 1
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == '__main__':
    sys.setrecursionlimit(20000)
    sys.exit(main(sys.argv))
