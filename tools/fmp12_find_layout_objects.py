#!/usr/bin/env python3
"""Heuristically locate FMLayoutPB.LayoutObject messages inside an fmp12 file.

Usage: python3 -I tools/fmp12_find_layout_objects.py <file.fmp12> [max]

Read-only scan of untrusted data; nothing is executed. Looks for a Rect
"bounds" field (tag 0x22, length 0x24, then doubles tagged 0x09/0x11/0x19/0x21)
and walks back to the enclosing LayoutObject (tags 0x08 type, 0x10 key,
0x18 flags). Prints offset, type, key, bounds, and any customStyleName.
This only finds objects whose bytes are contiguous in one HBAM block run.
"""
import struct
import sys


def varint(b, i):
    v = 0
    s = 0
    while True:
        c = b[i]
        i += 1
        v |= (c & 0x7F) << s
        if not c & 0x80:
            return v, i
        s += 7
        if s > 63:
            raise ValueError


def parse_fields(b, i, end):
    out = []
    while i < end:
        tag, i = varint(b, i)
        fn, wt = tag >> 3, tag & 7
        if fn == 0:
            raise ValueError
        if wt == 0:
            v, i = varint(b, i)
        elif wt == 1:
            v = b[i:i + 8]
            i += 8
        elif wt == 5:
            v = b[i:i + 4]
            i += 4
        elif wt == 2:
            ln, i = varint(b, i)
            if i + ln > end:
                raise ValueError
            v = (i, i + ln)
            i += ln
        else:
            raise ValueError
        out.append((fn, wt, v))
    if i != end:
        raise ValueError
    return out


def strings_in(b, s, e, depth=0):
    res = []
    try:
        for fn, wt, v in parse_fields(b, s, e):
            if wt == 2:
                sub = b[v[0]:v[1]]
                if sub and all(32 <= c < 127 for c in sub):
                    res.append(sub.decode())
                elif depth < 3:
                    res.extend(strings_in(b, v[0], v[1], depth + 1))
    except (ValueError, IndexError):
        pass
    return res


def main():
    data = open(sys.argv[1], "rb").read()
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 50
    found = 0
    i = 0
    while found < limit:
        i = data.find(b"\x22\x24\x09", i + 1)
        if i < 0:
            break
        if data[i + 11] != 0x11 or data[i + 20] != 0x19 or data[i + 29] != 0x21:
            continue
        top, left, bottom, right = (struct.unpack("<d", data[i + 3 + 9 * k:i + 11 + 9 * k])[0] for k in range(4))
        # walk back: find "12 <len> 08" such that the message spans the bounds
        best = None
        for back in range(3, 40):
            s = i - back
            if s < 0 or data[s] != 0x12:
                continue
            try:
                ln, j = varint(data, s + 1)
            except (ValueError, IndexError):
                continue
            if data[j] != 0x08 or j + ln <= i + 38:
                continue
            try:
                fields = parse_fields(data, j, j + ln)
            except (ValueError, IndexError):
                continue
            best = (s, j, ln, fields)
            break
        if not best:
            print(f"{i:#x} bounds t={top:g} l={left:g} b={bottom:g} r={right:g} (no enclosing object)")
            found += 1
            continue
        s, j, ln, fields = best
        f = {}
        for fn, wt, v in fields:
            f.setdefault(fn, []).append(v)
        typ = f.get(1, ["?"])[0]
        key = f.get(2, ["?"])[0]
        flags = f.get(3, [0])[0]
        names = []
        if 5 in f:
            names = strings_in(data, *f[5][0])
        children = len(f.get(6, []))
        subs = sorted(k for k in f if k >= 7)
        print(f"{s:#x} type={typ} key={key} flags={flags:#x} bounds t={top:g} l={left:g} b={bottom:g} r={right:g} "
              f"children={children} submsgs={subs} styles={names[:3]}")
        found += 1


if __name__ == "__main__":
    main()
