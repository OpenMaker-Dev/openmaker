#!/usr/bin/env python3
"""Check Get ( FileLocaleElements ) against the stored locale record.

    python3 -I tools/file_metadata_locale_json.py decoded.tsv <file.fmp12>=<result.tsv>...

`decoded.tsv` is crates/om-hbam/examples/file_metadata.rs output for the
files; each `<result.tsv>` is a tools/fms-calc-harness result whose first
non-comment line evaluated `Get ( FileLocaleElements )` in that file. The
JSON is rebuilt from the decoded `locale` row by the rules of
docs/re/file-metadata.md 2.4 and compared field by field (`LocaleID.Name`
and `Misc.Active` are only printed: they do not come from the record).
Read-only; inputs are parsed, never executed.
"""
import json
import sys

ORDER = {0: "MDY", 1: "DMY", 2: "YMD", 3: "MYD", 4: "DYM", 5: "YDM"}
CODES = {1: "D#", 3: "D$", 4: "M#", 6: "M$", 8: "YYYY#", 13: "ReignY"}
ISO = {20: "nl", 24: "fr", 25: "de", 30: "it", 31: "ja", 34: "pt", 39: "es", 41: "sv", 45: "zh", 76: "ko"}


def unescape(text):
    out, i = [], 0
    while i < len(text):
        if text[i] == "\\" and i + 1 < len(text):
            out.append({"t": "\t", "r": "\r", "n": "\n", "\\": "\\"}.get(text[i + 1], text[i + 1]))
            i += 2
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def expected(f):
    flag = lambda name: f[name] == "1"
    char = lambda name: chr(int(f[name][2:], 16))
    lst = lambda name: [unescape(x) for x in f[name].split("|")]
    elements = [int(x) for x in f["elements"].split(",")]
    seps = lst("elementSeps")
    names, seplist = [], []
    for i in (1, 2, 3, 0):
        if elements[i] == 0:
            break
        names.append(CODES.get(elements[i], f"?{elements[i]}"))
        seplist.append(seps[i if i else 4])
    days = lst("days")
    order = ORDER[int(f["order"])]
    return {
        "APIVers": 1,
        "Currency": {"Leading": flag("currencyLeading"), "Symbol": unescape(f["currency"])},
        "Date": {
            "DMQ": {
                "1stDayOfWeek": 1 if days[0] in ("Sunday", "日曜日") else 2,
                "DaysOfWeek": {"AbbrvList": lst("shortDays"), "NameList": days},
                "Months": {"AbbrvList": lst("shortMonths"), "NameList": lst("months")},
                "Quarters": {"AbbrvList": lst("shortQuarters"), "NameList": lst("quarters")},
            },
            order: {"ElementArray": {"NameList": names, "SepList": seplist}, "MustUseLocalesSep": False},
            "DateNums": {"0d": flag("day0"), "0m": flag("month0"), "YYyy": flag("century")},
            "DateOrderID": int(f["order"]),
            "DateOrderName": order,
            "Sep": char("dateSep"),
        },
        "LocaleID": {"IDNum": int(f["type"]), "ISOLangCode": ISO.get(int(f["type"]), "en")},
        "Misc": {"Metric": flag("metric")},
        "Num": {"1000s": char("thousands"), "Decimal": char("decimal"), "Lead0": flag("num0")},
        "Text": {"SQuotLead": char("quoteOpen"), "SQuotTrail": char("quoteClose"), "Sep": char("separator")},
        "Time": {
            "12h": flag("ampm"),
            "HMS": {"0h": flag("hour0"), "24h": unescape(f["h24"]), "Seconds": False},
            "NightDay": {"12hSuffix": flag("ampmSuffix"), "amStr": unescape(f["am"]), "pmStr": unescape(f["pm"])},
            "Sep": char("timeSep"),
        },
    }


def diff(exp, got, path=""):
    out = []
    for k, v in exp.items():
        if k not in got:
            out.append(f"{path}{k}: missing")
        elif isinstance(v, dict):
            out += diff(v, got[k], f"{path}{k}.")
        elif v != got[k]:
            out.append(f"{path}{k}: record {v!r} engine {got[k]!r}")
    return out


def main():
    rows = {}
    for line in open(sys.argv[1], encoding="utf-8"):
        cols = line.rstrip("\n").split("\t")
        if cols[0] == "locale":
            rows[cols[1]] = dict(c.split("=", 1) for c in cols[2:] if "=" in c)
    ok = 0
    for pair in sys.argv[2:]:
        name, result = pair.split("=", 1)
        line = next(l for l in open(result, encoding="utf-8") if not l.startswith("#"))
        engine = json.loads(unescape(line.rstrip("\n").split("\t")[3]))
        problems = diff(expected(rows[name]), engine)
        lid = engine.get("LocaleID", {})
        print(f"{name}\t{'equal' if not problems else 'DIFFERENT'}\tName={lid.get('Name')!r}\t"
              f"IDStr={lid.get('IDStr')!r}\tLID={lid.get('LID')!r}\tActive={engine.get('Misc', {}).get('Active')}")
        for p in problems:
            print("\t" + p)
        ok += not problems
    print(f"{ok} / {len(sys.argv) - 2} equal")


if __name__ == "__main__":
    main()
