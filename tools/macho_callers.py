#!/usr/bin/env python3
"""List the functions that call a given set of symbols in a Mach-O binary.

Reads a filtered llvm-objdump disassembly (function headers plus call lines)
and resolves calls through __stubs with the indirect symbol table, so calls
into other dylibs are attributed correctly.  Output is demangled.

Inputs are produced with:
  llvm-objdump-18 -d --no-show-raw-insn BIN | grep -E '^[0-9a-f]+ <|call' > calls.txt
  llvm-objdump-18 --macho --indirect-symbols BIN \
    | awk '/Indirect symbols for \\(__TEXT,__stubs\\)/{f=1;next} /Indirect symbols for/{f=0} f && /^0x/{print $1, $3}' > stubs.txt

Usage:
  python3 -I tools/macho_callers.py calls.txt stubs.txt REGEX [REGEX ...]
REGEX is matched against the mangled callee name.  Calls may be gzip'ed.
"""

import gzip
import re
import subprocess
import sys
from collections import defaultdict


def main():
    calls_path, stubs_path, *pats = sys.argv[1:]
    stubs = {}
    for line in open(stubs_path):
        addr, name = line.split()
        stubs[int(addr, 16)] = name
    rx = [re.compile(p) for p in pats]
    opener = gzip.open if calls_path.endswith(".gz") else open
    hdr = re.compile(r"^([0-9a-f]+) <(.*)>:")
    call = re.compile(r"call[q]?\s+(?:\*.*|0x([0-9a-f]+)\s*<(.*)>)")
    cur = "?"
    found = defaultdict(set)
    with opener(calls_path, "rt", errors="replace") as fh:
        for line in fh:
            m = hdr.match(line)
            if m:
                cur = m.group(2)
                continue
            m = call.search(line)
            if not m or m.group(1) is None:
                continue
            target = stubs.get(int(m.group(1), 16), m.group(2))
            for r in rx:
                if r.search(target):
                    found[cur].add(target)
    names = sorted(found)
    flat = names + sorted({t for v in found.values() for t in v})
    dem = subprocess.run(["llvm-cxxfilt-18", "-_"], input="\n".join(flat),
                         capture_output=True, text=True).stdout.splitlines()
    dm = dict(zip(flat, dem))
    for n in names:
        print(dm.get(n, n))
        for t in sorted(found[n]):
            print("    ->", dm.get(t, t)[:160])


if __name__ == "__main__":
    main()
