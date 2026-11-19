#!/usr/bin/env python3
"""Print a C++ struct/class layout (member name, offset, type) from the DWARF
in libDBEngine.so, using llvm-dwarfdump.

Usage: python3 -I tools/dwarf_struct.py <name> [so_path]

Read-only. Finds the definition DIE (a class/structure_type that has a
DW_AT_byte_size and DW_TAG_member children, not a forward declaration) and prints
each member's byte offset and resolved type name. This is the authoritative
source for on-disk/in-memory field offsets of the HBAM storage classes.
"""
import re
import subprocess
import sys

SO_DEFAULT = "re/extracted/fms/deb/opt/FileMaker/lib/libDBEngine.so"


def dwarf(args):
    return subprocess.run(["llvm-dwarfdump", *args], capture_output=True, text=True).stdout


def die_at(so, off, depth=0):
    """Dump a single DIE (optionally with children) at a hex offset."""
    a = ["--debug-info=" + hex(off)]
    if depth:
        a += ["--show-children", "--recurse-depth=%d" % depth]
    a += [so]
    return dwarf(a)


def parse_attr(block, attr):
    m = re.search(r'DW_AT_%s\s+\(([^\n]*)\)' % attr, block)
    return m.group(1).strip() if m else None


def typename(so, ref, cache, guard=0):
    """Resolve a type DIE offset to a readable name."""
    if ref is None:
        return "?"
    ref = ref.lower()
    m = re.search(r'0x([0-9a-f]+)', ref)
    if not m:
        return ref
    off = int(m.group(1), 16)
    if off in cache:
        return cache[off]
    if guard > 8:
        return hex(off)
    txt = die_at(so, off)
    # first DIE block
    first = txt.split("\n\n")
    blk = ""
    for b in first:
        if "DW_TAG_" in b:
            blk = b
            break
    tag = re.search(r'DW_TAG_(\w+)', blk)
    tag = tag.group(1) if tag else "?"
    name = parse_attr(blk, "name")
    res = None
    typ = parse_attr(blk, "type")
    if tag == "pointer_type":
        res = typename(so, typ, cache, guard + 1) + "*"
    elif tag == "reference_type":
        res = typename(so, typ, cache, guard + 1) + "&"
    elif tag == "const_type":
        res = "const " + typename(so, typ, cache, guard + 1)
    elif tag == "array_type":
        res = typename(so, typ, cache, guard + 1) + "[]"
    elif tag == "typedef" and name:
        res = name
    elif name:
        res = name
    elif typ:
        res = typename(so, typ, cache, guard + 1)
    else:
        res = tag
    cache[off] = res
    return res


def main():
    name = sys.argv[1]
    so = sys.argv[2] if len(sys.argv) > 2 else SO_DEFAULT
    txt = dwarf(["--name=" + name, so])
    # find definition DIEs: class/structure_type with byte_size
    offs = []
    for m in re.finditer(r'0x([0-9a-f]+):\s+DW_TAG_(class_type|structure_type|union_type)\n(.*?)(?=\n0x[0-9a-f]+:|\Z)', txt, re.S):
        off = int(m.group(1), 16)
        body = m.group(3)
        nm = parse_attr(body, "name")
        if nm == name and "DW_AT_byte_size" in body and "DW_AT_declaration" not in body:
            offs.append(off)
    if not offs:
        print(f"no definition DIE with byte_size for {name!r}; declarations only")
        # show the byte_size-bearing ones anyway
        for m in re.finditer(r'0x([0-9a-f]+):\s+DW_TAG_(class_type|structure_type|union_type)\n(.*?)(?=\n0x[0-9a-f]+:|\Z)', txt, re.S):
            body = m.group(3)
            if parse_attr(body, "name") == name:
                bs = parse_attr(body, "byte_size")
                dec = "decl" if "DW_AT_declaration" in body else "DEF"
                print(f"  cand {m.group(1)} byte_size={bs} {dec}")
        return
    cache = {}
    for off in offs[:1]:
        dump = die_at(so, off, depth=1)
        bs = parse_attr(dump.split("\n\n")[0], "byte_size")
        print(f"== {name}  (DIE {hex(off)}, byte_size={bs}) ==")
        for m in re.finditer(r'DW_TAG_(member|inheritance)\n(.*?)(?=\n\s*0x[0-9a-f]+:|\n0x[0-9a-f]+:|\Z)', dump, re.S):
            kind = m.group(1)
            body = m.group(2)
            mn = parse_attr(body, "name") or ("<base>" if kind == "inheritance" else "?")
            loc = parse_attr(body, "data_member_location")
            typ = parse_attr(body, "type")
            tn = typename(so, typ, cache)
            try:
                locn = int(loc, 0) if loc else None
            except ValueError:
                locn = loc
            off_s = f"{locn:#06x}" if isinstance(locn, int) else str(loc)
            print(f"  +{off_s:>8}  {mn:<28} {tn}")


if __name__ == "__main__":
    main()
