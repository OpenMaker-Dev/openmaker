#!/usr/bin/env python3
"""Find printable runs in an fmp12 file, both raw and XOR-0x5A decoded.

Usage: python3 -I tools/fmp12_xor_strings.py <file.fmp12> [min_len] [pattern]

Read-only scan of untrusted data. Prints "<mode> <hex offset> <text>" for each
printable ASCII/UTF-8 run of at least min_len bytes (default 6). If pattern is
given, only runs containing it (case-insensitive) are printed.
"""
import re
import sys


def runs(buf, n):
    for m in re.finditer(rb"[\x20-\x7e\t\r\n]{%d,}" % n, buf):
        yield m.start(), m.group().decode("ascii")


def main():
    path = sys.argv[1]
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 6
    pat = sys.argv[3].lower() if len(sys.argv) > 3 else None
    with open(path, "rb") as fh:
        raw = fh.read()
    xored = bytes(b ^ 0x5A for b in raw)
    for mode, buf in (("raw", raw), ("xor", xored)):
        for off, text in runs(buf, n):
            if pat and pat not in text.lower():
                continue
            print(mode, hex(off), text.replace("\n", "\\n")[:400])


if __name__ == "__main__":
    main()
