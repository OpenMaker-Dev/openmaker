#!/usr/bin/env python3
"""Print the initialized bytes of data symbols in an x86-64 ELF shared library.

Read-only. Maps each symbol's virtual address to a file offset through the
section headers and prints its bytes in hex ("bss" for zero-initialized
symbols, whose values are only set at run time).

    python3 -I tools/elf_symbol_bytes.py <lib.so> <nm -S output> SYMBOL...
"""
import struct
import sys


def sections(path):
    with open(path, "rb") as f:
        head = f.read(64)
        shoff, = struct.unpack_from("<Q", head, 0x28)
        shentsize, shnum = struct.unpack_from("<HH", head, 0x3a)
        f.seek(shoff)
        table = f.read(shentsize * shnum)
    out = []
    for i in range(shnum):
        _name, kind, _flags, addr, off, size = struct.unpack_from("<IIQQQQ", table, i * shentsize)
        if addr:
            out.append((addr, size, off, kind))
    return out


def main():
    lib, nm_path = sys.argv[1:3]
    wanted = set(sys.argv[3:])
    syms = {}
    for line in open(nm_path, encoding="utf-8", errors="replace"):
        parts = line.split()
        if len(parts) == 4 and parts[3] in wanted:
            syms[parts[3]] = (int(parts[0], 16), int(parts[1], 16))
    secs = sections(lib)
    with open(lib, "rb") as f:
        for name in sys.argv[3:]:
            if name not in syms:
                print(f"{name} missing")
                continue
            addr, size = syms[name]
            for base, length, off, kind in secs:
                if base <= addr < base + length:
                    if kind == 8:  # SHT_NOBITS
                        print(f"{name} size={size} bss")
                    else:
                        f.seek(off + addr - base)
                        print(f"{name} size={size} {f.read(min(size, 64)).hex()}")
                    break
            else:
                print(f"{name} unmapped")


if __name__ == "__main__":
    main()
