"""Build docs/re/catalogs/execute-sql-errors.tsv.

usage: python3 -I make_error_catalog.py <errors dump> <execute-sql-vectors.tsv> <out.tsv>

The errors dump is the output of `run.sh errors` (fqlgrammar.cpp):
"fql <TAB> n <TAB> text" for FQL::NonLocalizedStrings(n), the message of
FQLnnnn, and "error <TAB> code <TAB> text" for FQL::ErrorToString(code),
the " (code): message" part ExecuteSQLe shows for an engine error. Message
placeholders (U+007F followed by a digit in the binary) are written ^0, ^1.
Engine codes whose text is the default "FileMaker error" are left out. The
vectors give, for each FQL code seen in a run, the calculation error
(EvaluationError) it came with.
"""
import re
import sys


def main():
    dump, vectors, dst = sys.argv[1:4]
    seen = {}
    for line in open(vectors, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        if len(f) < 5:
            continue
        m = re.search(r"\? ERROR: FQL(\d{4})/", f[3])
        if m:
            seen.setdefault(int(m.group(1)), set()).add(f[4])
    out = ["kind\tcode\ttext\tevaluation_error\tevidence"]
    for line in open(dump, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        if len(f) < 3:
            continue
        kind, code, text = f[0], int(f[1]), f[2]
        if kind == "fql":
            if code == 0:
                out.append("fql-position\t-\t%s\t-\tFQL::NonLocalizedStrings(0): (line:column) after FQLnnnn/" % text)
                continue
            errs = ",".join(sorted(seen.get(code, ()))) or "-"
            ev = "established:FQL::NonLocalizedStrings(%d)" % code
            if code in seen:
                ev += "; observed in execute-sql-vectors.tsv"
            out.append("fql\tFQL%04d\t%s\t%s\t%s" % (code, text, errs, ev))
        elif kind == "error":
            if text.endswith("): FileMaker error"):
                continue
            msg = re.sub(r"^ \(-?\d+\): ", "", text)
            out.append("engine\t%d\t%s\t%d\testablished:FQL::ErrorToString(%d) gives \" (%d): \" + text" % (code, msg, code, code, code))
    with open(dst, "w", encoding="utf-8") as o:
        o.write("\n".join(out) + "\n")
    print("%d rows" % (len(out) - 1))


if __name__ == "__main__":
    main()
