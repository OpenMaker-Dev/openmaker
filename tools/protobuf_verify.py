#!/usr/bin/env python3
"""Round-trip check for regenerated .proto files.

Compiles every .proto under --proto-dir with protoc (from grpcio-tools) and
compares each resulting FileDescriptorProto with the serialized descriptor
that tools/protobuf_extract.py pulled out of the binary.  Differences are
printed; exit status is non-zero if any file differs.

Usage
  uv run --isolated --with protobuf --with grpcio-tools python -I \
      tools/protobuf_verify.py --proto-dir docs/re/protobuf \
      --blob-dir re/work/protobuf/scan/blobs/DBEngine.x86_64 \
      --blob-dir re/work/protobuf/scan/blobs/FMEngine.x86_64
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile

from google.protobuf import descriptor_pb2


def normalize(fdp):
    fdp = descriptor_pb2.FileDescriptorProto.FromString(fdp.SerializeToString())
    fdp.ClearField("source_code_info")

    def walk_msg(m):
        for f in list(m.field) + list(m.extension):
            f.ClearField("json_name")  # not present in embedded descriptors
        for n in m.nested_type:
            walk_msg(n)

    for m in fdp.message_type:
        walk_msg(m)
    for x in fdp.extension:
        x.ClearField("json_name")
    return fdp


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--proto-dir", required=True)
    ap.add_argument("--blob-dir", action="append", required=True)
    args = ap.parse_args()

    import grpc_tools
    from grpc_tools import protoc

    well_known = os.path.join(os.path.dirname(grpc_tools.__file__), "_proto")
    protos = []
    for d, _, fs in os.walk(args.proto_dir):
        for f in fs:
            if f.endswith(".proto"):
                protos.append(os.path.relpath(os.path.join(d, f), args.proto_dir))
    protos.sort()

    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "set.pb")
        rc = protoc.main([
            "protoc", f"-I{args.proto_dir}", f"-I{well_known}",
            f"--descriptor_set_out={out}", "--include_imports", *protos,
        ])
        if rc != 0:
            print("protoc failed", file=sys.stderr)
            return 2
        fds = descriptor_pb2.FileDescriptorSet.FromString(open(out, "rb").read())

    compiled = {f.name: f for f in fds.file}
    bad = 0
    for name in protos:
        blob = None
        for bd in args.blob_dir:
            p = os.path.join(bd, name + ".pb")
            if os.path.exists(p):
                blob = open(p, "rb").read()
                break
        if blob is None:
            print(f"SKIP {name}: no extracted blob")
            continue
        orig = normalize(descriptor_pb2.FileDescriptorProto.FromString(blob))
        mine = normalize(compiled[name])
        if orig == mine:
            # Generated code embeds descriptors without json_name (protoc only
            # adds it to --descriptor_set_out), so strip it before comparing
            # bytes.
            exact = mine.SerializeToString() == blob
            print(f"OK   {name}" + ("  (byte-identical after json_name strip)" if exact else "  (semantically equal)"))
        else:
            bad += 1
            print(f"DIFF {name}")
            a = str(orig).splitlines()
            b = str(mine).splitlines()
            import difflib
            for line in list(difflib.unified_diff(a, b, "extracted", "regenerated", lineterm=""))[:40]:
                print("   ", line)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
