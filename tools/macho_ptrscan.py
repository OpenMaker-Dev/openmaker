#!/usr/bin/env python3
"""Find data words in a thin x86_64 Mach-O that point at given C strings.

Handles LC_DYLD_CHAINED_FIXUPS rebase encoding (DYLD_CHAINED_PTR_64 and
_64_OFFSET: low 36 bits = target, high8 at bits 56..63 of the final address),
which is the encoding used by these images. Reads the file as data only.

Usage:
  python3 -I tools/macho_ptrscan.py <thin-macho> find <string> [<string> ...]
  python3 -I tools/macho_ptrscan.py <thin-macho> dump <vmaddr-hex> <nwords>
"""
import struct
import sys

LC_SEGMENT_64 = 0x19


def load(path):
    d = open(path, "rb").read()
    magic, cpu, sub, ftype, ncmds, sizeofcmds, flags, _ = struct.unpack_from("<IiiIIIII", d, 0)
    assert magic == 0xFEEDFACF, "not a thin 64-bit Mach-O"
    off = 32
    sects = []
    for _ in range(ncmds):
        cmd, size = struct.unpack_from("<II", d, off)
        if cmd == LC_SEGMENT_64:
            segname = d[off + 8:off + 24].rstrip(b"\0").decode()
            nsects = struct.unpack_from("<I", d, off + 64)[0]
            so = off + 72
            for _ in range(nsects):
                sectname = d[so:so + 16].rstrip(b"\0").decode()
                addr, size_, offset = struct.unpack_from("<QQI", d, so + 32)
                sects.append((segname, sectname, addr, size_, offset))
                so += 80
        off += size
    return d, sects


def v2o(sects, va):
    for seg, sec, addr, size, off in sects:
        if addr <= va < addr + size and off:
            return off + (va - addr)
    return None


def o2v(sects, fo):
    for seg, sec, addr, size, off in sects:
        if off and off <= fo < off + size:
            return addr + (fo - off)
    return None


def decode_ptr(w):
    if w >> 63:  # bind
        return None
    target = w & 0xFFFFFFFFF
    high8 = (w >> 36) & 0xFF
    return target | (high8 << 56)


def cstr_at(d, sects, va):
    fo = v2o(sects, va)
    if fo is None:
        return None
    end = d.find(b"\0", fo, fo + 4096)
    if end < 0:
        return None
    return d[fo:end].decode("utf-8", errors="replace")


def data_sections(sects):
    return [s for s in sects if s[0] in ("__DATA_CONST", "__DATA", "__AUTH_CONST") and s[4]]


def main():
    path, mode = sys.argv[1], sys.argv[2]
    d, sects = load(path)
    if mode == "find":
        targets = {}
        for s in sys.argv[3:]:
            needle = s.encode() + b"\0"
            p = 0
            while True:
                p = d.find(needle, p)
                if p < 0:
                    break
                if p == 0 or d[p - 1] == 0:
                    va = o2v(sects, p)
                    if va is not None:
                        targets[va] = s
                p += 1
        for seg, sec, addr, size, off in data_sections(sects):
            for i in range(0, size - 7, 8):
                w = struct.unpack_from("<Q", d, off + i)[0]
                t = decode_ptr(w)
                if t in targets:
                    print(f"{seg},{sec} va=0x{addr + i:x} -> 0x{t:x} {targets[t]!r}")
    elif mode == "dump":
        va = int(sys.argv[3], 16)
        n = int(sys.argv[4])
        for k in range(n):
            fo = v2o(sects, va + 8 * k)
            w = struct.unpack_from("<Q", d, fo)[0]
            t = decode_ptr(w)
            s = cstr_at(d, sects, t) if t else None
            lo, hi = w & 0xFFFFFFFF, w >> 32
            desc = repr(s)[:70] if s is not None and t and t > 0x1000 else ""
            print(f"0x{va + 8 * k:x}: {w:016x} lo={lo:<10d} hi={hi:<10d} {desc}")


if __name__ == "__main__":
    main()
