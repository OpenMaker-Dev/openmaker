#!/usr/bin/env python3
"""Condensed, read-only digest of x86-64 functions in an ELF library.

For each function (mangled symbol), prints in address order only the
instructions that carry interface facts: direct calls (demangled target),
virtual calls (`vcall +off`, optionally named from an elf_vtable.py listing),
RIP-relative symbol references, immediate operands, and memory accesses at
chosen struct offsets (default: the ScriptStep descriptor fields 0x14 opcode,
0x16 step value, 0x18 step flags). Branch targets are kept so control flow can
be followed. It prints no instruction bodies beyond these facts. Virtual-call
names from --vtable are only right for calls through that class's own vtable;
calls through other objects get the same offsets and wrong names.

    python3 -I tools/elf_func_digest.py <lib.so> [--vtable v.txt] [--offsets 0x14,0x16,0x18] SYMBOL...
"""
import argparse
import re
import subprocess


def objdump(lib, sym):
    tool = subprocess.run(["xcrun", "--find", "llvm-objdump"], capture_output=True, text=True).stdout.strip()
    out = subprocess.run([tool, "-d", "--no-show-raw-insn", f"--disassemble-symbols={sym}", lib],
                         capture_output=True, text=True).stdout
    return out.splitlines()


def demangle(names):
    if not names:
        return {}
    out = subprocess.run(["c++filt"], input="\n".join(names), capture_output=True, text=True).stdout.splitlines()
    return dict(zip(names, out))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lib")
    ap.add_argument("--vtable", action="append", default=[])
    ap.add_argument("--offsets", default="0x14,0x16,0x18")
    ap.add_argument("symbols", nargs="+")
    a = ap.parse_args()
    vt = {}
    for path in a.vtable:
        for line in open(path):
            p = line.split(None, 2)
            if len(p) == 3:
                name = re.sub(r"\(.*", "", p[2].strip())
                vt.setdefault(int(p[0], 16), []).append(name.split("::")[-1])
    offs = [int(x, 16) for x in a.offsets.split(",") if x]
    off_re = re.compile(r"(?<![0-9a-fx])(" + "|".join(f"{o:#x}" for o in offs) + r")\(%r(?!ip|sp)")
    for sym in a.symbols:
        lines = objdump(a.lib, sym)
        rows = []
        names = set()
        for line in lines:
            m = re.match(r"\s*([0-9a-f]+):\s+(\S+)\s*(.*)$", line)
            if not m:
                continue
            addr, op, rest = m.groups()
            comment = ""
            if "#" in rest:
                rest, comment = rest.split("#", 1)
            rest = rest.strip()
            sm = re.search(r"<([^>+]+)(\+0x[0-9a-f]+)?(@plt)?>", comment or rest)
            target_sym = sm.group(1) if sm else None
            keep = None
            if op.startswith("call"):
                if rest.startswith("*"):
                    vm = re.match(r"\*(0x[0-9a-f]+)\(%r\w+\)", rest)
                    if vm:
                        off = int(vm.group(1), 16)
                        cand = "/".join(vt.get(off, []))
                        keep = f"vcall +{off:#x} {cand}"
                    else:
                        keep = f"icall {rest}"
                elif target_sym:
                    names.add(target_sym)
                    keep = ("call", target_sym)
            elif op.startswith("j"):
                keep = f"{op} {rest.split()[0]}"
            elif "(%rip)" in rest and target_sym:
                names.add(target_sym)
                keep = (op, target_sym)
            elif "$" in rest and op not in ("push", "pushq", "sub", "subq", "add", "addq") and "%rsp" not in rest:
                keep = f"{op} {rest}"
            elif off_re.search(rest):
                keep = f"{op} {rest}"
            if keep is not None:
                rows.append((addr, keep))
        dm = demangle(sorted(names))
        print(f"== {dm.get(sym, sym) if sym in dm else sym}")
        for addr, keep in rows:
            if isinstance(keep, tuple):
                op, s = keep
                d = dm.get(s, s)
                d = re.sub(r"\((?:[^()]|\([^()]*\))*\)( const)?$", "", d) if len(d) > 90 else d
                print(f"  {addr}: {op} {d}")
            else:
                print(f"  {addr}: {keep}")


if __name__ == "__main__":
    main()
