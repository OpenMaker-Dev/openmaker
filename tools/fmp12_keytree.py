#!/usr/bin/env python3
"""Walk the HBAM block/key/slot structure of an fmp12/dls file and dump the
key -> value (slot) tree. Structure-only: values are shown as length + short
preview, not fully decoded.

Usage:
  python3 -I tools/fmp12_keytree.py <file> [--page N] [--max M] [--raw]

Read-only scan of untrusted data; nothing is executed.

Model (derived from HPgHdr/CompletePage DWARF and HPage::FromDiskFmt,
HBAMSlot::SlotUnpackLengths / S_SlotUnpack in libDBEngine.so):

  Each 0x1000 block = HPgHdr(20B) + mFileData(4076B).
  HPgHdr: +0 LSN u32, +4 PrevPage u32, +8 NextPage u32, +0x0c PhysLevel u16,
          +0x0e FreeSpace u16, +0x10 ModCount u32   (all big-endian)
  usedBytes = PAGE_DATA_MAX - FreeSpace, chunks fill mFileData[0:usedBytes].

  Chunk code byte c:
    c == 0x40  -> pop one key segment (end hierarchy)
    c == 0x80  -> no-op marker (begin/continuation); 1 byte
    else       -> data/index slot (optionally a new key segment)
      key-size class  = (c >> 3) & 3 : 0->1B, 1->2B, 2->3B, 3->u8-len-prefixed
      data-size class =  c       & 7 : 0->none,1->1,2->2,3->4,4->6,5->8,
                                       6->u8-len-prefixed, 7->u16-len-prefixed
      bit 0x20 set    -> slot carries a key segment (push: following slots are
                         nested under this key)
      bits 0xC0 set   -> push marker variant (same, top bits stripped)
"""
import struct
import sys

BLOCK = 0x1000
HDR = 0x14
PAGE_DATA_MAX = BLOCK - HDR  # 4076


def u16(b, o):
    return struct.unpack_from(">H", b, o)[0]


def u32(b, o):
    return struct.unpack_from(">I", b, o)[0]


def slot_lengths(buf, start, code=None):
    """Return (keyoff, keylen, dataoff, datalen) for the chunk at buf[start].
    Mirrors HBAMSlot::SlotUnpackLengths. `code` overrides the interpreted code
    byte (callers pass the push-masked value); size prefixes are still read from
    buf at their real offsets."""
    c = buf[start] if code is None else code
    if c in (0x80, 0x40):
        return (0, 1, 0, 1)
    keyoff = 1
    kc = (c >> 3) & 3
    if kc == 0:
        keylen = 1
        dataoff = keyoff + 1
    elif kc == 1:
        keylen = 2
        dataoff = keyoff + 2
    elif kc == 2:
        keylen = 3
        dataoff = keyoff + 3
    else:  # 3: length-prefixed key
        keylen = buf[start + 1]
        keyoff = 2
        dataoff = keyoff + keylen
    dc = c & 7
    if dc == 0:
        datalen = 0
    elif dc == 1:
        datalen = 1
    elif dc == 2:
        datalen = 2
    elif dc == 3:
        datalen = 4
    elif dc == 4:
        datalen = 6
    elif dc == 5:
        datalen = 8
    elif dc == 6:
        datalen = buf[start + dataoff]
        dataoff += 1
    else:  # 7
        datalen = (buf[start + dataoff] << 8) | buf[start + dataoff + 1]
        dataoff += 2
    return (keyoff, keylen, dataoff, datalen)


def keypath_str(stack):
    parts = []
    for seg in stack:
        if len(seg) <= 4:
            n = int.from_bytes(seg, "big")
            parts.append(str(n))
        else:
            parts.append(seg.hex())
    return "/".join(parts)


def decode_page(data, pageno, args):
    base = pageno * BLOCK
    hdr = data[base:base + HDR]
    lsn, prev, nxt = u32(hdr, 0), u32(hdr, 4), u32(hdr, 8)
    level, free = u16(hdr, 0xc), u16(hdr, 0xe)
    modc = u32(hdr, 0x10)
    used = PAGE_DATA_MAX - free
    fdata = data[base + HDR: base + BLOCK]
    print(f"\n# page {pageno} lvl={level} prev={prev} next={nxt} free={free} "
          f"used={used} lsn={lsn} mod={modc}")
    if level == 0xffff or used <= 0 or used > PAGE_DATA_MAX:
        print("  (empty/free page)")
        return nxt
    stack = []
    o = 0
    n = 0
    while o < used:
        c = fdata[o]
        if c == 0x40:
            if stack:
                stack.pop()
            o += 1
            continue
        if c == 0x80:
            o += 1
            continue
        push = (c & 0xc0) == 0xc0 or (c & 0x20) != 0
        mc = c & 0x3f  # strip push top bits for size decode
        keyoff, keylen, dataoff, datalen = slot_lengths(fdata, o, code=mc)
        seg = fdata[o + keyoff:o + keyoff + keylen]
        total = dataoff + datalen
        val = fdata[o + dataoff:o + dataoff + datalen]
        if push:
            stack.append(seg)
            fullkey = keypath_str(stack)
        else:
            fullkey = keypath_str(stack + [seg])
        kind = "idx" if level > 0 else ("node" if push else "leaf")
        if args.get("raw") or n < args.get("max", 40):
            prev_s = ""
            if datalen:
                if level > 0:
                    prev_s = f"child_page={int.from_bytes(val, 'big')}"
                else:
                    printable = all(32 <= x < 127 for x in val[:16])
                    xall = bytes(x ^ 0x5A for x in val[:24])
                    xprint = all(32 <= x < 127 for x in xall[:8])
                    prev_s = (f"dlen={datalen} raw={val[:16].hex()}"
                              + (f" ascii={val[:16].decode('ascii','replace')!r}" if printable else "")
                              + (f" xor5A={xall.decode('ascii','replace')!r}" if xprint else ""))
            print(f"  c={c:#04x} {kind:4} key={fullkey:<24} {prev_s}")
        o += total
        n += 1
    if o != used:
        print(f"  !! desync: consumed {o} of {used}")
    return nxt


def main():
    args = {"max": 40}
    pos = []
    i = 2
    argv = sys.argv
    while i < len(argv):
        if argv[i] == "--page":
            args["page"] = int(argv[i + 1]); i += 2
        elif argv[i] == "--max":
            args["max"] = int(argv[i + 1]); i += 2
        elif argv[i] == "--raw":
            args["raw"] = True; i += 1
        else:
            pos.append(argv[i]); i += 1
    path = argv[1]
    with open(path, "rb") as fh:
        data = fh.read()
    nblocks = len(data) // BLOCK
    if "page" in args:
        decode_page(data, args["page"], args)
        return
    # find leaf chain head: a level-0 page with prev==0
    head = None
    for b in range(1, nblocks):
        base = b * BLOCK
        if u16(data, base + 0xc) == 0 and u32(data, base + 4) == 0:
            head = b
            break
    print(f"file={path} blocks={nblocks} leaf-chain head={head}")
    cur = head
    seen = set()
    while cur and cur not in seen and cur < nblocks:
        seen.add(cur)
        cur = decode_page(data, cur, args)


if __name__ == "__main__":
    main()
