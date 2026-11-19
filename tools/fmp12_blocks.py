#!/usr/bin/env python3
"""Dump the 4096-byte block/page structure of an HBAM7 (.fmp12 / .dls) file.

Usage: python3 -I tools/fmp12_blocks.py <file> [first_block] [count]

Read-only scan of untrusted data; nothing in the file is executed. Treats the
file as a sequence of 0x1000-byte blocks. Block 0 is the file header. For every
other block it prints the candidate 20-byte header as five big-endian u32 words
plus the payload's first bytes, so the prev/next linkage and per-block fields can
be read off directly. No interpretation is assumed beyond the block size.
"""
import struct
import sys

BLOCK = 0x1000


def u32(b, off):
    return struct.unpack_from(">I", b, off)[0]


def main():
    path = sys.argv[1]
    first = int(sys.argv[2]) if len(sys.argv) > 2 else 0
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 1 << 30
    with open(path, "rb") as fh:
        data = fh.read()
    nblocks = len(data) // BLOCK
    print(f"file={path} size={len(data)} blocks={nblocks}")
    last = min(nblocks, first + count)
    for blk in range(first, last):
        base = blk * BLOCK
        hdr = data[base:base + 20]
        if blk == 0:
            print(f"blk {blk:5d} @ {base:#08x} HEADER magic={data[base+0x0f:base+0x14]!r} "
                  f"words={[hex(u32(data, base + 4 * k)) for k in range(8)]}")
            continue
        words = [u32(data, base + 4 * k) for k in range(5)]
        # tentative interpretation: w0 flags, w1 prev, w2 next, w3 ?, w4 ?
        payload = data[base + 20:base + 20 + 16]
        print(f"blk {blk:5d} @ {base:#08x} "
              f"w0={words[0]:#010x} prev={words[1]:<6d} next={words[2]:<6d} "
              f"w3={words[3]:<8d} w4={words[4]:<8d} pay={payload.hex()}")


if __name__ == "__main__":
    main()
