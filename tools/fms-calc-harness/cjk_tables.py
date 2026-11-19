#!/usr/bin/env python3
"""Chinese and Korean ordering tables of om-calc, measured with the engine.

Chinese (Pinyin) and Korean files order Han characters, and Korean files
also jamo and a set of compatibility characters, with tables of FileMaker's
own (calc-values.md 13, "Oracle answers (collation gaps)"). Neither table
follows public data closely enough to rebuild it: the Chinese order is
pinyin, then stroke count, then an order of FileMaker's (CLDR's pinyin
tables of 1.3 to 24 and Unihan kMandarin + kTotalStrokes all differ), and
the Korean one lists CLDR's KS X 1001 Hanja per reading and then about
20,000 more Hanja, many without a Korean reading in Unihan. So the tables
are FileMaker's observed order: this script writes a battery that sorts
every BMP character (and one character per high surrogate of U+20000 to
U+2B7FF), once in ascending and once in descending input order, and turns
the results into rank tables.

    python3 -I tools/fms-calc-harness/cjk_tables.py battery \\
        tools/fms-calc-harness/batteries/vc-cjk-order.txt
    python3 -I tools/fms-calc-harness/cjk_tables.py battery \\
        tools/fms-calc-harness/batteries/vc-cjk-stroke.txt Chinese_Stroke
    for f in zh ko; do
        tools/fms-calc-harness/runbat.sh tools/fms-calc-harness/batteries/vc-cjk-order.txt \\
            contacts-$f.fmp12 /tmp/cjk
    done
    tools/fms-calc-harness/runbat.sh tools/fms-calc-harness/batteries/vc-cjk-stroke.txt \\
        contacts-en.fmp12 /tmp/cjk
    python3 -I tools/fms-calc-harness/cjk_tables.py build /tmp/cjk crates/om-calc/data

`fixture <file>` writes the vc-cjk battery (crates/om-calc/tests/value_core.rs
checks its results in the en, ja, ko and zh Contacts files).

`SortValues` keeps the input order of values that compare equal, so a
character that comes before a lower code point in the descending run ties
with it. Each table is the run of FileMaker's groups (characters that tie)
between two anchors, numbered from 1:

- zh-han.bin (Chinese files and the locale name Chinese) and zh-stroke.bin
  (the locale name Chinese_Stroke, sorted with that name in an English
  file): after U+A4F7 (LISU LETTER OE, the last BMP letter of DUCET 5.2) up
  to U+9FA6, the first ideograph the table does not list;
- ko-hangul.bin: after U+A4F7 up to U+2F00 (KANGXI RADICAL ONE, the first
  explicit ideograph weight of DUCET), without the Hangul syllables, which
  sort by their jamo.

A supplementary character sorts after the group just before it (its key
has two weights, the second ordering it among the 1,024 characters of its
high surrogate); when that group is in the table its rank is recorded per
high surrogate.

File format (little endian): u16 range count; per range u32 first code
point and u16 length; the ranks of each range (u16, 0 = not listed); u16
supplementary count; per entry u16 high surrogate and u16 rank.

The results are runs of the engine (black box); nothing here reads
FileMaker code.
"""

import re
import struct
import sys
from pathlib import Path

LAST_LETTER = 0xA4F7
ZH_END = 0x9FA6
KO_END = 0x2F00
HIGHS = range(0xD840, 0xD86E)  # U+20000..U+2B7FF


def surrogates(cp):
    cp -= 0x10000
    return 0xD800 + (cp >> 10), 0xDC00 + (cp & 0x3FF)


def supplementary_samples():
    """Two characters per high surrogate: its first and last."""
    out = []
    for high in HIGHS:
        base = 0x10000 + ((high - 0xD800) << 10)
        out += [base, base + 0x3FF]
    return out


def char(cp):
    if cp < 0x10000:
        return f"Char ( {cp} )"
    high, low = surrogates(cp)
    return f"Char ( {low * 100000 + high} )"


