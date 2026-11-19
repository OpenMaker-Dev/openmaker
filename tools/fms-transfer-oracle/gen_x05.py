#!/usr/bin/env python3
"""Write the export battery x05: summary fields in exports.

    python3 -I gen_x05.py > batteries/x05-export-summaries.txt

Six records with a break field B, a weight W and a 3-repetition NR; every
summary operation and option of probe_schema.py (S, SRT ... SNRRI) exported
unsorted, sorted by B, by B descending and by K only, with and without
group-by fields, in tab, XML, Excel, comma and HTML; then the script
engine's own value of each summary on each record of the same found sets
and of GetSummary ( s ; s ) and GetSummary ( s ; B ) ($$vals, written to
the log).
"""
import sys

SUMS = ["S", "SRT", "SRT0", "SRC", "SRCR", "SF", "SFS", "SA", "SWA", "SMIN", "SMAX", "SSD", "SSDP", "SL", "SNR",
        "SNRI", "SNRRI"]
REPEATING = ("SNRI", "SNRRI")
REC = [  # K, B, N, W, NR
    ("1", "b", "5", "1", ("1", "2", "3")),
    ("2", "a", "1", "2", ("4", "", "6")),
    ("3", "b", "4", "3", ("", "5", "")),
    ("4", "a", "2", "1", ("7", "8", "9")),
    ("5", "c", "", "1", ("", "", "")),
    ("6", "c", "3", "2", ("10", "", "")),
]
SORTS = {"u": "Unsort Records", "s": "Sort Records [ Contacts::B ; Contacts::K ]", "k": "Sort Records [ Contacts::K ]",
         "d": "Sort Records [ Contacts::B desc ; Contacts::K ]"}


def fields(names):
    return " | ".join("Contacts::" + n for n in names)


ALL = ["K", "B", "N"] + SUMS
BY = [f"Contacts::{s} > Contacts::B" for s in SUMS]
GRP = fields(ALL) + " | " + " | ".join(BY)
NOREP = fields([n for n in ALL if n not in REPEATING])
BY_NOREP = " | ".join(f"Contacts::{s} > Contacts::B" for s in SUMS if s not in REPEATING)
EXPORTS = [  # name, sort, type, extension, fields, group-by
    ("x5_u_tab", "u", "TABS", "tab", fields(ALL), ""),
    ("x5_s_tab", "s", "TABS", "tab", fields(ALL), ""),
    ("x5_k_tab", "k", "TABS", "tab", fields(ALL), ""),
    ("x5_d_tab", "d", "TABS", "tab", fields(ALL), ""),
    ("x5_ub_tab", "u", "TABS", "tab", GRP, ""),
    ("x5_sb_tab", "s", "TABS", "tab", GRP, ""),
    ("x5_kb_tab", "k", "TABS", "tab", GRP, ""),
    ("x5_g_tab", "s", "TABS", "tab", GRP, "Contacts::B"),
    ("x5_gd_tab", "d", "TABS", "tab", GRP, "Contacts::B"),
    ("x5_gu_tab", "u", "TABS", "tab", GRP, "Contacts::B"),
    ("x5_gk_tab", "k", "TABS", "tab", GRP, "Contacts::B"),
    ("x5_g2_tab", "s", "TABS", "tab", GRP + " | " + " | ".join(f"Contacts::{s} > Contacts::K" for s in SUMS),
     "Contacts::B | Contacts::K"),
    ("x5_u_xml", "u", "XMLE", "xml", fields(ALL), ""),
    ("x5_s_xml", "s", "XMLE", "xml", fields(ALL), ""),
    ("x5_g_xml", "s", "XMLE", "xml", GRP, "Contacts::B"),
    ("x5_u_xlsx", "u", "XLXE", "xlsx", NOREP, ""),
    ("x5_s_xlsx", "s", "XLXE", "xlsx", NOREP, ""),
    ("x5_g_xlsx", "s", "XLXE", "xlsx", NOREP + " | " + BY_NOREP, "Contacts::B"),
    ("x5_rep_xlsx", "u", "XLXE", "xlsx", "Contacts::K | Contacts::SNRI | Contacts::SNRRI", ""),
    ("x5_g_coms", "s", "COMS", "csv", GRP, "Contacts::B"),
    ("x5_g_htm", "s", "HTML", "htm", GRP, "Contacts::B"),
]
VALS = " & Char ( 9 ) & ".join(["Contacts::K"] + [f"Contacts::{s}" for s in SUMS if s not in REPEATING]
                                + [f"GetRepetition ( Contacts::{s} ; {i} )" for s in REPEATING for i in (1, 2, 3)])
