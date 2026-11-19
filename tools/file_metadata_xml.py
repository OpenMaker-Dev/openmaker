#!/usr/bin/env python3
"""Compare the file-metadata decoder with FileMaker's own Save as XML.

    cargo run --release -p om-hbam --example file_metadata -- <dir>/*.fmp12 > decoded.tsv
    python3 -I tools/file_metadata_xml.py decoded.tsv <xml dir> [--verbose]

`decoded.tsv` is the output of crates/om-hbam/examples/file_metadata.rs;
`<xml dir>` holds `FMDeveloperTool --saveAsXML` exports (FMSaveAsXML 2.3)
named like the files (`x.fmp12` -> `x.xml`). For every file with an export
the tool compares, record by record (docs/re/file-metadata.md):

- catalog ids and names: base tables, occurrences, relationships, layouts,
  scripts, value lists, custom functions, accounts, fields;
- relationships: occurrences, predicates (operator, fields), cascade
  create/delete per side, sorted sides and their sort fields;
- summary fields: operation, repetitions, restart, summarized and
  additional fields;
- value lists: source, custom values, fields, show/sort options, related
  start occurrence, re-sort language;
- file triggers and options: the Metadata ScriptTriggers, default layout,
  use-default-fields;
- layout and object triggers: per layout, the multiset of (event, script,
  modes, has parameter) for the layout itself and for its objects.

Prints per-check match counts and the mismatches. Read-only; the inputs are
untrusted data and are only parsed.
"""
import collections
import os
import sys
import xml.etree.ElementTree as ET

OPS = {"Equal": 0, "NotEqual": 1, "Less": 2, "LessOrEqual": 3, "Greater": 4,
       "GreaterOrEqual": 5, "CartesianProduct": 6}
SUMMARY = {  # XML operation -> (kind, option bit 0x01, running bit 0x04)
    "Total": (1, 0, 0), "RunningTotal": (1, 0, 4), "Average": (2, 0, 0),
    "WeightedAverage": (2, 1, 0), "Count": (3, 0, 0), "RunningCount": (3, 0, 4),
    "Minimum": (4, 0, 0), "Maximum": (5, 0, 0), "StdDeviation": (6, 0, 0),
    "StdDeviationByPopulation": (6, 1, 0), "Fractional": (7, 0, 0),
    "FractionalSubtotal": (7, 1, 0), "List": (9, 0, 0),
}


def unescape(text):
    """Undo the decoder's escapes; XML parsing turns CR into LF, so do the same."""
    out, i = [], 0
    while i < len(text):
        c = text[i]
        if c == "\\" and i + 1 < len(text):
            out.append({"t": "\t", "r": "\n", "n": "\n", "\\": "\\"}.get(text[i + 1], text[i + 1]))
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def norm(text):
    return (text or "").replace("\r\n", "\n").replace("\r", "\n")


def fields_of(row):
    out = {}
    for part in row:
        if "=" in part:
            k, v = part.split("=", 1)
            out[k] = v
    return out


def load_decoded(path):
    data = collections.defaultdict(lambda: collections.defaultdict(list))
    for line in open(path, encoding="utf-8"):
        cols = line.rstrip("\n").split("\t")
        kind, file = cols[0], cols[1]
        data[file][kind].append(cols[2:])
    return data


class Report:
    def __init__(self, verbose):
        self.ok = collections.Counter()
        self.bad = collections.Counter()
        self.notes = []
        self.verbose = verbose

    def check(self, name, file, expected, actual, what=""):
        if expected == actual:
            self.ok[name] += 1
        else:
            self.bad[name] += 1
            self.notes.append(f"{name}\t{file}\t{what}\txml={expected!r}\tdecoded={actual!r}")


def ref_id(elem, tag):
    r = elem.find(tag) if elem is not None else None
    return r.get("id") if r is not None else None


