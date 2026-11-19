#!/usr/bin/env python3
"""Print the C strings and callees referenced by one function of a thin x86_64
Mach-O (RIP-relative lea/mov operands that land in __cstring/__const, and call
targets resolved through __stubs).  The binary is only read, never executed.

Usage:
  python3 -I tools/macho_func_strings.py BIN STUBS_TXT ALLSYMS_TXT REGEX
ALLSYMS_TXT is `llvm-nm BIN | llvm-cxxfilt -_` output; REGEX selects functions
by demangled name (first 5 matches).  STUBS_TXT as in tools/macho_callers.py.
"""

import re
import subprocess
import sys


def main():
    binp, stubs_p, syms_p, rx = sys.argv[1:5]
    data = open(binp, "rb").read()
    stubs = {int(a, 16): n for a, n in (l.split() for l in open(stubs_p))}
    syms = []
    for line in open(syms_p):
        p = line.rstrip("\n").split(" ", 2)
        if len(p) == 3 and p[0] and p[1] in "tT":
            syms.append((int(p[0], 16), p[2]))
    syms.sort()
    addrs = [a for a, _ in syms]
    pat = re.compile(rx)
    hits = [(i, s) for i, s in enumerate(syms) if pat.search(s[1])][:5]
    for i, (start, name) in hits:
        end = syms[i + 1][0] if i + 1 < len(syms) else start + 0x1000
        end = min(end, start + 0x20000)
        out = subprocess.run(["llvm-objdump-18", "-d", "--no-show-raw-insn",
                              f"--start-address={start:#x}", f"--stop-address={end:#x}", binp],
                             capture_output=True, text=True).stdout
        print(f"== {name[:150]}  [{start:#x}-{end:#x}]")
        seen = set()
        for line in out.splitlines():
            m = re.search(r"## (0x[0-9a-f]+)", line)
            if m:
                a = int(m.group(1), 16)
                if a in seen or a >= len(data):
                    continue
                seen.add(a)
                s = data[a:a + 120].split(b"\0", 1)[0]
                if len(s) >= 3 and all(32 <= c < 127 for c in s):
                    print(f"   str {a:#x}: {s.decode()!r}")
            m = re.search(r"call[q]?\s+0x([0-9a-f]+)", line)
            if m:
                a = int(m.group(1), 16)
                if a in stubs:
                    t = stubs[a]
                else:
                    import bisect
                    j = bisect.bisect_right(addrs, a) - 1
                    t = syms[j][1] if j >= 0 and syms[j][0] == a else None
                if t and t not in seen:
                    seen.add(t)
                    dm = subprocess.run(["llvm-cxxfilt-18", "-_"], input=t, capture_output=True, text=True).stdout.strip()
                    print(f"   call {dm[:140]}")


if __name__ == "__main__":
    main()
