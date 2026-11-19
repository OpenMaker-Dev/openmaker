#!/usr/bin/env python3
"""Character tables of om-calc's text functions, measured with the engine.

FileMaker's case mapping, word classes and GetAsCSS escapes come from its own
per-character tables (Unicode 5.2-era, BMP only). This script writes
batteries that probe every BMP code point through tools/fms-calc-harness and
turns the results into crates/om-calc/src/text/tables.rs.

    python3 -I tools/fms-calc-harness/text_tables.py batteries /tmp/scan
    for b in /tmp/scan/*.txt; do
        tools/fms-calc-harness/runbat.sh "$b" contacts-en.fmp12 /tmp/scan/results
    done
    python3 -I tools/fms-calc-harness/text_tables.py build /tmp/scan/results \
        crates/om-calc/src/text/tables.rs

The results are runs of the engine (black box); nothing here reads FileMaker
code. The word scan was also run with contacts-de/fr/sv/ja: only the digit
joiners of the `.` and `/` families and (fr) the apostrophes change with the
locale, which text/words.rs decides from the file's locale.
"""

import re
import sys
from pathlib import Path

CODES = [c for c in range(1, 0x10000) if not 0xD800 <= c <= 0xDFFF]

# Word classes (`words::Class` in text/words.rs).
SEP, LETTER, DIGIT, MARK, OTHER = 0, 1, 2, 3, 4
HAN, HIRAGANA, KATAKANA, PROLONGED, KANA_MARK = 5, 6, 7, 8, 9
MID_LETTER, MID_NUM, MID_NUM_LETTER = 10, 11, 12
MID_NUM_DOT, MID_NUM_SLASH, MID_NUM_LETTER_DOT, APOSTROPHE = 13, 14, 15, 16
IDEO = -1  # before the ideographic groups are told apart
# Representatives of the ideographic groups: Han, hiragana, katakana, the
# prolonged sound mark and the spacing voiced mark.
IDEO_REPS = [0x4E00, 0x3041, 0x30A1, 0x30FC, 0x309B]

# Joiners whose digit role follows the locale's separators (contacts-de and
# contacts-sv drop the `/` family, contacts-fr and contacts-sv the `.` one).
DOT_MID_NUM = {0x3002, 0xFF61}
SLASH_MID_NUM = {0x002F, 0x2044, 0x2215, 0xFF0F}
DOT_MID_NUM_LETTER = {0x002E, 0x06D4, 0xFE52, 0xFF0E}
# Letter joiners that separate in contacts-fr.
APOSTROPHES = {0x0027, 0x2018, 0x2019, 0xFF07}


def char(c):
    return "Char ( %d )" % c


def batteries(out):
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    with open(out / "scan-case.txt", "w") as f:
        for c in CODES:
            f.write("Code ( Upper ( %s ) ) & \"|\" & Code ( Lower ( %s ) )\n" % (char(c), char(c)))
    with open(out / "scan-word.txt", "w") as f:
        for c in CODES:
            C = char(c)
            parts = [
                'WordCount ( "x" & %s & "y" )' % C,
                'WordCount ( "1" & %s & "2" )' % C,
                "WordCount ( %s )" % C,
                'WordCount ( "x" & %s & "1" )' % C,
                'WordCount ( "1" & %s & "x" )' % C,
                "WordCount ( %s & \"'\" & %s )" % (C, C),
                'WordCount ( %s & "," & %s )' % (C, C),
                "WordCount ( \"a\" & %s & \"'b\" )" % C,
                'WordCount ( "1" & %s & ",2" )' % C,
            ]
            f.write(' & "|" & '.join(parts) + "\n")
    with open(out / "scan-ideo.txt", "w") as f:
        for c in CODES:
            C = char(c)
            parts = []
            for rep in IDEO_REPS:
                parts.append("WordCount ( %s & %s )" % (char(rep), C))
                parts.append("WordCount ( %s & %s )" % (C, char(rep)))
            parts.append("WordCount ( %s & %s )" % (C, C))
            f.write(' & "|" & '.join(parts) + "\n")
    with open(out / "scan-css.txt", "w") as f:
        for c in CODES:
            f.write("GetAsCSS ( %s )\n" % char(c))


def results(path):
    for line in open(path, encoding="utf-8", errors="surrogateescape"):
        cols = line.rstrip("\n").split("\t")
        # The scanned character is the last `Char` of the line.
        m = re.findall(r"Char \( (\d+) \)", cols[0])
        if m:
            yield int(m[-1]), cols[3]


def units(code):
    """The UTF-16 units of a `Code` result, first unit first."""
    n, out = int(code), []
    while n:
        out.append(n % 100000)
        n //= 100000
    return out


