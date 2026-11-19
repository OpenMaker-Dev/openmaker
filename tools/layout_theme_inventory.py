#!/usr/bin/env python3
"""Inventory FileMaker theme CSS and manifest files (read-only, untrusted input).

Usage: python3 -I tools/layout_theme_inventory.py <Themes dir> <out.json>

Parses each *.css with a minimal tokenizer (comments, selector, block of
declarations) and records the selector grammar (element type, named style
class, state pseudo-class, sub-element/part selector), property names, value
shapes, and -fm-* functions. Also collects manifest.xml element paths.
Never executes or imports anything from the input tree.
"""
import collections
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

COMMENT = re.compile(r"/\*.*?\*/", re.S)
RULE = re.compile(r"([^{}]+)\{([^{}]*)\}", re.S)
SEL = re.compile(
    r"^(?P<elem>[A-Za-z_][A-Za-z0-9_]*)?"
    r"(?P<cls>(?:\.[A-Za-z0-9_\-]+)*)"
    r"(?P<pseudo>(?::[A-Za-z_\-]+)*)"
    r"(?:\s+(?P<sub>.+))?$"
)
FUNC = re.compile(r"(-fm-[a-z\-]+|rgba?|url|linear-gradient|radial-gradient|-webkit-[a-z\-]+)\(")


def value_shape(v):
    v = v.strip()
    if re.fullmatch(r"#[0-9a-fA-F]{3,8}", v):
        return "hexcolor"
    if re.fullmatch(r"[0-9a-fA-F]{6}", v):
        return "barehex"
    if re.fullmatch(r"-?[0-9.]+(pt|px|em|%)?", v):
        m = re.fullmatch(r"-?[0-9.]+(pt|px|em|%)?", v)
        return "number" + (m.group(1) or "")
    if v.startswith("rgba(") or v.startswith("rgb("):
        return "rgba()"
    m = FUNC.match(v)
    if m:
        return m.group(1) + "()"
    if " " in v:
        return "list"
    return "keyword:" + v


def main(root, out):
    stats = {
        "css_files": 0,
        "rules": 0,
        "elements": collections.Counter(),
        "classes": collections.Counter(),
        "pseudos": collections.Counter(),
        "subs": collections.Counter(),
        "elem_subs": collections.defaultdict(collections.Counter),
        "elem_pseudos": collections.defaultdict(collections.Counter),
        "props": collections.Counter(),
        "prop_shapes": collections.defaultdict(collections.Counter),
        "keyword_values": collections.defaultdict(collections.Counter),
        "functions": collections.Counter(),
        "units": collections.Counter(),
        "unparsed_selectors": collections.Counter(),
        "manifest_paths": collections.Counter(),
        "manifest_values": collections.defaultdict(collections.Counter),
        "themes": {},
    }
    for dirpath, _dirs, files in os.walk(root):
        for fn in sorted(files):
            p = os.path.join(dirpath, fn)
            rel = os.path.relpath(p, root)
            if fn.endswith(".css"):
                stats["css_files"] += 1
                with open(p, "rb") as fh:
                    data = fh.read().decode("utf-8-sig", "replace")
                data = COMMENT.sub("", data)
                nrules = 0
                for m in RULE.finditer(data):
                    nrules += 1
                    sel = " ".join(m.group(1).split())
                    sm = SEL.match(sel)
                    if not sm:
                        stats["unparsed_selectors"][sel] += 1
                        continue
                    elem = sm.group("elem") or "(none)"
                    stats["elements"][elem] += 1
                    for c in filter(None, sm.group("cls").split(".")):
                        stats["classes"][c] += 1
                    for ps in filter(None, sm.group("pseudo").split(":")):
                        stats["pseudos"][ps] += 1
                        stats["elem_pseudos"][elem][ps] += 1
                    sub = sm.group("sub") or "(none)"
                    stats["subs"][sub] += 1
                    stats["elem_subs"][elem][sub] += 1
                    for decl in m.group(2).split(";"):
                        if ":" not in decl:
                            continue
                        k, v = decl.split(":", 1)
                        k = k.strip()
                        v = v.strip()
                        stats["props"][k] += 1
                        shape = value_shape(v)
                        stats["prop_shapes"][k][shape] += 1
                        if shape.startswith("keyword:"):
                            stats["keyword_values"][k][v] += 1
                        for f in FUNC.findall(v):
                            stats["functions"][f] += 1
                        for u in re.findall(r"[0-9.](pt|px|em|%)", v):
                            stats["units"][u] += 1
                stats["rules"] += nrules
                if "partials" not in rel and "_" not in fn.replace(".css", "").split("/")[-1][-3:]:
                    stats["themes"].setdefault(os.path.dirname(rel), {})["css_rules"] = nrules
            elif fn.startswith("manifest") and fn.endswith(".xml"):
                try:
                    tree = ET.parse(p)
                except ET.ParseError:
                    stats["manifest_paths"]["(parse error)"] += 1
                    continue

                def walk(node, path):
                    path = path + "/" + node.tag
                    stats["manifest_paths"][path] += 1
                    t = (node.text or "").strip()
                    if t and fn == "manifest.xml" and len(node) == 0:
                        stats["manifest_values"][path][t[:60]] += 1
                    for ch in node:
                        walk(ch, path)

                walk(tree.getroot(), "")
                if fn == "manifest.xml" and "partials" not in rel:
                    r = tree.getroot()
                    stats["themes"].setdefault(os.path.dirname(rel), {}).update(
                        {k: (r.findtext(k) or "") for k in ("id", "name", "group", "version", "platform", "cssfile")}
                    )

    def ser(o):
        if isinstance(o, collections.Counter):
            return dict(o.most_common())
        if isinstance(o, dict):
            return {k: ser(v) for k, v in o.items()}
        return o

    with open(out, "w") as fh:
        json.dump(ser(stats), fh, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
