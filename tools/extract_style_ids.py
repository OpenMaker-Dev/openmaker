#!/usr/bin/env python3
"""Extract FMStyle constant tables from Ghidra decompiler text of a static
initializer (our own scratch output, not shipped data).

Usage: python3 -I tools/extract_style_ids.py <decompiled .c> [label]

Prints "id<TAB>symbol<TAB>nearest preceding string literal<TAB>class" for every
"(&FMStyle::kX, <int>," constructor call found in a static initializer. The
optional label argument is ignored (kept for call-site readability).
"""
import re
import sys


def main():
    text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
    flat = re.sub(r"\s+", " ", text)
    pat = re.compile(r"&FMStyle::(k[A-Za-z0-9_]+),\s*(0x[0-9a-fA-F]+|-?[0-9]+)\s*,")
    lit = re.compile(r'"((?:[^"\\]|\\.)*)"')
    cls = re.compile(r"FMStyle::([A-Za-z]+Property)::\1|\(\(?([A-Za-z]+Property) \*\)&FMStyle::k")
    rows = []
    last_end = 0
    for m in pat.finditer(flat):
        window = flat[max(last_end, m.start() - 1500):m.start()]
        lits = lit.findall(window)
        name = lits[-1] if lits else ""
        before = flat[max(0, m.start() - 120):m.start() + 5]
        cm = None
        for cm in cls.finditer(before):
            pass
        klass = (cm.group(1) or cm.group(2)) if cm else ""
        rows.append((int(m.group(2), 0), m.group(1), name, klass))
        last_end = m.end()
    seen = set()
    for r in sorted(rows):
        if r[:2] in seen:
            continue
        seen.add(r[:2])
        print("\t".join(str(x) for x in r))


if __name__ == "__main__":
    main()
