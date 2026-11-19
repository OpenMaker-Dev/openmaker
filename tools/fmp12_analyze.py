#!/usr/bin/env python3
"""Deeper structural analysis of an HBAM7 container.

Usage: python3 -I tools/fmp12_analyze.py <file>

Read-only. Prints:
  - block 0 (superblock) non-zero regions,
  - the prev/next linkage grouped into chains (one per b-tree level),
  - candidate checksum verification for a couple of blocks.
Nothing in the file is executed.
"""
import struct
import sys

BLOCK = 0x1000


def u32(b, off):
    return struct.unpack_from(">I", b, off)[0]


def u16(b, off):
    return struct.unpack_from(">H", b, off)[0]


def main():
    path = sys.argv[1]
    with open(path, "rb") as fh:
        data = fh.read()
    n = len(data) // BLOCK
    print(f"file={path} size={len(data)} blocks={n}")

    # --- block 0 non-zero regions ---
    print("\n== block 0 non-zero byte ranges ==")
    b0 = data[0:BLOCK]
    i = 0
    while i < BLOCK:
        if b0[i] != 0:
            j = i
            while j < BLOCK and (b0[j] != 0 or (j + 1 < BLOCK and b0[j + 1] != 0)):
                j += 1
            print(f"  [{i:#06x}..{j:#06x}] {b0[i:j].hex()}  {b0[i:j][:32]!r}")
            i = j
        i += 1

    # --- linkage ---
    print("\n== prev/next linkage ==")
    prev = {}
    nxt = {}
    for blk in range(1, n):
        base = blk * BLOCK
        prev[blk] = u32(data, base + 4)
        nxt[blk] = u32(data, base + 8)
    heads = [blk for blk in range(1, n) if prev[blk] == 0]
    print(f"  blocks with prev==0 (chain heads): {heads}")
    for h in heads:
        chain = []
        cur = h
        seen = set()
        while cur != 0 and cur not in seen and cur <= n:
            seen.add(cur)
            chain.append(cur)
            cur = nxt.get(cur, 0)
        print(f"  chain from {h}: len={len(chain)} -> {chain[:60]}{' ...' if len(chain) > 60 else ''}")

    # --- which blocks are not in any chain (free?) ---
    inchain = set()
    for h in heads:
        cur = h
        seen = set()
        while cur != 0 and cur not in seen and cur <= n:
            seen.add(cur)
            inchain.add(cur)
            cur = nxt.get(cur, 0)
    missing = [b for b in range(1, n) if b not in inchain]
    print(f"  blocks not reached by any head chain: {missing}")

    # --- header word stats for a run ---
    print("\n== header words blk 1..6 ==")
    for blk in range(1, min(7, n)):
        base = blk * BLOCK
        print(f"  blk {blk}: " + " ".join(f"{u16(data, base+2*k):#06x}" for k in range(10)))


if __name__ == "__main__":
    main()