def battery(path, name=None):
    # U+2028 and U+2029 separate values like a return; controls are left out.
    asc = (
        'While ( [ i = 32 ; r = "" ] ; i ≤ 40000 ; [ r = If ( i = 8232 or i = 8233 ; r ; '
        'r & Char ( i ) & ¶ ) ; i = i + 1 ] ; r ) & '
        'While ( [ i = 40001 ; r = "" ] ; i ≤ 65535 ; [ r = r & Char ( i ) & ¶ ; i = i + 1 ] ; r )'
    )
    desc = (
        'While ( [ i = 65535 ; r = "" ] ; i ≥ 40001 ; [ r = r & Char ( i ) & ¶ ; i = i - 1 ] ; r ) & '
        'While ( [ i = 40000 ; r = "" ] ; i ≥ 32 ; [ r = If ( i = 8232 or i = 8233 ; r ; '
        'r & Char ( i ) & ¶ ) ; i = i - 1 ] ; r )'
    )
    supp = supplementary_samples()
    supp_asc = " & ".join(f"{char(cp)} & ¶" for cp in supp)
    supp_desc = " & ".join(f"{char(cp)} & ¶" for cp in reversed(supp))
    locale = f' ; 1 ; "{name}"' if name else ""
    lines = [
        "# FileMaker's order of every BMP character but the controls, U+2028 and",
        "# U+2029, and of the first and last character of each high surrogate of",
        "# U+20000..U+2B7FF; ascending, then descending input (cjk_tables.py).",
        f"SortValues ( {asc} & {supp_asc}{locale} )",
        f"SortValues ( {supp_desc} & {desc}{locale} )",
    ]
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")