def word_class(c, f):
    xy, d12, alone, x1, d1x, ll, dd, ext_l, ext_d = f
    if c in DOT_MID_NUM:
        return MID_NUM_DOT
    if c in SLASH_MID_NUM:
        return MID_NUM_SLASH
    if c in DOT_MID_NUM_LETTER:
        return MID_NUM_LETTER_DOT
    if c in APOSTROPHES:
        return APOSTROPHE
    if (xy, d12, alone, x1, d1x) == ("3", "2", "1", "2", "3"):
        return KANA_MARK
    if xy == "3":
        return IDEO
    if alone == "0":
        if (xy, d12) == ("1", "1"):
            return MID_NUM_LETTER
        if (xy, d12) == ("1", "2"):
            return MID_LETTER
        if (xy, d12) == ("2", "1"):
            return MID_NUM
        return SEP
    if ll == "1":
        return LETTER
    if dd == "1":
        return DIGIT
    if (ext_l, ext_d) == ("1", "1"):
        return MARK
    return OTHER


def ideo_class(f):
    """The group of an ideographic character from its joins with the
    representatives (IDEO_REPS, each before and after it, then itself)."""
    han, _, hira_before, hira_after, kata_before, kata_after = f[:6]
    if han == "1":
        return HAN
    if (hira_before, hira_after, kata_before, kata_after) == ("1", "1", "1", "1"):
        return KANA_MARK
    if (hira_before, hira_after) == ("1", "1"):
        return HIRAGANA
    if (kata_before, kata_after) == ("1", "1"):
        return KATAKANA
    if (hira_before, hira_after) == ("1", "2"):
        return PROLONGED
    return HAN


def build(results_dir, out):
    d = Path(results_dir)
    upper, lower = [], []
    for c, v in results(d / "scan-case.contacts-en.tsv"):
        u, l = v.split("|")
        if units(u) != [c]:
            upper.append((c, units(u)[0]))
        if units(l) != [c]:
            lower.append((c, units(l)[0]))
    classes = {}
    for c, v in results(d / "scan-word.contacts-en.tsv"):
        classes[c] = word_class(c, v.split("|"))
    for c, v in results(d / "scan-ideo.contacts-en.tsv"):
        if classes[c] in (IDEO, KANA_MARK):
            classes[c] = ideo_class(v.split("|"))
    ranges = []
    for c in CODES:
        k = classes[c]
        if not ranges or ranges[-1][1] != k:
            ranges.append((c, k))
    entities = []
    for c, v in results(d / "scan-css.contacts-en.tsv"):
        m = re.fullmatch(r'<span style="" >(&[A-Za-z0-9]+;)</span>', v)
        if m and m.group(1) != "&#%d;" % c:
            entities.append((c, m.group(1)))

    def rows(items, fmt, per_line):
        lines = []
        for i in range(0, len(items), per_line):
            lines.append("    " + " ".join(fmt(x) for x in items[i : i + per_line]))
        return "\n".join(lines)

    pair = lambda p: "(0x%04X, 0x%04X)," % p
    with open(out, "w") as f:
        f.write(
            "//! Character tables measured with FileMaker Server's engine (every BMP\n"
            "//! code point through `tools/fms-calc-harness`); generated by\n"
            "//! `tools/fms-calc-harness/text_tables.py`, do not edit.\n\n"
        )
        f.write("/// (code unit, its `Upper`) for every BMP unit `Upper` changes.\n")
        f.write("#[rustfmt::skip]\npub(super) const UPPER: &[(u16, u16)] = &[\n")
        f.write(rows(upper, pair, 6) + "\n];\n\n")
        f.write("/// (code unit, its `Lower`) for every BMP unit `Lower` changes.\n")
        f.write("#[rustfmt::skip]\npub(super) const LOWER: &[(u16, u16)] = &[\n")
        f.write(rows(lower, pair, 6) + "\n];\n\n")
        f.write(
            "/// Word classes (`words::Class` discriminants) as (first code unit,\n"
            "/// class) ranges, each running to the next entry.\n"
        )
        f.write("#[rustfmt::skip]\npub(super) const WORD_CLASSES: &[(u16, u8)] = &[\n")
        f.write(rows(ranges, lambda r: "(0x%04X, %d)," % r, 8) + "\n];\n\n")
        f.write("/// The named entities `GetAsCSS` writes, by code unit.\n")
        f.write("#[rustfmt::skip]\npub(super) const HTML_ENTITIES: &[(u16, &str)] = &[\n")
        f.write(rows(entities, lambda e: '(0x%04X, "%s"),' % e, 6) + "\n];\n")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "batteries":
        batteries(sys.argv[2])
    elif len(sys.argv) == 4 and sys.argv[1] == "build":
        build(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