def compare_ids(rep, file, root, dec):
    st = root.find("Structure/AddAction")
    xml_ids = collections.defaultdict(set)

    def add(cat, elems, name_of=lambda e: e.get("name")):
        for e in elems:
            xml_ids[cat].add((e.get("id"), norm(name_of(e) or "")))

    add("table", st.iterfind("BaseTableCatalog/BaseTable"))
    add("occurrence", st.iterfind("TableOccurrenceCatalog/TableOccurrence"))
    add("relationship", st.iterfind("RelationshipCatalog/Relationship"), lambda e: "")
    add("layout", (e for e in st.iterfind("LayoutCatalog//Layout") if e.get("id")))
    add("script", st.iterfind("ScriptCatalog//Script"))
    add("script", st.iterfind("ScriptCatalog//Group"))
    add("valuelist", st.iterfind("ValueListCatalog/ValueList"))
    add("function", st.iterfind("CustomFunctionsCatalog/ObjectList/CustomFunction"))
    add("account", st.iterfind("AccountsCatalog/ObjectList/Account"),
        lambda e: (e.findtext("Authentication/AccountName") or ""))
    for fc in st.iterfind("FieldsForTables/FieldCatalog"):
        t = ref_id(fc, "BaseTableReference")
        add(f"field:{t}", fc.iterfind("ObjectList/Field"))
    got = collections.defaultdict(set)
    for row in dec["id"]:
        cat, ident = row[0], row[2]
        name = norm(unescape(row[3])) if len(row) > 3 else ""
        if cat == "account":
            name = name.replace("\ue002", "Guest")
        if cat == "relationship":
            name = ""
        got[cat].add((ident, name))
    for cat in sorted(set(xml_ids) | set(got)):
        rep.check(f"ids {cat.split(':')[0]}", file, sorted(xml_ids[cat], key=str),
                  sorted(got[cat], key=str), cat)


def sort_fields(spec_elem):
    if spec_elem is None:
        return None
    out = []
    for s in spec_elem.iterfind("SortList/Sort"):
        f = s.find("PrimaryField/FieldReference")
        to = f.find("TableOccurrenceReference") if f is not None else None
        vl = s.find("ValueListReference")
        lang = s.find("LanguageReference")
        out.append((to.get("id") if to is not None else None, f.get("id") if f is not None else None,
                    s.get("type"), vl.get("id") if vl is not None else None,
                    lang.get("id") if lang is not None else None))
    return out


def decoded_sort(text):
    flags, _, entries = text.partition("|")
    out = []
    for e in entries.split(";") if entries else []:
        parts = e.split(",")
        to, field, _rep = parts[0].split(":")
        opts = dict(p.split("=", 1) if "=" in p else (p, "1") for p in parts[1:])
        kind = "Custom" if "vl" in opts else ("Descending" if "desc" in opts else "Ascending")
        out.append((to, field, kind, opts.get("vl"), opts.get("lang")))
    return int(flags), out


def compare_relationships(rep, file, root, dec):
    st = root.find("Structure/AddAction")
    rels = {r[0]: fields_of(r[1:]) for r in dec["rel"]}
    for r in st.iterfind("RelationshipCatalog/Relationship"):
        d = rels.get(r.get("id"))
        if d is None:
            rep.check("rel present", file, r.get("id"), None)
            continue
        lt, rt = r.find("LeftTable"), r.find("RightTable")
        rep.check("rel occurrences", file,
                  (ref_id(lt, "TableOccurrenceReference"), ref_id(rt, "TableOccurrenceReference")),
                  (d["left"], d["right"]), r.get("id"))
        opt = int(d["options"], 16)
        xml_opt = ((0x20 if lt.get("cascadeCreate") == "True" else 0)
                   | (0x02 if lt.get("cascadeDelete") == "True" else 0)
                   | (0x10 if rt.get("cascadeCreate") == "True" else 0)
                   | (0x01 if rt.get("cascadeDelete") == "True" else 0)
                   | (0x80 if lt.find("SortSpecification") is not None else 0)
                   | (0x40 if rt.find("SortSpecification") is not None else 0))
        rep.check("rel options", file, xml_opt, opt, r.get("id"))
        for side, elem, key in (("left", lt, "sortLeft"), ("right", rt, "sortRight")):
            spec = elem.find("SortSpecification")
            if spec is not None:
                flags, entries = decoded_sort(d[key])
                rep.check("rel sort", file, sort_fields(spec), entries, f"{r.get('id')} {side}")
                rep.check("rel sort flags", file,
                          (spec.get("maintain") == "False", spec.get("blanksLast") == "True"),
                          (bool(flags & 1), bool(flags & 2)), f"{r.get('id')} {side}")
        preds = []
        for p in r.iterfind("JoinPredicateList/JoinPredicate"):
            preds.append((OPS.get(p.get("type"), p.get("type")),
                          ref_id(p.find("LeftField"), "FieldReference"),
                          ref_id(p.find("RightField"), "FieldReference")))
        got = [(int(op), l, rr) for _n, l, op, rr in
               (x.split(":") for x in d["predicates"].split(";") if x)]
        rep.check("rel predicates", file, preds, got, r.get("id"))