def unescape(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n in "rnt\\":
                out.append({"r": "\r", "n": "\n", "t": "\t", "\\": "\\"}[n])
                i += 2
                continue
            if n == "u" and re.fullmatch(r"[0-9A-F]{4}", s[i + 2 : i + 6]):
                out.append(chr(int(s[i + 2 : i + 6], 16)))
                i += 6
                continue
        out.append(c)
        i += 1
    # The harness prints supplementary characters as UTF-8; lone surrogates
    # come as escapes and stay single code units.
    return "".join(out)


def code_points(value):
    """The code point of a one-character value (a surrogate pair joined)."""
    units = value.encode("utf-16-le", "surrogatepass")
    units = [int.from_bytes(units[i : i + 2], "little") for i in range(0, len(units), 2)]
    if len(units) == 2 and 0xD800 <= units[0] < 0xDC00 <= units[1] < 0xE000:
        return 0x10000 + ((units[0] - 0xD800) << 10) + (units[1] - 0xDC00)
    assert len(units) == 1, value
    return units[0]


def load(path):
    rows = []
    for line in Path(path).read_text(encoding="utf-8", errors="surrogatepass").split("\n"):
        if line.startswith("#") or not line:
            continue
        cols = line.split("\t")
        assert cols[1] == "0", f"{path}: error {cols[1]}"
        values = [v for v in unescape(cols[3]).split("\r") if v]
        rows.append([code_points(v) for v in values])
    asc, desc = rows
    assert sorted(asc) == sorted(desc)
    return asc, desc


def groups(asc, desc):
    """FileMaker's order as groups of characters that compare equal."""
    at = {cp: i for i, cp in enumerate(desc)}
    out = [[asc[0]]]
    for a, b in zip(asc, asc[1:]):
        # Input order is ascending in one run and descending in the other.
        if a < b and at[b] < at[a]:
            out[-1].append(b)
        else:
            out.append([b])
    return out


def table(gs, end, skip=lambda cp: False):
    index = {cp: k for k, g in enumerate(gs) for cp in g}
    first, last = index[LAST_LETTER] + 1, index[end]
    ranks = {}
    rank = 0
    for k in range(first, last):
        members = [cp for cp in gs[k] if cp < 0x10000 and not skip(cp)]
        if not members:
            continue
        rank += 1
        for cp in members:
            ranks[cp] = rank
    # Supplementary characters: the rank of the group before each one.
    supp = {}
    for high in HIGHS:
        cp = 0x10000 + ((high - 0xD800) << 10)
        k = index[cp] - 1
        while k >= 0 and not any(c in ranks for c in gs[k]):
            if any(c < 0x10000 for c in gs[k]):
                break
            k -= 1
        listed = [ranks[c] for c in gs[k] if c in ranks]
        if listed and first <= k < last:
            supp[high] = listed[0]
    return ranks, supp


def write(path, ranks, supp):
    cps = sorted(ranks)
    spans = []
    for cp in cps:
        if spans and cp - spans[-1][1] <= 64:
            spans[-1][1] = cp
        else:
            spans.append([cp, cp])
    out = bytearray(struct.pack("<H", len(spans)))
    for lo, hi in spans:
        out += struct.pack("<IH", lo, hi - lo + 1)
    for lo, hi in spans:
        for cp in range(lo, hi + 1):
            out += struct.pack("<H", ranks.get(cp, 0))
    out += struct.pack("<H", len(supp))
    for high in sorted(supp):
        out += struct.pack("<HH", high, supp[high])
    Path(path).write_bytes(bytes(out))
    print(
        f"{path}: {len(cps)} characters, {max(ranks.values())} ranks, "
        f"{len(spans)} ranges, {len(supp)} high surrogates, {len(out)} bytes"
    )


def build(results, out_dir):
    results, out_dir = Path(results), Path(out_dir)
    surrogate = lambda cp: 0xD800 <= cp <= 0xDFFF  # noqa: E731
    hangul = lambda cp: surrogate(cp) or 0xAC00 <= cp <= 0xD7A3  # noqa: E731
    for results_file, name, end, skip in [
        ("vc-cjk-order.contacts-zh.tsv", "zh-han.bin", ZH_END, surrogate),
        ("vc-cjk-stroke.contacts-en.tsv", "zh-stroke.bin", ZH_END, surrogate),
        ("vc-cjk-order.contacts-ko.tsv", "ko-hangul.bin", KO_END, hangul),
    ]:
        asc, desc = load(results / results_file)
        ranks, supp = table(groups(asc, desc), end, skip)
        write(out_dir / name, ranks, supp)


def sample_pool(rng):
    """Characters the tables and FileMaker's computed weights order, and
    neighbours they meet (Latin, kana, radicals, full width forms)."""
    blocks = [
        (0x4E00, 0x9FFF, 40),
        (0x3400, 0x4DBF, 10),
        (0xF900, 0xFAFF, 6),
        (0xAC00, 0xD7A3, 16),
        (0x1100, 0x11FF, 5),
        (0x3131, 0x318E, 4),
        (0x3190, 0x32FF, 5),
        (0x3300, 0x33FF, 2),
        (0x2E80, 0x2FD5, 3),
        (0x3041, 0x30FF, 4),
        (0x31C0, 0x31E3, 1),
        (0xFF01, 0xFF5E, 3),
        (0xFF61, 0xFFDC, 3),
        (0x20000, 0x2B7FF, 6),
        (0x2B800, 0x2B81D, 1),
        (0x0041, 0x007A, 3),
        (0x0030, 0x0039, 1),
    ]
    excluded = {0x22, 0x5C, 0xFF02, 0xFF3C}  # quote and backslash forms
    pool = []
    for lo, hi, weight in blocks:
        cps = [cp for cp in range(lo, hi + 1) if cp not in excluded and not 0x5B <= cp <= 0x60]
        pool += [rng.choice(cps) for _ in range(weight * 40)]
    return pool


def fixture(path):
    """The vc-cjk battery: random lists and pairs over sample_pool (seeded,
    so the file is reproducible), then the rules found while building the
    tables."""
    import random

    rng = random.Random(20261029)
    pool = sample_pool(rng)
    lit = lambda cps: '"' + "".join(chr(c) for c in cps) + '"'  # noqa: E731
    lines = [
        "# CJK ordering (calc-values.md 13, collation gaps): random lists and pairs",
        "# drawn by cjk_tables.py fixture, then the rules behind the tables.",
    ]
    for _ in range(40):
        cps = rng.sample(pool, 120)
        lines.append(f'SortValues ( "{"¶".join(chr(c) for c in cps)}" )')
        lines.append(f'UniqueValues ( "{"¶".join(chr(c) for c in cps)}" )')
    for _ in range(250):
        a, b = rng.sample(pool, 2)
        lines.append(f"{lit([a])} < {lit([b])}")
        lines.append(f"{lit([a])} = {lit([b])}")
    for _ in range(60):
        a, b = rng.sample(pool, 2)
        lines.append(f"Max ( {lit([a])} ; {lit([b])} )")
        lines.append(f"Min ( {lit([a])} ; {lit([b])} )")
    lines += [
        "# Korean syllables: jamo plus a filler for a missing final",
        '"가" = Char ( 4352 ) & Char ( 4449 )',
        '"가" < Char ( 4352 ) & Char ( 4449 )',
        'Char ( 4352 ) & Char ( 4449 ) < "가"',
        '"각" = Char ( 4352 ) & Char ( 4449 ) & Char ( 4520 )',
        'UniqueValues ( "가¶" & Char ( 4352 ) & Char ( 4449 ) )',
        'UniqueValues ( "각¶" & Char ( 4352 ) & Char ( 4449 ) & Char ( 4520 ) )',
        'SortValues ( "가¶" & Char ( 4352 ) & Char ( 4449 ) & "a¶" & Char ( 4352 ) & Char ( 4449 ) & "ꓷ¶" '
        '& Char ( 4352 ) & Char ( 4449 ) & "！¶" & Char ( 4352 ) & Char ( 4449 ) & "ᄀ¶" '
        '& Char ( 4352 ) & Char ( 4449 ) & Char ( 4520 ) )',
        '"伽" < "각"',
        '"깋" < "ᆨ"',
        '"ᆨ" < "伽"',
        '"ㄱ" = "ᄀ"',
        '"ａ" = "a"',
        '"Ａ" = "A"',
        '"ｱ" = "ア"',
        'Max ( "가" ; "½" )',
        'Max ( "가" ; "ß" )',
        'Max ( "伽" ; "ß" )',
        'Max ( "각" ; "나" )',
        "# Japanese collisions",
        '"填" = "㐀"',
        '"暐" = "가"',
        '"甑" = Char ( 40870 )',
        '"仝" < "亜"',
        '"㊀" = "一"',
        '"⼀" = "一"',
        'UniqueValues ( "填¶㐀¶暐¶가¶一¶㊀¶⼀" )',
        'SortValues ( "填¶㐀¶暐¶가¶𠀋¶龥¶一¶㊀¶⼀" )',
        "# FileMaker's computed weights and DUCET's ideograph weights",
        'SortValues ( "⼀¶一¶가¶㐀¶𠀀¶﨎¶豈¶豈¶" & Char ( 40908 ) & "¶" & Char ( 64046 ) & "¶𫝀¶𫠝" )',
        '"豈" = "豈"',
        'Max ( "𠀋" ; "ß" )',
        'Max ( "𠀋" ; "½" )',
        'Max ( "一" ; "ß" )',
        'Max ( "가" ; "ß" )',
        "# The locale name Chinese_Stroke",
        'SortValues ( "一¶丨¶乙¶二¶十¶丁¶人¶龜¶齉¶龼" ; 1 ; "Chinese_Stroke" )',
    ]
    stroke = random.Random(20261030)
    for _ in range(10):
        cps = stroke.sample(pool, 120)
        values = "¶".join(chr(c) for c in cps)
        lines.append(f'SortValues ( "{values}" ; 1 ; "Chinese_Stroke" )')
        lines.append(f'UniqueValues ( "{values}" ; 1 ; "Chinese_Stroke" )')
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    if len(sys.argv) in (3, 4) and sys.argv[1] == "battery":
        battery(*sys.argv[2:])
    elif len(sys.argv) == 3 and sys.argv[1] == "fixture":
        fixture(sys.argv[2])
    elif len(sys.argv) == 4 and sys.argv[1] == "build":
        build(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
