#!/usr/bin/env python3
"""Heuristically locate FMLayoutPB.LayoutObject records inside .fmp12 files
and report, per LayoutObject.type value, which type-specific sub-message is
populated.  Files are read as bytes only.  This is a corpus probe, not a file
format parser: it finds `0x12 <len> 0x08 ...` wrappers (LayoutObjectStore
field 2) whose payload parses as a LayoutObject with a `bounds` Rect.

Usage:
  uv run --isolated --with protobuf python -I tools/fmlayoutpb_probe.py \
      --blobs re/work/protobuf/scan/blobs/FMEngine.x86_64 FILE.fmp12 [...]
"""
import argparse
import collections
import re

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

ap = argparse.ArgumentParser()
ap.add_argument("--blobs", required=True)
ap.add_argument("files", nargs="+")
args = ap.parse_args()

pool = descriptor_pool.DescriptorPool()
for n in ("Style", "LayoutObject"):
    with open(f"{args.blobs}/{n}.proto.pb", "rb") as fh:
        pool.Add(descriptor_pb2.FileDescriptorProto.FromString(fh.read()))
LO = message_factory.GetMessageClass(pool.FindMessageTypeByName("FMLayoutPB.LayoutObject"))
SUB = [f.name for f in LO.DESCRIPTOR.fields if f.name.endswith("Obj")]
rect = re.compile(rb"\x22\x24\x09.{8}\x11.{8}\x19.{8}\x21.{8}", re.S)

by_type = collections.defaultdict(collections.Counter)
style_types = collections.defaultdict(collections.Counter)
total = 0
for path in args.files:
    with open(path, "rb") as fh:
        d = fh.read()
    for m in rect.finditer(d):
        r = m.start()
        done = False
        for back in range(2, 40):
            s = r - back
            if d[s] != 0x08:
                continue
            for lb in (1, 2):
                t = s - 1 - lb
                if t < 0 or d[t] != 0x12:
                    continue
                if lb == 1:
                    if d[t + 1] & 0x80:
                        continue
                    ln = d[t + 1]
                else:
                    if not d[t + 1] & 0x80 or d[t + 2] & 0x80:
                        continue
                    ln = (d[t + 1] & 0x7F) | (d[t + 2] << 7)
                if s + ln < m.end():
                    continue
                try:
                    msg = LO.FromString(d[s:s + ln])
                except Exception:
                    continue
                if msg.ByteSize() != ln or not msg.HasField("bounds"):
                    continue
                subs = [n for n in SUB if msg.HasField(n)] or ["(none)"]
                by_type[msg.type][",".join(subs)] += 1
                if msg.HasField("style"):
                    style_types[msg.type][msg.style.objectTypeId] += 1
                total += 1
                done = True
                break
            if done:
                break

print(f"decoded {total} LayoutObject records from {len(args.files)} files")
for t in sorted(by_type):
    print(f"type {t:>3}: {dict(by_type[t].most_common(3))}  style.objectTypeId {dict(style_types[t].most_common(4))}")
