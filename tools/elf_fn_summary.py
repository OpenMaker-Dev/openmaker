#!/usr/bin/env python3
"""Summarize functions of an unstripped ELF library (libDBEngine.so etc.)
without printing their bodies: callees in call order, immediate constants,
referenced C strings, and the source files and line numbers the code maps to.

    python3 -I tools/elf_fn_summary.py LIB REGEX [--max N] [--lines] [--all-imm]

REGEX is matched (re.search) against demangled symbol names, for example
'DBDataArg::Left\\(' or '^Draco::DBDataArg::Json'. Requires llvm-nm,
llvm-objdump and llvm-cxxfilt (found through xcrun on macOS, else PATH).
Read-only; meant for clean-room evidence gathering (cite callees and
constants, never paste the disassembly).
"""
import argparse
import re
import shutil
import struct
import subprocess
import sys


NOISE = re.compile(r"^(operator (new|delete)|__clang_call_terminate|_Unwind_Resume|__stack_chk_fail|"
                   r"Draco::Error::~Error|Draco::Text::Text\(\)|Draco::Text::~Text|"
                   r"Draco::DBData::ConvertToNative\(\)|Draco::Msg::~Msg|std::__throw_)")


def tool(name):
    try:
        return subprocess.run(["xcrun", "--find", name], capture_output=True,
                              text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return shutil.which(name) or name


def load_sections(path):
    with open(path, "rb") as f:
        data = f.read()
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    secs = []
    for i in range(shnum):
        off = shoff + i * shentsize
        name, typ, flags, addr, offset, size = struct.unpack_from("<IIQQQQ", data, off)
        secs.append([name, typ, addr, offset, size])
    stroff = secs[shstrndx][3]
    out = []
    for name, typ, addr, offset, size in secs:
        end = data.index(b"\0", stroff + name)
        out.append((data[stroff + name:end].decode(), addr, offset, size, typ))
    return data, out


def cstr_at(data, secs, va):
    for name, addr, offset, size, typ in secs:
        if typ == 8 or not size:  # NOBITS
            continue
        if addr <= va < addr + size and name.startswith((".rodata", ".data")):
            o = offset + (va - addr)
            end = data.find(b"\0", o, o + 300)
            if end < 0:
                return None
            b = data[o:end]
            if len(b) >= 2 and all(32 <= c < 127 or c in (9, 10, 13) for c in b):
                return b.decode()
            # UTF-16LE string (unichar16 literals)
            u = []
            for j in range(o, o + 400, 2):
                c, = struct.unpack_from("<H", data, j)
                if c == 0:
                    break
                u.append(c)
            if len(u) >= 2 and all(9 <= c < 0x3000 for c in u) and any(c < 0x80 for c in u):
                return "u16:" + "".join(map(chr, u))
            return None
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lib")
    ap.add_argument("regex")
    ap.add_argument("--max", type=int, default=20)
    ap.add_argument("--lines", action="store_true", help="print source line ranges")
    ap.add_argument("--all-imm", action="store_true", help="include small immediates")
    ap.add_argument("--noise", action="store_true", help="keep allocation, unwind and conversion helpers in calls")
    a = ap.parse_args()

    nm = subprocess.run([tool("llvm-nm"), "--defined-only", a.lib], capture_output=True, text=True).stdout
    syms = []
    for line in nm.splitlines():
        p = line.split()
        if len(p) == 3 and p[1] in "tTwW":
            syms.append((p[2], int(p[0], 16)))
    names = subprocess.run([tool("llvm-cxxfilt")], input="\n".join(s for s, _ in syms),
                           capture_output=True, text=True).stdout.splitlines()
    rx = re.compile(a.regex)
    hits = [(m, d, addr) for (m, addr), d in zip(syms, names) if rx.search(d)]
    seen = set()
    hits = [h for h in hits if not (h[2] in seen or seen.add(h[2]))]
    if not hits:
        sys.exit("no symbol matches")
    data, secs = load_sections(a.lib)
    for mangled, dem, addr in hits[:a.max]:
        cmd = [tool("llvm-objdump"), "-d", "--no-show-raw-insn",
               "--disassemble-symbols=" + mangled, a.lib]
        if a.lines:
            cmd.insert(2, "-l")
        dis = subprocess.run(cmd, capture_output=True, text=True).stdout
        dis = subprocess.run([tool("llvm-cxxfilt")], input=dis, capture_output=True, text=True).stdout
        calls, imms, strs, lines = [], [], [], []
        ninsn = 0
        for ln in dis.splitlines():
            m = re.match(r"^; (/\S+):(\d+)", ln)
            if m:
                f = m.group(1).rsplit("/", 1)[-1]
                lines.append((f, int(m.group(2))))
                continue
            m = re.match(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$", ln)
            if not m:
                continue
            ninsn += 1
            op, rest = m.group(2), m.group(3)
            if op.startswith("call") or (op.startswith("jmp") and "<" in rest and "+0x" not in rest):
                t = re.search(r"<(.*)>", rest)
                tgt = t.group(1) if t else rest.strip()
                tgt = tgt.replace("@plt", "")
                if not a.noise and NOISE.search(tgt):
                    continue
                if not calls or calls[-1] != tgt:
                    calls.append(tgt)
            for im in re.findall(r"\$(-?0x[0-9a-f]+)", rest):
                v = int(im, 16)
                if v >= 1 << 63:
                    v -= 1 << 64
                if a.all_imm or abs(v) > 8:
                    imms.append(v)
            c = re.search(r"# (0x[0-9a-f]+)", rest)
            if c and "lea" in op:
                s = cstr_at(data, secs, int(c.group(1), 16))
                if s is not None:
                    strs.append(s)
        print(f"== {dem}  @0x{addr:x}  ({ninsn} insns)")
        if a.lines and lines:
            byfile = {}
            for f, n in lines:
                byfile.setdefault(f, set()).add(n)
            for f, ns in byfile.items():
                ns = sorted(ns)
                print(f"   lines {f}: {ns[0]}-{ns[-1]} ({len(ns)} distinct)")
        print("   calls: " + " | ".join(calls))
        if imms:
            uniq = []
            for v in imms:
                if v not in uniq:
                    uniq.append(v)
            print("   imm: " + " ".join(str(v) if abs(v) < 4096 else hex(v) for v in uniq))
        if strs:
            print("   strings: " + " | ".join(repr(s) for s in dict.fromkeys(strs)))
    if len(hits) > a.max:
        print(f"... {len(hits) - a.max} more matches")


if __name__ == "__main__":
    main()
