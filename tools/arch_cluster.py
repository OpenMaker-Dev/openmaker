#!/usr/bin/env python3
"""Cluster demangled exported symbols into namespaces and classes.

Input: a demangled symbol listing (`address type name`, as produced by
`llvm-cxxfilt -_` over `llvm-nm` output). Output: JSON with counts per
top-level namespace, per class scope, and per C-symbol prefix.

Usage: python3 -I tools/arch_cluster.py <dsym.txt> <out.json>
"""
import collections
import json
import re
import sys

SPECIAL = [
    "vtable for ", "typeinfo for ", "typeinfo name for ", "VTT for ",
    "construction vtable for ", "non-virtual thunk to ", "virtual thunk to ",
    "guard variable for ", "thread-local wrapper routine for ",
    "thread-local initialization routine for ", "reference temporary #0 for ",
]


def split_top(s, sep="::"):
    """Split on sep at template/paren depth 0."""
    parts, depth, cur, i = [], 0, [], 0
    while i < len(s):
        c = s[i]
        if c in "<(":
            depth += 1
        elif c in ">)":
            depth = max(0, depth - 1)
        if depth == 0 and s.startswith(sep, i):
            parts.append("".join(cur))
            cur = []
            i += len(sep)
            continue
        cur.append(c)
        i += 1
    parts.append("".join(cur))
    return parts


def qualified_name(name):
    """Return (kind, qualified name without parameter list)."""
    kind = "func"
    for p in SPECIAL:
        if name.startswith(p):
            kind = p.strip()
            name = name[len(p):]
            break
    name = name.replace("(anonymous namespace)", "{anon}")
    # strip parameter list: first '(' at template depth 0
    depth = 0
    for i, c in enumerate(name):
        if c == "<":
            depth += 1
        elif c == ">":
            depth -= 1
        elif c == "(" and depth == 0:
            # operator() special case
            if name[max(0, i - 8):i].endswith("operator"):
                continue
            name = name[:i]
            break
    # drop return type for template functions ("void foo<int>")
    if " " in name.split("<")[0]:
        name = name.split("<")[0].split(" ")[-1] + ("<" + name.split("<", 1)[1] if "<" in name else "")
    return kind, name.strip()


def strip_tpl(s):
    out, depth = [], 0
    for c in s:
        if c == "<":
            depth += 1
            continue
        if c == ">":
            depth -= 1
            continue
        if depth == 0:
            out.append(c)
    return "".join(out)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    ns = collections.Counter()
    cls = collections.Counter()
    cprefix = collections.Counter()
    cnames = []
    for line in open(src, encoding="utf-8", errors="replace"):
        parts = line.rstrip("\n").split(" ", 2)
        if len(parts) < 3:
            continue
        name = parts[2]
        if "::" not in name and not any(name.startswith(p) for p in SPECIAL):
            n = name.lstrip("_")
            m = re.match(r"([A-Za-z]+[0-9]*?)(?:_|(?=[A-Z][a-z]))", n)
            cprefix[(m.group(1) if m else n[:6])] += 1
            cnames.append(name)
            continue
        kind, q = qualified_name(name)
        comps = [strip_tpl(c) for c in split_top(q)]
        comps = [c for c in comps if c]
        if not comps:
            continue
        ns[comps[0]] += 1
        if kind == "func":
            scope = "::".join(comps[:-1]) if len(comps) > 1 else "<global>"
        else:
            scope = "::".join(comps)
        cls[scope] += 1
    json.dump({
        "namespaces": ns.most_common(),
        "classes": cls.most_common(),
        "c_prefixes": cprefix.most_common(),
        "c_symbol_count": len(cnames),
    }, open(dst, "w"), indent=0)


if __name__ == "__main__":
    main()
