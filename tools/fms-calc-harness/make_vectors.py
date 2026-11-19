"""Build docs/re/catalogs/calc-value-vectors.tsv from fms-calc-harness results.

usage: python3 -I make_vectors.py <results dir> <out.tsv> [schema.tsv]

Each results file is named <battery>.contacts-<lang>.tsv and holds the lines
calceval printed: expression, error, types, result text. Comment lines (#...)
name the section of the battery that follows. Rows whose result depends on
state left by an earlier line of the same run, or on the harness context
(file name, account, missing table occurrence), are left out; rows that
depend on the clock or on the harness session are kept and flagged.
Record-context batteries (x<NN>-<name>.<file>.tsv) are evaluated in a table
occurrence and record of a sample file; their "# @ <context> <record ID>"
and "# @rep <n>" lines become context=, record= and rep= evidence. Rows of
the optional schema.tsv (same four columns: rules stated for an abstract
schema, settled by the record-context runs) are appended unchanged.
"""
import os
import re
import sys

TYPES = {0: "invalid", 1: "text", 2: "number", 3: "date", 4: "time", 5: "timestamp",
         6: "container", 7: "boolean", 8: "json", 9: "unknown"}
LOCALES = {"en": "en-US", "de": "de-DE", "fr": "fr-FR", "sv": "sv-SE", "ja": "ja-JP"}
DECLARED = {"1": "text", "2": "number", "3": "date", "4": "time", "5": "timestamp"}

# Expressions that read variables set by an earlier line of the same run.
STATEFUL = re.compile(r"^(\$a|\$r1\[2\]|\$\$g1\[2\]|Let\(x=5;x\+\$x\))$|\$x\[|^Let\(\[\$x\[")
# Sections whose results describe the harness session rather than the language.
SKIP_SECTIONS = {"misc"}
SESSION = re.compile(r"Get\((SystemLanguage|CalculationRepetitionNumber)\)")
CLOCK = re.compile(r'^GetAsDate\("1/2(/\d{1,2})?"\)$|^GetAsNumber\(GetAsDate\("1/2/30"\)\)$|^Get\(CurrentDate\)')


def context_rows(path, battery, file):
    """Rows of a record-context battery, with the context in the evidence."""
    rows, ctx, record, rep = [], "", "", 0
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        m = re.match(r"^# @ (\w+)(?: (\d+))? records=\d+", line)
        if m:
            ctx, record = m.group(1), m.group(2) or ""
            continue
        m = re.match(r"^# @rep (\d+)", line)
        if m:
            rep = int(m.group(1))
            continue
        parts = line.split("\t")
        if not line or line.startswith("#") or len(parts) < 4:
            continue
        expr, err, types, text = parts[0], parts[1], parts[2], parts[3]
        native = int(types.split("/")[1])
        ev = ["established:run fms26.0.3.309", f"file={file}", f"context={ctx}"]
        if record:
            ev.append(f"record={record}")
        if rep:
            ev.append(f"rep={rep}")
        ev += [f"type={TYPES.get(native, native)}", f"battery={battery}"]
        if err == "7":
            ev.append("aborts-calculation")
        rows.append((expr, text, err, " ".join(ev)))
    return rows


def main():
    src, out = sys.argv[1], sys.argv[2]
    rows, seen = [], set()
    for name in sorted(os.listdir(src)):
        m = re.match(r"(\d\d-[\w-]+)\.contacts-(\w+)\.tsv$", name)
        x = re.match(r"(x\d\d-[\w-]+)\.([\w-]+)\.tsv$", name)
        if x:
            rows.extend(context_rows(os.path.join(src, name), x.group(1), x.group(2)))
            continue
        if not m:
            continue
        battery, lang = m.group(1), m.group(2)
        section = ""
        for line in open(os.path.join(src, name), encoding="utf-8"):
            line = line.rstrip("\n")
            if not line:
                continue
            if line.startswith("#"):
                section = line.lstrip("# ").strip()
                continue
            parts = line.split("\t")
            if len(parts) < 4:
                continue
            expr, err, types, text = parts[0], parts[1], parts[2], parts[3]
            if section in SKIP_SECTIONS or STATEFUL.search(expr):
                continue
            declared = None
            dm = re.match(r"^!(\d) (.*)$", expr)
            if dm:
                declared, expr = DECLARED.get(dm.group(1), dm.group(1)), dm.group(2)
            key = (expr, declared, lang)
            if key in seen:
                continue
            seen.add(key)
            native = int(types.split("/")[1])
            ev = [f"established:run fms26.0.3.309", f"file={LOCALES.get(lang, lang)}",
                  f"type={TYPES.get(native, native)}", f"battery={battery}"]
            if declared:
                ev.append(f"declared-result={declared}")
            if CLOCK.search(expr):
                ev.append("clock=2026-10-28")
            if SESSION.search(expr):
                ev.append("env=fms-harness-session")
            if err == "7":
                ev.append("aborts-calculation")
            rows.append((expr, text, err, " ".join(ev)))
    if len(sys.argv) > 3:
        for line in open(sys.argv[3], encoding="utf-8"):
            line = line.rstrip("\n")
            if line and not line.startswith("#") and not line.startswith("expression\t"):
                rows.append(tuple(line.split("\t")))
    with open(out, "w", encoding="utf-8") as f:
        f.write("expression\texpected_result\texpected_error\tevidence\n")
        for r in rows:
            f.write("\t".join(r) + "\n")
    print(f"{len(rows)} vectors -> {out}")


if __name__ == "__main__":
    main()
