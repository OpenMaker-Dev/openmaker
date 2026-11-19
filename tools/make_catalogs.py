#!/usr/bin/env python3
"""Build docs/re/catalogs/*.csv from the DBEngine string-resource export.

Input: TSV produced by tools/dbengine_strres.py (resource_id, index, f0..f3,
backslash-escaped). Output: CSV catalogs. Pure data transformation.

Usage: python3 -I tools/make_catalogs.py <dbengine-strres.tsv> <outdir>
"""
import collections
import csv
import os
import sys


def unesc(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == "\\" and s.startswith("u", i + 1) and len(s) >= i + 6:
            out.append(chr(int(s[i + 2:i + 6], 16)))
            i += 6
            continue
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            out.append({"n": "\n", "r": "\r", "t": "\t", "\\": "\\"}.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def load(path):
    res = collections.defaultdict(list)
    for line in open(path, encoding="utf-8"):
        p = line.rstrip("\n").split("\t")
        p += [""] * (6 - len(p))
        res[int(p[0])].append((int(p[1]), *[unesc(x) for x in p[2:6]]))
    return res


def split_desc(s):
    if "|" in s:
        a, b = s.split("|", 1)
        return a, b
    return "", s


def writer(outdir, name, header):
    f = open(os.path.join(outdir, name), "w", encoding="utf-8", newline="")
    w = csv.writer(f, lineterminator="\n")
    w.writerow(header)
    return f, w


# Resource ids holding calc functions, with the inferred grouping. The
# per-arity grouping of 140-145 is an inference from the signatures.
FUNC_RES = {
    130: "base table (index presumed = function token id)",
    134: "Design functions",
    137: "legacy byte functions (obsolete, names only)",
    138: "legacy Status() selectors (obsolete, names only)",
    139: "While",
    140: "newer functions, variable or optional parameters",
    141: "newer zero-parameter functions and constants",
    142: "newer one-parameter functions",
    143: "newer two-parameter functions",
    144: "newer three-parameter functions",
    145: "newer four-parameter functions",
    156: "Location functions",
}


def main():
    src, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    r = load(src)
    fcat = {i: a for i, a, *_ in r[135]}  # function category names
    counts = {}

    def cat_name(c):
        if not c.strip().isdigit():
            return ""
        return fcat.get(int(c) - 1, "")

    # calc functions
    f, w = writer(out, "calc-functions.csv", [
        "resource_id", "index", "group", "kind", "name", "signature",
        "category_id", "category", "display_name", "description"])
    n = 0
    for rid, grp in FUNC_RES.items():
        for idx, name, sig, cat, desc in r[rid]:
            if name.startswith("#") and name.endswith("#"):
                kind = "placeholder"
            elif not name and not sig:
                kind = "empty"
            elif sig or cat:
                kind = "function"
            else:
                kind = "keyword-or-constant"
            if rid in (137, 138):
                kind = "obsolete"
            disp, d = split_desc(desc)
            w.writerow([rid, idx, grp, kind, name, sig, cat, cat_name(cat), disp, d])
            n += kind in ("function",)
    f.close()
    counts["calc-functions.csv (kind=function)"] = n

    # Get selectors
    f, w = writer(out, "get-selectors.csv", [
        "index", "selector", "signature", "category_id", "display_name", "description"])
    n = 0
    for idx, name, sig, cat, desc in r[131]:
        disp, d = split_desc(desc)
        w.writerow([idx, name, sig, cat, disp, d])
        n += bool(name)
    f.close()
    counts["get-selectors.csv (named)"] = n

    # script steps
    f, w = writer(out, "script-steps.csv", ["id", "name", "category", "description"])
    n = 0
    for idx, name, cat, desc, _ in r[132]:
        if name == desc:
            desc = ""
        w.writerow([idx, name, cat, desc])
        n += bool(name) and not name.startswith("HACK_")
    f.close()
    counts["script-steps.csv (named)"] = n

    # error codes
    f, w = writer(out, "error-codes.csv", ["code", "message"])
    for idx, code, msg, *_ in r[159]:
        w.writerow([code, msg])
    f.close()
    counts["error-codes.csv"] = len(r[159])

    # script triggers: resource 129, name/description pairs from "Unknown"
    rows = r[129]
    marker = "This is an unknown trigger, possibly from a newer version; it will not work in this version."
    start = next(i for i, (idx, a, *_) in enumerate(rows) if a == marker) - 1
    f, w = writer(out, "script-triggers.csv", ["order", "name", "description", "res129_index"])
    i, order = start, 0
    trig = []
    while i < len(rows):
        name = rows[i][1]
        if not (name == "Unknown" or name.startswith("On")):
            break
        nxt = rows[i + 1][1] if i + 1 < len(rows) else ""
        if nxt.startswith("On") or nxt == "":
            desc, step = "", 1
        else:
            desc, step = nxt, 2
        trig.append((order, name, desc, rows[i][0]))
        order += 1
        i += step
    for t in trig:
        w.writerow(t)
    f.close()
    counts["script-triggers.csv"] = len(trig)

    # small enumerations
    f, w = writer(out, "enumerations.csv", ["enumeration", "resource_id", "index", "value"])
    enums = {
        133: "script-step-category",
        135: "function-category (category_id = index + 1)",
        146: "GetLayoutObjectAttribute attribute",
        147: "GetLayoutObjectAttribute objectType value",
        148: "catalog kind",
        149: "catalog item kind",
        136: "index/sort language",
        158: "GetAVPlayerAttribute attribute",
        162: "GetSensor sensor name",
        161: "certificate status message",
    }
    n = 0
    for rid, label in enums.items():
        for idx, a, *_ in r[rid]:
            w.writerow([label, rid, idx, a])
            n += 1
    f.close()
    counts["enumerations.csv"] = n

    for k, v in counts.items():
        print(f"{k}: {v}")


if __name__ == "__main__":
    main()