def compare_summaries(rep, file, root, dec):
    st = root.find("Structure/AddAction")
    sums = {(r[0], r[1]): fields_of(r[2:]) for r in dec["summary"]}
    for fc in st.iterfind("FieldsForTables/FieldCatalog"):
        t = ref_id(fc, "BaseTableReference")
        for f in fc.iterfind("ObjectList/Field"):
            if f.get("fieldtype") != "Summary":
                continue
            d = sums.get((t, f.get("id")))
            if d is None:
                rep.check("summary present", file, f.get("id"), None)
                continue
            info = f.find("SummaryInfo")
            kind, opt, run = SUMMARY.get(info.get("operation"), (None, 0, 0))
            flags = int(d["flags"], 16)
            rep.check("summary kind", file, kind, int(d["kind"]), f.get("name"))
            rep.check("summary flags", file,
                      (opt, run, info.get("summarizeRepetition") == "Individually",
                       info.get("restartEachGroup") == "True"),
                      (flags & 1, flags & 4, bool(flags & 2), bool(flags & 0x10)), f.get("name"))
            fr = info.find("SummaryField/FieldReference")
            rep.check("summary field", file, fr.get("id") if fr is not None else None,
                      d["field"].split(":")[1], f.get("name"))
            ar = info.find("AdditionalField/FieldReference")
            rep.check("summary additional", file, ar.get("id") if ar is not None else "0",
                      d["aux"].split(":")[1], f.get("name"))


def compare_value_lists(rep, file, root, dec):
    st = root.find("Structure/AddAction")
    vls = {r[0]: fields_of(r[1:]) for r in dec["vl"]}
    for v in st.iterfind("ValueListCatalog/ValueList"):
        d = vls.get(v.get("id"))
        if d is None:
            rep.check("vl present", file, v.get("id"), None)
            continue
        src = v.find("Source").get("value")
        got_src = {"none": "Custom", "0": "Custom", "1": "FromField", "2": "External"}.get(
            d.get("type"), d.get("type"))
        rep.check("vl source", file, src, got_src, v.get("name"))
        if src == "Custom":
            text = v.findtext("CustomValues/Text") or ""
            rep.check("vl custom", file, norm(text), norm(unescape(d.get("custom", ""))),
                      v.get("name"))
        if src == "FromField":
            opt = int(d["options"], 16)
            pf = v.find("Field/PrimaryField")
            sf = v.find("Field/SecondaryField")
            rep.check("vl primary", file, ref_id(pf, "FieldReference"), d["primary"].split(":")[1],
                      v.get("name"))
            rep.check("vl secondary", file, ref_id(sf, "FieldReference") or "0",
                      d["secondary"].split(":")[1], v.get("name"))
            want = (pf.get("show") == "False", pf.get("sort") == "False")
            rep.check("vl show/sort", file, want, (bool(opt & 4), bool(opt & 2)), v.get("name"))
            sr = v.find("Field/ShowRelated")
            rep.check("vl related", file,
                      (sr.get("value") == "True", ref_id(sr, "TableOccurrenceReference") or "0"),
                      (bool(opt & 1), d["start"]), v.get("name"))
        lang = v.find(".//LanguageReference")
        got_lang = None
        if "sort" in d:
            _flags, entries = decoded_sort(d["sort"])
            got_lang = next((e[4] for e in entries if e[4]), None)
        rep.check("vl language", file, lang.get("id") if lang is not None else None, got_lang,
                  v.get("name"))


PREF_KEY = {201: "onFirstWindowOpen", 202: "onLastWindowClose", 204: "onWindowOpen",
            205: "onWindowClose", 206: "onFileAVPlayerChange", 209: "onWindowTransaction"}


