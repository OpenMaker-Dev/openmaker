#!/usr/bin/env python3
"""Check decoded script-step specs against FileMaker's own Save as XML.

    python3 -I tools/script_specs_xml.py <emit.tsv> <xml dir> [--show N]

`emit.tsv` is the output of
`cargo run -p om-scripts --example stored_specs -- --emit <files>`: one line
per stored spec (file, script id, step index, step id, kind, canonical text).
`xml dir` holds `FMDeveloperTool --saveAsXML` exports of the same files,
named `<file without .fmp12>.xml`. For every step the XML describes with a
sort order, find requests, import options or export options, this builds the
same canonical text from the XML elements (script-steps.md 11 gives the
correspondence) and compares the two; other kinds are counted as not
compared.
Files are untrusted data: parsed with ElementTree, nothing is executed.
"""
import collections
import os
import sys
import xml.etree.ElementTree as ET


def ref_id(el):
    """An element's id; FileMaker writes 0 for an empty stored key."""
    return el.get("id") if el is not None and el.get("id") != "0" else "-"


def field(el):
    """FieldReference -> 'TO:F' (ids as FileMaker writes them)."""
    to = el.find("TableOccurrenceReference")
    return f"{ref_id(to)}:{ref_id(el)}"


def sort_text(spec):
    s = f"maintain={spec.get('maintain') == 'True'} blanksLast={spec.get('blanksLast') == 'True'}".replace(
        "True", "true").replace("False", "false")
    for sort in spec.iter("Sort"):
        prim = sort.find("PrimaryField/FieldReference")
        s += f" | {sort.get('type')} {field(prim)}"
        vl = sort.find("ValueListReference")
        if vl is not None:
            s += f" vl={vl.get('id')}"
        summ = sort.find("SummaryField/FieldReference")
        if summ is not None:
            s += f" summary={field(summ)}"
        lang = sort.find("LanguageReference")
        if lang is not None:
            s += f" lang={lang.get('id')}"
    return s


def find_text(rset):
    parts = []
    for req in rset.findall("FindRequest"):
        crit = []
        for c in req.findall("find"):
            f = c.find("FieldReference")
            text = c.get("criteria") or ""
            crit.append(f" {field(f)}[{f.get('repetition') or 1}] {rust_debug(text)}")
        parts.append(req.get("action") + "".join(sorted(crit)))
    return " | ".join(parts)


def rust_debug(text):
    """The text as Rust's {:?} prints a String."""
    out = '"'
    for ch in text:
        if ch == '"':
            out += '\\"'
        elif ch == "\\":
            out += "\\\\"
        elif ch == "\n":
            out += "\\n"
        elif ch == "\r":
            out += "\\r"
        elif ch == "\t":
            out += "\\t"
        elif ord(ch) < 0x20 or ord(ch) == 0x7f:
            out += "\\u{%x}" % ord(ch)
        else:
            out += ch
    return out + '"'


IMPORT_FLAGS = [("splitRepetitions", 0x01), ("doAutoEntry", 0x02), ("matchFieldNames", 0x04),
                ("copyContainersAsIs", 0x10), ("keepOriginalData", 0x20)]


def import_text(imp, n):
    """FMImportOptions as stored_specs renders it, for its first n entries
    (FileMaker's XML lists every field of the target table; the stored order
    has one entry per source column)."""
    opts = imp.find("Options")
    flags = sum(bit for name, bit in IMPORT_FLAGS if opts is not None and opts.get(name) == "True")
    flags |= 0x08 if imp.get("addasnew") == "True" else 0
    table = imp.find("Target/TableOccurrenceReference")
    action = imp.find("action")
    charset = imp.find("CharacterSet")
    s = (f"table={ref_id(table)} action={action.get('value') if action is not None else '?'} "
         f"flags={flags:#04x} charset={charset.get('value') if charset is not None else '?'} n={n}")
    for i, m in enumerate(imp.iter("Map")):
        if i == n:
            break
        s += f" {m.get('index')}:{ref_id(m.find('FieldReference'))}={m.get('kind')}"
    return s


def export_text(exp, tail):
    """FMExportOptions: the first two tail bytes are ExpFormat and the
    character set; XML lists fields and group-by fields with their ids."""
    opts = exp.find("Options")
    s = f"tail={int(opts.get('Formatting') == 'True'):02x}{int(opts.get('value')):02x}{tail[4:]}"
    for g in exp.findall("Order[@type='Group']/Group"):
        s += f" | group {field(g.find('FieldReference'))}[1] 1"
    for f in exp.findall("Order[@type='Field']/Field"):
        s += f" | field {field(f.find('FieldReference'))}[1]"
    return s


def xml_specs(path):
    """(script id, step index) -> {kind: canonical or element}."""
    out = {}
    root = ET.parse(path).getroot()
    for script in root.iter("Script"):
        ref = script.find("ScriptReference")
        steps = script.find("ObjectList")
        if ref is None or steps is None:
            continue
        for step in steps.findall("Step"):
            key = (ref.get("id"), step.get("index"))
            d = out.setdefault(key, {"step": step})
            spec = step.find(".//SortSpecification")
            if spec is not None and spec.find("SortList") is not None:
                d["sort"] = sort_text(spec)
            rset = step.find(".//FindRequestSet")
            if rset is not None:
                d["find"] = find_text(rset)
            imp = step.find(".//ImportField")
            if imp is not None:
                d["import"] = imp
            exp = step.find(".//Export")
            if exp is not None and exp.find("Options") is not None:
                d["export"] = exp
    return out


def main():
    emit, xml_dir = sys.argv[1], sys.argv[2]
    show = int(sys.argv[sys.argv.index("--show") + 1]) if "--show" in sys.argv else 3
    rows = collections.defaultdict(list)
    for line in open(emit, encoding="utf-8"):
        f, sid, idx, step, kind, text = line.rstrip("\n").split("\t", 5)
        rows[f].append((sid, idx, step, kind, text))
    stats = collections.Counter()
    shown = collections.Counter()
    for f, items in sorted(rows.items()):
        path = os.path.join(xml_dir, f[: -len(".fmp12")] + ".xml")
        if not os.path.exists(path):
            stats["no xml"] += len(items)
            continue
        specs = xml_specs(path)
        for sid, idx, step, kind, text in items:
            d = specs.get((sid, idx))
            if d is None:
                stats[f"{kind}: step not in xml"] += 1
                continue
            if kind not in ("sort", "find", "import", "export"):
                stats[f"{kind}: not compared"] += 1
                continue
            if kind not in d:
                stats[f"{kind}: xml has no {kind} (step {step})"] += 1
                if shown[kind + "-missing"] < show:
                    shown[kind + "-missing"] += 1
                    print(f"MISSING {kind} {f} script {sid} #{idx}: {text}")
                continue
            if kind == "import":
                d = dict(d, import_=import_text(d["import"], int(text.split(" n=")[1].split()[0])))
                kind = "import_"
            elif kind == "export":
                d = dict(d, export_=export_text(d["export"], text.split()[0][len("tail="):]))
                kind = "export_"
            if d[kind] == text:
                stats[f"{kind}: equal"] += 1
            else:
                stats[f"{kind}: DIFFERENT"] += 1
                if shown[kind] < show:
                    shown[kind] += 1
                    print(f"DIFF {kind} {f} script {sid} #{idx}\n  bin {text}\n  xml {d[kind]}")
    for k, v in sorted(stats.items()):
        print(f"{v:6} {k}")


if __name__ == "__main__":
    main()
