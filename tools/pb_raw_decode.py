#!/usr/bin/env python3
"""Schema-less protobuf wire decoder for exploring untrusted blobs.

Usage: python3 -I tools/pb_raw_decode.py <file> <hex offset> <hex length> [max_depth]

Decodes the byte range as protobuf wire format and prints a tree of
field-number / wire-type / value. Length-delimited fields are tried as nested
messages first (if they parse cleanly), then printed as text or hex.
Fixed32 values are also shown as float, fixed64 as double.
Stops at the first malformed tag and reports the offset.
"""
import struct
import sys


class Bad(Exception):
    pass


def varint(b, i, end):
    shift = 0
    val = 0
    while True:
        if i >= end:
            raise Bad("varint overrun")
        c = b[i]
        i += 1
        val |= (c & 0x7F) << shift
        if not c & 0x80:
            return val, i
        shift += 7
        if shift > 70:
            raise Bad("varint too long")


def parse(b, i, end):
    out = []
    while i < end:
        start = i
        tag, i = varint(b, i, end)
        fn, wt = tag >> 3, tag & 7
        if fn == 0:
            raise Bad("field 0 at %#x" % start)
        if wt == 0:
            v, i = varint(b, i, end)
            out.append((start, fn, "varint", v))
        elif wt == 1:
            if i + 8 > end:
                raise Bad("fixed64 overrun")
            raw = b[i:i + 8]
            i += 8
            out.append((start, fn, "fixed64", (struct.unpack("<Q", raw)[0], struct.unpack("<d", raw)[0])))
        elif wt == 5:
            if i + 4 > end:
                raise Bad("fixed32 overrun")
            raw = b[i:i + 4]
            i += 4
            out.append((start, fn, "fixed32", (struct.unpack("<I", raw)[0], struct.unpack("<f", raw)[0])))
        elif wt == 2:
            ln, i = varint(b, i, end)
            if i + ln > end:
                raise Bad("len overrun at %#x (len %d)" % (start, ln))
            out.append((start, fn, "len", (i, i + ln)))
            i += ln
        else:
            raise Bad("wire type %d at %#x" % (wt, start))
    return out


def show(b, i, end, depth, maxd, base):
    try:
        items = parse(b, i, end)
    except Bad as e:
        print("  " * depth + "!! " + str(e))
        return False
    for start, fn, kind, v in items:
        pad = "  " * depth
        if kind == "len":
            s, e = v
            sub = b[s:e]
            nested_ok = False
            if depth < maxd and len(sub) > 0:
                try:
                    parse(b, s, e)
                    nested_ok = True
                except Bad:
                    nested_ok = False
            printable = all(32 <= c < 127 or c in (9, 10, 13) for c in sub) or _utf8(sub)
            if printable and (not nested_ok or len(sub) < 3):
                print(f"{pad}{fn}: str {sub.decode('utf-8', 'replace')!r}  @{base + start:#x}")
            elif nested_ok:
                print(f"{pad}{fn}: msg ({len(sub)} bytes)  @{base + start:#x}")
                show(b, s, e, depth + 1, maxd, base)
            else:
                print(f"{pad}{fn}: bytes[{len(sub)}] {sub[:48].hex()}  @{base + start:#x}")
        elif kind == "fixed32":
            print(f"{pad}{fn}: f32 {v[0]:#x} ({v[1]:.6g})")
        elif kind == "fixed64":
            print(f"{pad}{fn}: f64 {v[0]:#x} ({v[1]:.6g})")
        else:
            print(f"{pad}{fn}: {v}")
    return True


def _utf8(sub):
    try:
        t = sub.decode("utf-8")
    except UnicodeDecodeError:
        return False
    return all(ch.isprintable() or ch in "\t\r\n" for ch in t)


def main():
    path = sys.argv[1]
    off = int(sys.argv[2], 16)
    ln = int(sys.argv[3], 16)
    maxd = int(sys.argv[4]) if len(sys.argv) > 4 else 12
    with open(path, "rb") as fh:
        fh.seek(off)
        b = fh.read(ln)
    show(b, 0, len(b), 0, maxd, off)


if __name__ == "__main__":
    main()