def modes(t):
    return ((1 if t.get("browseMode") == "True" else 0) | (2 if t.get("findMode") == "True" else 0)
            | (4 if t.get("previewMode") == "True" else 0))


def compare_file_options(rep, file, root, dec):
    md = root.find("Metadata/AddAction")
    if not dec["prefs"]:
        return
    p = fields_of(dec["prefs"][0])
    xml_trig = {}
    for t in md.iterfind("ScriptTriggers/ScriptTrigger"):
        xml_trig[int(t.get("id"))] = (ref_id(t, "ScriptReference"), t.get("scriptParameterFieldName", ""))
    got = {}
    for ev, key in PREF_KEY.items():
        if p.get(key, "0") != "0":
            got[ev] = (p[key], unescape(p.get("transactionField", "")) if ev == 209 else "")
    rep.check("file triggers", file, xml_trig, got)
    # The layout is kept when "Switch to layout" is off; option 0x40 is the checkbox.
    switch = bool(int(p["options"], 16) & 0x40)
    rep.check("default layout", file, ref_id(md.find("Defaults"), "LayoutReference") or "0",
              p["defaultLayout"] if switch else "0")
    udf = md.find("UseDefaultFields")
    if udf is not None:
        rep.check("use default fields", file, udf.get("enable") == "True",
                  bool(int(p["options"], 16) & 0x8000))


def trigger_key(t):
    calc = t.find("ScriptReference/Calculation")
    has_param = calc is not None and bool((calc.findtext("Text") or "").strip())
    return (t.get("id"), ref_id(t, "ScriptReference") or "0", modes(t), has_param)


def compare_layout_triggers(rep, file, root, dec):
    st = root.find("Structure/AddAction")
    xml = collections.defaultdict(collections.Counter)
    for lay in st.iter("Layout"):
        lid = lay.get("id")
        for t in lay.iterfind("ScriptTriggers/ScriptTrigger"):
            xml[(lid, "layout")][trigger_key(t)] += 1
        for obj in lay.iter("LayoutObject"):
            for t in obj.findall("ScriptTriggers/ScriptTrigger"):
                xml[(lid, "object")][trigger_key(t)] += 1
    got = collections.defaultdict(collections.Counter)
    for row in dec["trigger"]:
        lid, owner = row[0], row[1]
        f = fields_of(row[2:])
        if f.get("event") == "none":
            continue
        where = "layout" if owner == "layout" else ("tableview" if owner.startswith("tableview:") else "object")
        got[(lid, where)][(f["event"], f["script"], int(f["flags"], 16), "parameter" in f)] += 1
    for key in sorted(set(xml) | {k for k in got if k[1] != "tableview"}):
        rep.check(f"triggers {key[1]}", file, dict(xml[key]), dict(got[key]), f"layout {key[0]}")
    tv = sum(sum(c.values()) for k, c in got.items() if k[1] == "tableview")
    if tv:
        rep.ok["table view triggers (not in XML)"] += tv


def main():
    decoded, xml_dir = sys.argv[1], sys.argv[2]
    rep = Report("--verbose" in sys.argv)
    data = load_decoded(decoded)
    files = 0
    for file in sorted(data):
        xml_path = os.path.join(xml_dir, file[:-6] + ".xml") if file.endswith(".fmp12") else None
        if not xml_path or not os.path.exists(xml_path):
            continue
        root = ET.parse(xml_path).getroot()
        files += 1
        dec = data[file]
        compare_ids(rep, file, root, dec)
        compare_relationships(rep, file, root, dec)
        compare_summaries(rep, file, root, dec)
        compare_value_lists(rep, file, root, dec)
        compare_file_options(rep, file, root, dec)
        compare_layout_triggers(rep, file, root, dec)
    print(f"files compared\t{files}")
    for name in sorted(set(rep.ok) | set(rep.bad)):
        print(f"{name}\t{rep.ok[name]} equal\t{rep.bad[name]} different")
    if rep.notes:
        print("\nmismatches:")
        for n in rep.notes[: None if rep.verbose else 40]:
            print(n[:600])


if __name__ == "__main__":
    main()
