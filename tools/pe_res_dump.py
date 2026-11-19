#!/usr/bin/env python3
"""Dump PE resource payloads using offsets from `rea inspect-pe-resources --format json`.

Writes each payload to <outdir>/<type>/<name>_<lang>.bin and decodes RT_STRING
(type 6) blocks into <outdir>/strings.tsv (id<TAB>text, escapes \\n \\t \\\\).

Usage: python3 -I tools/pe_res_dump.py <pe-file> <rea-json> <outdir>
Reads the PE as data only; nothing is executed.
"""
import json
import os
import sys


def esc(s):
    return s.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t")


def main():
    pe, rj, out = sys.argv[1:4]
    data = open(pe, "rb").read()
    res = json.load(open(rj))["normalized_result"]["resources"]
    os.makedirs(out, exist_ok=True)
    strings = []
    for r in res:
        t = r["type"]
        tname = t["name"] if t["kind"] == "name" else str(t["id"])
        n = r["name"]
        nname = n["name"] if n["kind"] == "name" else str(n["id"])
        lang = r["language"]["id"] if r["language"]["kind"] == "id" else r["language"].get("name")
        loc = r["payload"]["location"]
        blob = data[loc["offset"]:loc["offset"] + loc["bytes"]]
        d = os.path.join(out, tname)
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, f"{nname}_{lang}.bin"), "wb") as f:
            f.write(blob)
        if tname == "6" and n["kind"] == "id":
            base = (n["id"] - 1) * 16
            p = 0
            for i in range(16):
                if p + 2 > len(blob):
                    break
                ln = int.from_bytes(blob[p:p + 2], "little")
                p += 2
                s = blob[p:p + 2 * ln].decode("utf-16-le", errors="replace")
                p += 2 * ln
                if ln:
                    strings.append((base + i, s))
    strings.sort()
    with open(os.path.join(out, "strings.tsv"), "w", encoding="utf-8") as f:
        for i, s in strings:
            f.write(f"{i}\t{esc(s)}\n")
    print(f"{len(res)} resources, {len(strings)} RT_STRING entries")


if __name__ == "__main__":
    main()
