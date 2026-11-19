#!/usr/bin/env python3
"""Locate and export arrays of string-pointer records in a thin x86_64 Mach-O.

A "record" is `width` consecutive 8-byte words, each a chained-fixup rebase
pointer into a C-string section. Runs of such records are reported (scan
mode) or exported as TSV with the record index (export mode). Reads the file
as data only.

Usage:
  python3 -I tools/macho_strtables.py <thin-macho> scan <width> [min_records]
  python3 -I tools/macho_strtables.py <thin-macho> export <start-va-hex> <width> <count> <out.tsv>
"""
import struct
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from macho_ptrscan import load, v2o, decode_ptr  # noqa: E402


def cstr_sections(sects):
    return [(a, a + s) for seg, sec, a, s, o in sects if sec in ("__cstring", "__const") and seg == "__TEXT"]


def is_str_ptr(w, ranges):
    t = decode_ptr(w)
    if t is None:
        return None
    for lo, hi in ranges:
        if lo <= t < hi:
            return t
    return None


def cstr(d, sects, va):
    fo = v2o(sects, va)
    end = d.find(b"\0", fo, fo + 8192)
    return d[fo:end].decode("utf-8", errors="replace")


def esc(s):
    return s.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t")


def main():
    path, mode = sys.argv[1], sys.argv[2]
    d, sects = load(path)
    ranges = cstr_sections(sects)
    if mode == "scan":
        width = int(sys.argv[3])
        minrec = int(sys.argv[4]) if len(sys.argv) > 4 else 20
        for seg, sec, addr, size, off in sects:
            if seg not in ("__DATA_CONST", "__DATA") or not off:
                continue
            nwords = size // 8
            ok = [is_str_ptr(struct.unpack_from("<Q", d, off + 8 * i)[0], ranges) is not None for i in range(nwords)]
            i = 0
            while i < nwords:
                if not ok[i]:
                    i += 1
                    continue
                j = i
                while j < nwords and ok[j]:
                    j += 1
                n = (j - i) // width
                if n >= minrec:
                    va = addr + 8 * i
                    first = [cstr(d, sects, decode_ptr(struct.unpack_from("<Q", d, off + 8 * (i + k))[0]))[:40] for k in range(width)]
                    print(f"{seg},{sec} va=0x{va:x} words={j - i} records~{n} first={first}")
                i = j
    elif mode == "export":
        va = int(sys.argv[3], 16)
        width, count, out = int(sys.argv[4]), int(sys.argv[5]), sys.argv[6]
        with open(out, "w", encoding="utf-8") as f:
            for r in range(count):
                fields = []
                for k in range(width):
                    w = struct.unpack_from("<Q", d, v2o(sects, va + 8 * (r * width + k)))[0]
                    t = is_str_ptr(w, ranges)
                    fields.append(esc(cstr(d, sects, t)) if t is not None else f"#0x{w:x}")
                f.write(f"{r}\t" + "\t".join(fields) + "\n")


if __name__ == "__main__":
    main()