# GetSummary ( s ; s ) and GetSummary ( s ; B ) on each record (repetition 1)
GETS = " & Char ( 9 ) & ".join(["Contacts::K"] + [f"GetSummary ( Contacts::{s} ; Contacts::{s} )" for s in SUMS]
                                + [f"GetSummary ( Contacts::{s} ; Contacts::B )" for s in SUMS])


def main():
    out = []
    w = out.append
    w("# Export Records probes (battery x05, issue #372), written by gen_x05.py: summary\n"
      "# fields of every operation and option over six records (K B N W NR):\n"
      "# 1 b 5 1 1|2|3, 2 a 1 2 4||6, 3 b 4 3 |5|, 4 a 2 1 7|8|9, 5 c - 1 ||, 6 c 3 2 10||.\n"
      "# Paths: Get ( DocumentsPath ), or /root/Documents/ in the script harness, where\n"
      "# it is empty (fms-script-harness README, DOCUMENTS).\n")
    w("== Setup\nSet Error Capture [ On ]\nGo to Layout [ \"Contact List\" ]\nShow All Records\nLoop\n"
      "Exit Loop If [ Get ( FoundCount ) = 0 ]\nDelete Record/Request [ With dialog: Off ]\nEnd Loop")
    for k, b, n, wt, nr in REC:
        w("New Record/Request")
        for name, v in (("K", k), ("VNE", "v" + k), ("VNO", "o" + k), ("VU", "u" + k), ("B", b)):
            w(f'Set Field [ Contacts::{name} ; "{v}" ]')
        if n:
            w(f"Set Field [ Contacts::N ; {n} ]")
        w(f"Set Field [ Contacts::W ; {wt} ]")
        for i, v in enumerate(nr, 1):
            if v:
                w(f"Set Field [ Contacts::NR[{i}] ; {v} ]")
    w("Commit Records/Requests [ With dialog: Off ]\nShow All Records\n"
      'Set Variable [ $$dir ; If ( IsEmpty ( Get ( DocumentsPath ) ) ; "/root/Documents/" ; Get ( DocumentsPath ) ) ]\n'
      'Set Variable [ $$log ; "setup" & Char ( 9 ) & Get ( LastError ) & Char ( 9 ) & Get ( FoundCount ) & ¶ ]\n')
    names = []
    for name, sort, typ, ext, flds, group in EXPORTS:
        names.append(name)
        g = " ; Group: " + group if group else ""
        w(f"== {name}\nSet Error Capture [ On ]\nGo to Layout [ \"Contact List\" ]\nShow All Records\n{SORTS[sort]}\n"
          f'Set Variable [ $p0 ; $$dir & "omprobe_{name}.{ext}" ]\n'
          f'Export Records [ "file:$p0" ; Type: {typ} ; Charset: 4 ; Fields: {flds} ; With dialog: Off{g} ]\n'
          f'Set Variable [ $$log ; $$log & "{name}" & Char ( 9 ) & Get ( LastError ) & Char ( 9 ) & '
          f"Get ( FoundCount ) & ¶ ]\n")
    for kind, expr in (("vals", VALS), ("gets", GETS)):
        for s in SORTS:
            names.append(f"x5_{kind}_{s}")
            w(f"== x5_{kind}_{s}\nSet Error Capture [ On ]\nGo to Layout [ \"Contact List\" ]\nShow All Records\n"
              f'{SORTS[s]}\nSet Variable [ $$vals ; $$vals & "{kind} {s}" & ¶ ]\n'
              "Go to Record/Request/Page [ First ]\nLoop\n"
              f"Set Variable [ $$vals ; $$vals & {expr} & ¶ ]\n"
              "Go to Record/Request/Page [ Next ; Exit after last: On ]\nEnd Loop\n")
    w("== Run\nPerform Script [ \"Setup\" ]")
    for n in names:
        w(f'Perform Script [ "{n}" ]')
    w("Go to Layout [ \"Contact List\" ]\nShow All Records\nShow Omitted Only\nNew Record/Request\n"
      'Set Field [ Contacts::VNE ; "log" ]\nSet Field [ Contacts::VNO ; "log" ]\nSet Field [ Contacts::VU ; "log" ]\n'
      'Set Field [ Contacts::LOG ; $$log & "vals" & ¶ & $$vals ]\nCommit Records/Requests [ With dialog: Off ]\n'
      'Set Variable [ $p0 ; $$dir & "omprobe_log.tab" ]\n'
      'Export Records [ "file:$p0" ; Type: TABS ; Charset: 4 ; Fields: Contacts::LOG ; With dialog: Off ]\n')
    w("--- run\nRun\t\t$$log\t$$vals")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
