"""Build docs/re/catalogs/execute-sql-vectors.tsv from the fms-sql results.

usage: python3 -I make_sql_vectors.py <results dir> <out.tsv>

Each results file is <battery>.<file>.tsv as tools/fms-calc-harness/runbat.sh
writes it: expression, error, types, result text (calceval escapes \\r \\n
\\t \\\\ \\uXXXX). Comment lines name the section that follows; "# @ <context>
[<record ID>] records=<n>" lines move the run into a table occurrence and
record, which becomes the row's context. Every row is kept; rows whose
result depends on the clock, on the harness session (account name, the name
of the private file copy, the application name) or on an uninitialized
engine value are marked in the evidence column with depends=... so a test
can skip or relax them.
"""
import os
import re
import sys

FILES = {
    "fms-sample-en": "en_FMServer_Sample.fmp12 (FileMaker Server 26 sample, en)",
    "contacts-en": "Contacts/en.fmp12 (FileMaker Pro 26 template, en-US)",
    "contacts-de": "Contacts/de.fmp12 (FileMaker Pro 26 template, de-DE)",
    "contacts-sv": "Contacts/sv.fmp12 (FileMaker Pro 26 template, sv-SE)",
    "contacts-ja": "Contacts/ja.fmp12 (FileMaker Pro 26 template, ja-JP)",
    "mbs-audit": "MBS example Audit/Audit.fmp12",
    "mbs-email": "MBS example CURL/Email/Build and send Email.fmp12",
    "mbs-events": "MBS example Audit/Event Management with Audit/Event Management with Audit.fmp12",
    "mbs-atb": "MBS example Third Party/Apple TouchBar/AppleTouchBar.fmp12",
    "mbs-swissqr": "MBS example Barcode/Swiss QR-Code for invoices/Swiss QR-Code for invoices ISO 20022.fmp12",
}
TYPES = {0: "empty", 1: "text", 2: "number", 3: "date", 4: "time", 5: "timestamp"}
CLOCK = re.compile(r"CURRENT_DATE|CURRENT_TIME|CURRENT_TIMESTAMP|CURDATE|CURTIME|CURTIMESTAMP|TODAY|"
                   r"\b(DATE|TIME|TIMESTAMP)\(\)")
SESSION = re.compile(r"\bUSER\b|USERNAME|CURRENT_USER|Get \( ApplicationVersion \)")


def rows(path, battery, file):
    out, section, context = [], "", "-"
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if line.startswith("#"):
            m = re.match(r"^# @ (\w+)(?: (\d+))? records=\d+", line)
            if m:
                context = m.group(1) + (" record " + m.group(2) if m.group(2) else " no record")
            elif line.startswith("# ---"):
                section = line[5:].strip()
            continue
        f = line.split("\t")
        if len(f) < 4:
            continue
        expr, err, types, result = f[0], f[1], f[2], f[3]
        cached = TYPES.get(int(types.split("/")[0]), types)
        evidence = "established:run fms26.0.3.309 battery=%s type=%s" % (battery, cached)
        if section:
            evidence += " section=" + section.replace(" ", "_")
        depends = []
        if err == "0" and CLOCK.search(expr) and not re.search(r"CURRENT_DATE - CURRENT_DATE|CURDATE - CURRENT_DATE|TODAY - CURRENT_DATE|"
                                                 r"DATE\(\) - CURRENT_DATE|CURTIMESTAMP - CURRENT_TIMESTAMP|CURTIME - CURTIME|"
                                                 r"LENGTH\(STRVAL\(CURRENT", expr):
            depends.append("clock")
        if err == "0" and (SESSION.search(expr) or "|work|" in result or result.endswith("|work")
                           or "|work\\r" in result):
            depends.append("session")
        if re.search(r"FQL-\d+/", result):
            depends.append("uninitialized")
        if depends:
            evidence += " depends=" + ",".join(depends)
        out.append((expr, FILES.get(file, file), context, result, err, evidence))
    return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    allrows = []
    for name in sorted(os.listdir(src)):
        m = re.match(r"^(q\w+(?:-[\w]+)*)\.([\w-]+)\.tsv$", name)
        if not m:
            continue
        allrows += rows(os.path.join(src, name), m.group(1), m.group(2))
    with open(dst, "w", encoding="utf-8") as out:
        out.write("expression\tfile\tcontext\texpected_result\texpected_error\tevidence\n")
        for r in allrows:
            out.write("\t".join(r) + "\n")
    print("%d rows" % len(allrows))


if __name__ == "__main__":
    main()
