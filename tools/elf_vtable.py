#!/usr/bin/env python3
"""Print the virtual-function slots of a C++ vtable in an x86-64 ELF shared library.

Read-only. Resolves each 8-byte slot from the dynamic relocations
(`llvm-objdump -R` output: R_X86_64_RELATIVE addends and R_X86_64_64 symbol
targets) and names targets with `llvm-nm --defined-only` output.

    python3 -I tools/elf_vtable.py <lib.so> <nm.txt> <relocs.txt> <vtable symbol>

Prints "<offset from the vtable's address point> <target> <symbol>", where the
offset is the displacement used by `call *off(%rax)` after loading the vptr
(address point = vtable symbol + 16).
"""
import struct
import subprocess
import sys


def load_nm(path):
    by_addr, by_name, sizes = {}, {}, {}
    for line in open(path, encoding="utf-8", errors="replace"):
        parts = line.split(None, 2)
        if len(parts) != 3:
            continue
        addr, _kind, name = parts
        a = int(addr, 16)
        by_addr.setdefault(a, name.strip())
        by_name[name.strip()] = a
    return by_addr, by_name


def load_relocs(path):
    relocs = {}
    for line in open(path, encoding="utf-8", errors="replace"):
        parts = line.split()
        if len(parts) < 3 or not parts[1].startswith("R_X86_64"):
            continue
        off = int(parts[0], 16)
        relocs[off] = (parts[1], parts[2])
    return relocs


def demangle(names):
    try:
        out = subprocess.run(["c++filt"], input="\n".join(names), capture_output=True,
                             text=True, check=True).stdout.splitlines()
        return out
    except (OSError, subprocess.CalledProcessError):
        return names


def main():
    lib, nm_path, reloc_path, vt = sys.argv[1:5]
    by_addr, by_name = load_nm(nm_path)
    relocs = load_relocs(reloc_path)
    start = by_name[vt]
    # the vtable size is the distance to the next symbol
    later = sorted(a for a in by_addr if a > start)
    end = later[0] if later else start + 0x800
    rows = []
    for slot in range(start + 16, end, 8):
        kind, value = relocs.get(slot, ("", ""))
        target = None
        if kind == "R_X86_64_RELATIVE" and value.startswith("*ABS*+"):
            target = int(value[6:], 16)
            name = by_addr.get(target, "?")
        elif kind:
            name = value
        else:
            name = "(no relocation)"
        rows.append((slot - start - 16, target, name))
    names = demangle([r[2] for r in rows])
    for (off, target, _), name in zip(rows, names):
        print(f"{off:#06x} {target:#x} {name}" if target is not None else f"{off:#06x} - {name}")


if __name__ == "__main__":
    main()
