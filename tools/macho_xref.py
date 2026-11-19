#!/usr/bin/env python3
"""Find x86_64 RIP-relative LEA/MOV references from __text into an address range.

Pattern-scans __TEXT,__text for REX.W(48/4C) 8D/8B modrm(mod=00, rm=101)
disp32 and reports instructions whose target falls in [lo, hi). Heuristic:
it does not disassemble, so false positives are possible; confirm in Ghidra
or with llvm-objdump --start-address. Reads the file as data only.

Usage: python3 -I tools/macho_xref.py <thin-macho> <lo-hex> <hi-hex> [--symbols dsym.txt]
"""
import bisect
import re
import struct
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from macho_ptrscan import load  # noqa: E402


def main():
    path, lo, hi = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3], 16)
    syms = []
    if "--symbols" in sys.argv:
        for line in open(sys.argv[sys.argv.index("--symbols") + 1], encoding="utf-8", errors="replace"):
            p = line.rstrip("\n").split(" ", 2)
            if len(p) == 3 and p[1] in ("T", "t"):
                syms.append((int(p[0], 16), p[2]))
        syms.sort()
    addrs = [a for a, _ in syms]
    d, sects = load(path)
    text = [s for s in sects if s[1] == "__text"][0]
    _, _, taddr, tsize, toff = text
    blob = d[toff:toff + tsize]
    pat = re.compile(rb"[\x48\x4c][\x8d\x8b][\x05\x0d\x15\x1d\x25\x2d\x35\x3d]", re.S)
    for m in pat.finditer(blob):
        i = m.start()
        disp = struct.unpack_from("<i", blob, i + 3)[0]
        va = taddr + i
        target = va + 7 + disp
        if lo <= target < hi:
            fn = ""
            if syms:
                k = bisect.bisect_right(addrs, va) - 1
                if k >= 0:
                    fn = f"{syms[k][1][:110]}+0x{va - syms[k][0]:x}"
            op = "lea" if blob[i + 1] == 0x8D else "mov"
            print(f"0x{va:x} {op} -> 0x{target:x} {fn}")


if __name__ == "__main__":
    main()
