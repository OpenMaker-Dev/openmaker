#!/usr/bin/env python3
"""Extract embedded serialized google.protobuf.FileDescriptorProto blobs from
binaries (Mach-O thin or fat, PE, or any raw file) and regenerate .proto
source for the ones that are not well-known third-party schemas.

The binaries are treated as untrusted data: they are only read as bytes, never
loaded or executed.

How it works
  1. Every occurrence of b".proto" is a potential end of a `name` field.  We
     look backwards for `0x0a <varint len>` whose length ends exactly there.
     Each such position is a candidate descriptor start.
  2. From a candidate start we walk the top-level wire fields as long as they
     are legal FileDescriptorProto fields with the right wire type, in
     non-decreasing field-number order (protoc serializes in field order),
     with singular fields at most once.  Every field boundary is a possible
     end of the blob.
  3. The longest prefix that parses with descriptor_pb2 and carries no unknown
     fields outside *Options messages wins.
  4. Blobs are de-duplicated per (container, slice) and written to
     <out>/blobs/<label>/<proto path>.pb with a manifest.json.
  5. For non-third-party files, .proto text is regenerated into --proto-out.
     Custom options are decoded with a descriptor pool built from every
     extracted file, so extension options print by name.

Both C++ libprotobuf (`descriptor_table_protodef_*`) and upb (`*_upbdefs.c`
`descriptor[]`) embed the same serialized FileDescriptorProto bytes, so one
scanner covers both.

Usage
  uv run --isolated --with protobuf python -I tools/protobuf_extract.py \
      --out re/work/protobuf/scan --proto-out docs/re/protobuf \
      --scan-dir "re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app" \
      --scan-dir "re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro" \
      [<binary> ...]
--scan-dir walks a tree and selects every Mach-O (thin or fat) and PE file by
magic bytes.  Explicit binaries can be given in addition or instead.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import struct
import sys
from collections import defaultdict

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory, text_format
from google.protobuf.unknown_fields import UnknownFieldSet

THIRD_PARTY_PREFIXES = (
    "google/", "envoy/", "xds/", "udpa/", "validate/", "grpc/", "opencensus/",
    "src/proto/grpc/", "net/proto2/", "third_party/", "cel/", "io/prometheus/",
)

# FileDescriptorProto top-level fields: number -> allowed wire types, singular?
FDP_FIELDS = {
    1: ({2}, True),     # name
    2: ({2}, True),     # package
    3: ({2}, False),    # dependency
    4: ({2}, False),    # message_type
    5: ({2}, False),    # enum_type
    6: ({2}, False),    # service
    7: ({2}, False),    # extension
    8: ({2}, True),     # options
    9: ({2}, True),     # source_code_info
    10: ({0, 2}, False),  # public_dependency
    11: ({0, 2}, False),  # weak_dependency
    12: ({2}, True),    # syntax
    14: ({0}, True),    # edition
    15: ({2}, False),   # option_dependency (newer descriptor.proto)
}

NAME_RE = re.compile(rb"[A-Za-z0-9_][A-Za-z0-9_./-]*\.proto\Z")
CPU_NAMES = {0x01000007: "x86_64", 0x0100000C: "arm64", 7: "i386", 12: "arm"}


# ---------------------------------------------------------------- containers

def slices(data: bytes):
    """Yield (arch_label, offset, length) for each slice of the container."""
    magic = data[:4]
    if magic in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        is64 = magic == b"\xca\xfe\xba\xbf"
        nfat = struct.unpack(">I", data[4:8])[0]
        if 0 < nfat < 32:  # avoid confusing Java class files with fat Mach-O
            pos = 8
            out = []
            for _ in range(nfat):
                if is64:
                    cpu, _sub, off, size, _al, _res = struct.unpack(">iiQQII", data[pos:pos + 32])
                    pos += 32
                else:
                    cpu, _sub, off, size, _al = struct.unpack(">iiIII", data[pos:pos + 20])
                    pos += 20
                out.append((CPU_NAMES.get(cpu & 0xFFFFFFFF, hex(cpu)), off, size))
            if all(o + s <= len(data) for _, o, s in out):
                yield from out
                return
    if magic in (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe"):
        cpu = struct.unpack("<I", data[4:8])[0]
        yield (CPU_NAMES.get(cpu, hex(cpu)), 0, len(data))
        return
    if data[:2] == b"MZ":
        yield ("pe", 0, len(data))
        return
    yield ("raw", 0, len(data))


# ------------------------------------------------------------------ wire walk

def read_varint(buf, pos, end):
    result = 0
    shift = 0
    while pos < end and shift < 64:
        b = buf[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, pos
        shift += 7
    return None, pos


def candidate_starts(buf, lo, hi):
    """Positions of `0x0a <len> name.proto` inside buf[lo:hi]."""
    found = set()
    idx = buf.find(b".proto", lo, hi)
    while idx != -1:
        end = idx + 6
        for length in range(7, min(400, end - lo)):
            s = end - length
            if length < 128:
                if s - 2 < lo or buf[s - 1] != length or buf[s - 2] != 0x0A:
                    continue
                start = s - 2
            else:
                if s - 3 < lo:
                    continue
                if buf[s - 2] != (length & 0x7F) | 0x80 or buf[s - 1] != length >> 7 or buf[s - 3] != 0x0A:
                    continue
                start = s - 3
            if NAME_RE.match(bytes(buf[s:end])):
                found.add(start)
        idx = buf.find(b".proto", idx + 1, hi)
    return sorted(found)


def walk_top_level(buf, start, hi):
    """Return the list of field-boundary end offsets reachable from start."""
    pos = start
    last_field = 0
    seen = set()
    ends = []
    while pos < hi:
        tag, p = read_varint(buf, pos, hi)
        if tag is None or tag == 0:
            break
        field, wt = tag >> 3, tag & 7
        spec = FDP_FIELDS.get(field)
        if spec is None or wt not in spec[0]:
            break
        if field < last_field:
            break
        if spec[1] and field in seen:
            break
        if wt == 0:
            v, p = read_varint(buf, p, hi)
            if v is None:
                break
        else:
            ln, p = read_varint(buf, p, hi)
            if ln is None or p + ln > hi:
                break
            p += ln
        if field == 1 and pos != start:
            break
        seen.add(field)
        last_field = field
        pos = p
        ends.append(pos)
    return ends


def has_stray_unknowns(msg) -> bool:
    """True if a non-Options message in the tree has unknown fields."""
    name = msg.DESCRIPTOR.name
    if not name.endswith("Options") and name != "UninterpretedOption":
        if len(UnknownFieldSet(msg)):
            return True
    else:
        return False
    for fd, value in msg.ListFields():
        if fd.type != fd.TYPE_MESSAGE:
            continue
        if fd.is_repeated:
            if any(has_stray_unknowns(v) for v in value):
                return True
        elif has_stray_unknowns(value):
            return True
    return False


def extract_at(buf, start, hi):
    ends = walk_top_level(buf, start, hi)
    for end in reversed(ends):
        blob = bytes(buf[start:end])
        fdp = descriptor_pb2.FileDescriptorProto()
        try:
            fdp.ParseFromString(blob)
        except Exception:
            continue
        if not fdp.name.endswith(".proto"):
            continue
        if has_stray_unknowns(fdp):
            continue
        return blob, fdp
    return None, None


def scan_file(path):
    with open(path, "rb") as fh:
        data = fh.read()
    results = []
    for arch, off, size in slices(data):
        hi = off + size
        mv = memoryview(data)
        taken_until = -1
        for start in candidate_starts(data, off, hi):
            if start < taken_until:
                # Inside an already extracted blob (e.g. a dependency string).
                continue
            blob, fdp = extract_at(mv, start, hi)
            if blob is None:
                continue
            # Require at least one structural element besides the name, or the
            # name alone is just a string that happens to be preceded by 0x0a.
            if len(fdp.ListFields()) < 2:
                continue
            taken_until = start + len(blob)
            results.append({
                "arch": arch,
                "file_offset": start,
                "slice_offset": start - off,
                "length": len(blob),
                "sha256": hashlib.sha256(blob).hexdigest(),
                "name": fdp.name,
                "package": fdp.package,
                "syntax": fdp.syntax or "proto2",
                "dependencies": list(fdp.dependency),
                "messages": len(fdp.message_type),
                "enums": len(fdp.enum_type),
                "services": [s.name for s in fdp.service],
                "blob": blob,
            })
    return results


def is_third_party(name):
    return name.startswith(THIRD_PARTY_PREFIXES)


# ------------------------------------------------------------------ rendering

SCALAR_NAMES = {
    1: "double", 2: "float", 3: "int64", 4: "uint64", 5: "int32", 6: "fixed64",
    7: "fixed32", 8: "bool", 9: "string", 12: "bytes", 13: "uint32",
    15: "sfixed32", 16: "sfixed64", 17: "sint32", 18: "sint64",
}
F = descriptor_pb2.FieldDescriptorProto


def default_json_name(name):
    out = []
    upper = False
    for ch in name:
        if ch == "_":
            upper = True
        elif upper:
            out.append(ch.upper())
            upper = False
        else:
            out.append(ch)
    return "".join(out)


def c_escape(data: bytes) -> str:
    out = []
    for b in data:
        if b == 0x22:
            out.append('\\"')
        elif b == 0x5C:
            out.append("\\\\")
        elif b == 0x27:
            out.append("\\'")
        elif b == 0x0A:
            out.append("\\n")
        elif b == 0x0D:
            out.append("\\r")
        elif b == 0x09:
            out.append("\\t")
        elif 0x20 <= b < 0x7F:
            out.append(chr(b))
        else:
            out.append("\\%03o" % b)
    return "".join(out)


class SymbolTable:
    """Full names of every symbol known from the extracted descriptors."""

    def __init__(self):
        self.kinds = {}  # full name -> "package" | "message" | "enum" | "service" | "value" | "field"

    def add_file(self, fdp):
        if fdp.package:
            parts = fdp.package.split(".")
            for i in range(1, len(parts) + 1):
                self.kinds.setdefault(".".join(parts[:i]), "package")
        prefix = fdp.package
        for m in fdp.message_type:
            self._add_msg(prefix, m)
        for e in fdp.enum_type:
            self._add_enum(prefix, e)
        for s in fdp.service:
            self.kinds[self._join(prefix, s.name)] = "service"
        for x in fdp.extension:
            self.kinds[self._join(prefix, x.name)] = "field"

    @staticmethod
    def _join(a, b):
        return f"{a}.{b}" if a else b

    def _add_msg(self, prefix, m):
        full = self._join(prefix, m.name)
        self.kinds[full] = "message"
        for f in m.field:
            self.kinds.setdefault(self._join(full, f.name), "field")
        for x in m.extension:
            self.kinds.setdefault(self._join(full, x.name), "field")
        for o in m.oneof_decl:
            self.kinds.setdefault(self._join(full, o.name), "field")
        for n in m.nested_type:
            self._add_msg(full, n)
        for e in m.enum_type:
            self._add_enum(full, e)

    def _add_enum(self, prefix, e):
        full = self._join(prefix, e.name)
        self.kinds[full] = "enum"
        for v in e.value:
            # Enum values are siblings of their enum (C++ scoping rules).
            self.kinds.setdefault(self._join(prefix, v.name), "value")

    def resolve(self, scope, rel):
        """Mimic protoc's relative name lookup from `scope`."""
        first = rel.split(".")[0]
        scope_parts = scope.split(".") if scope else []
        while True:
            base = ".".join(scope_parts)
            cand_first = self._join(base, first)
            kind = self.kinds.get(cand_first)
            if kind is not None and (rel == first or kind in ("package", "message", "enum", "service")):
                full = self._join(base, rel)
                return full if full in self.kinds else None
            if not scope_parts:
                return None
            scope_parts.pop()

    def shortest(self, scope, full_with_dot):
        full = full_with_dot.lstrip(".")
        parts = full.split(".")
        for i in range(len(parts) - 1, -1, -1):
            rel = ".".join(parts[i:])
            if self.resolve(scope, rel) == full:
                return rel
        return full_with_dot  # fully qualified with leading dot is always valid


class Renderer:
    def __init__(self, fdp, symbols, pool, source_note):
        self.fdp = fdp
        self.sym = symbols
        self.pool = pool
        self.note = source_note
        self.lines = []
        self.syntax = fdp.syntax or "proto2"

    # ---- options
    def _options_class(self, opt_name):
        try:
            desc = self.pool.FindMessageTypeByName(f"google.protobuf.{opt_name}")
            return message_factory.GetMessageClass(desc)
        except Exception:
            return None

    def option_items(self, opts):
        """Return list of (name, value_text) for an *Options message."""
        if opts is None or not opts.ByteSize():
            return []
        cls = self._options_class(opts.DESCRIPTOR.name)
        msg = opts
        if cls is not None:
            msg = cls()
            msg.ParseFromString(opts.SerializeToString())
        items = []
        for fd, value in msg.ListFields():
            name = f"({fd.full_name})" if fd.is_extension else fd.name
            if fd.name == "uninterpreted_option":
                continue
            values = value if fd.is_repeated else [value]
            for v in values:
                items.append((name, self.value_text(fd, v)))
        unk = UnknownFieldSet(msg)
        for u in unk:
            items.append((f"/* unknown option field {u.field_number} */", repr(u.data)))
        return items

    def value_text(self, fd, v):
        if fd.type == fd.TYPE_MESSAGE:
            body = text_format.MessageToString(v, as_one_line=True, descriptor_pool=self.pool)
            return "{ " + body + " }" if body else "{}"
        if fd.type == fd.TYPE_ENUM:
            ev = fd.enum_type.values_by_number.get(v)
            return ev.name if ev else str(v)
        if fd.type == fd.TYPE_BOOL:
            return "true" if v else "false"
        if fd.type == fd.TYPE_STRING:
            return '"' + c_escape(v.encode("utf-8")) + '"'
        if fd.type == fd.TYPE_BYTES:
            return '"' + c_escape(v) + '"'
        return repr(v) if isinstance(v, float) else str(v)

    # ---- emit helpers
    def emit(self, indent, text):
        self.lines.append("  " * indent + text if text else "")

    def render(self):
        fdp = self.fdp
        n = self.note
        self.emit(0, f"// Regenerated by tools/protobuf_extract.py from the descriptor embedded in")
        for line in n:
            self.emit(0, f"// {line}")
        self.emit(0, "// Names, numbers, types, labels, oneofs and options are exact (recompiling")
        self.emit(0, "// this file reproduces the embedded descriptor byte for byte, see")
        self.emit(0, "// tools/protobuf_verify.py). Source comments and the interleaving of")
        self.emit(0, "// enums and messages at one nesting level are not recorded in descriptors.")
        self.emit(0, "")
        if self.syntax == "editions":
            self.emit(0, f'edition = "{descriptor_pb2.Edition.Name(fdp.edition).replace("EDITION_", "")}";')
        else:
            self.emit(0, f'syntax = "{self.syntax}";')
        self.emit(0, "")
        if fdp.package:
            self.emit(0, f"package {fdp.package};")
            self.emit(0, "")
        if fdp.dependency:
            pub = set(fdp.public_dependency)
            weak = set(fdp.weak_dependency)
            for i, d in enumerate(fdp.dependency):
                mod = "public " if i in pub else "weak " if i in weak else ""
                self.emit(0, f'import {mod}"{d}";')
            self.emit(0, "")
        opts = self.option_items(fdp.options if fdp.HasField("options") else None)
        for name, val in opts:
            self.emit(0, f"option {name} = {val};")
        if opts:
            self.emit(0, "")
        scope = fdp.package
        first = True
        for e in fdp.enum_type:
            if not first:
                self.emit(0, "")
            self.render_enum(e, 0)
            first = False
        for m in fdp.message_type:
            if not first:
                self.emit(0, "")
            self.render_message(m, scope, 0)
            first = False
        self.render_extensions(fdp.extension, scope, 0, first)
        for s in fdp.service:
            self.emit(0, "")
            self.render_service(s, scope)
        return "\n".join(self.lines).rstrip() + "\n"

    def render_enum(self, e, ind):
        self.emit(ind, f"enum {e.name} {{")
        for name, val in self.option_items(e.options if e.HasField("options") else None):
            self.emit(ind + 1, f"option {name} = {val};")
        for v in e.value:
            vo = self.option_items(v.options if v.HasField("options") else None)
            suffix = " [" + ", ".join(f"{a} = {b}" for a, b in vo) + "]" if vo else ""
            self.emit(ind + 1, f"{v.name} = {v.number}{suffix};")
        for r in e.reserved_range:
            self.emit(ind + 1, f"reserved {self.range_text(r.start, r.end, inclusive=True)};")
        if e.reserved_name:
            self.emit(ind + 1, "reserved " + ", ".join(f'"{x}"' for x in e.reserved_name) + ";")
        self.emit(ind, "}")

    @staticmethod
    def range_text(start, end, inclusive):
        last = end if inclusive else end - 1
        if last >= 536870911 or end == 2147483647:
            return f"{start} to max"
        return str(start) if last == start else f"{start} to {last}"

    def type_text(self, f, scope):
        if f.type in (F.TYPE_MESSAGE, F.TYPE_ENUM, F.TYPE_GROUP):
            return self.sym.shortest(scope, f.type_name)
        return SCALAR_NAMES[f.type]

    def field_text(self, f, scope, map_entries, in_oneof):
        opts = []
        if f.HasField("default_value"):
            if f.type == F.TYPE_STRING:
                dv = '"' + c_escape(f.default_value.encode("utf-8")) + '"'
            elif f.type == F.TYPE_BYTES:
                dv = '"' + f.default_value + '"'  # already C-escaped in descriptors
            else:
                dv = f.default_value
            opts.append(("default", dv))
        if f.HasField("json_name") and f.json_name != default_json_name(f.name):
            opts.append(("json_name", f'"{f.json_name}"'))
        opts += self.option_items(f.options if f.HasField("options") else None)
        suffix = " [" + ", ".join(f"{a} = {b}" for a, b in opts) + "]" if opts else ""

        entry = map_entries.get(f.type_name) if f.type == F.TYPE_MESSAGE else None
        if entry is not None and f.label == F.LABEL_REPEATED:
            k = next(x for x in entry.field if x.number == 1)
            v = next(x for x in entry.field if x.number == 2)
            return f"map<{self.type_text(k, scope)}, {self.type_text(v, scope)}> {f.name} = {f.number}{suffix};"

        label = ""
        if f.label == F.LABEL_REPEATED:
            label = "repeated "
        elif in_oneof:
            label = ""
        elif self.syntax == "proto2":
            label = "required " if f.label == F.LABEL_REQUIRED else "optional "
        elif f.proto3_optional:
            label = "optional "
        return f"{label}{self.type_text(f, scope)} {f.name} = {f.number}{suffix};"

    def render_message(self, m, scope, ind):
        full = f"{scope}.{m.name}" if scope else m.name
        self.emit(ind, f"message {m.name} {{")
        for name, val in self.option_items(m.options if m.HasField("options") else None):
            self.emit(ind + 1, f"option {name} = {val};")

        map_entries = {}
        for n in m.nested_type:
            if n.options.map_entry:
                map_entries[f".{full}.{n.name}"] = n

        for e in m.enum_type:
            self.render_enum(e, ind + 1)
        for n in m.nested_type:
            if n.options.map_entry:
                continue
            self.render_message(n, full, ind + 1)

        # Real oneofs: those referenced by a field that is not proto3_optional.
        real_oneofs = {f.oneof_index for f in m.field if f.HasField("oneof_index") and not f.proto3_optional}
        emitted_oneofs = set()
        for f in m.field:
            if f.HasField("oneof_index") and f.oneof_index in real_oneofs:
                oi = f.oneof_index
                if oi in emitted_oneofs:
                    continue
                emitted_oneofs.add(oi)
                od = m.oneof_decl[oi]
                self.emit(ind + 1, f"oneof {od.name} {{")
                for name, val in self.option_items(od.options if od.HasField("options") else None):
                    self.emit(ind + 2, f"option {name} = {val};")
                for g in m.field:
                    if g.HasField("oneof_index") and g.oneof_index == oi:
                        self.emit(ind + 2, self.field_text(g, full, map_entries, True))
                self.emit(ind + 1, "}")
            else:
                self.emit(ind + 1, self.field_text(f, full, map_entries, False))

        for r in m.extension_range:
            ro = self.option_items(r.options if r.HasField("options") else None)
            suffix = " [" + ", ".join(f"{a} = {b}" for a, b in ro) + "]" if ro else ""
            self.emit(ind + 1, f"extensions {self.range_text(r.start, r.end, inclusive=False)}{suffix};")
        for r in m.reserved_range:
            self.emit(ind + 1, f"reserved {self.range_text(r.start, r.end, inclusive=False)};")
        if m.reserved_name:
            self.emit(ind + 1, "reserved " + ", ".join(f'"{x}"' for x in m.reserved_name) + ";")
        self.render_extensions(m.extension, full, ind + 1, True)
        self.emit(ind, "}")

    def render_extensions(self, exts, scope, ind, first):
        by_ext = defaultdict(list)
        for x in exts:
            by_ext[x.extendee].append(x)
        for extendee, fields in by_ext.items():
            if not first:
                self.emit(ind, "")
            first = False
            self.emit(ind, f"extend {self.sym.shortest(scope, extendee)} {{")
            for x in fields:
                self.emit(ind + 1, self.field_text(x, scope, {}, False))
            self.emit(ind, "}")

    def render_service(self, s, scope):
        self.emit(0, f"service {s.name} {{")
        for name, val in self.option_items(s.options if s.HasField("options") else None):
            self.emit(1, f"option {name} = {val};")
        for mth in s.method:
            cs = "stream " if mth.client_streaming else ""
            ss = "stream " if mth.server_streaming else ""
            it = self.sym.shortest(scope, mth.input_type)
            ot = self.sym.shortest(scope, mth.output_type)
            mo = self.option_items(mth.options if mth.HasField("options") else None)
            if mo:
                self.emit(1, f"rpc {mth.name}({cs}{it}) returns ({ss}{ot}) {{")
                for name, val in mo:
                    self.emit(2, f"option {name} = {val};")
                self.emit(1, "}")
            elif mth.HasField("options"):
                # protoc records an empty MethodOptions when the source used
                # a `{}` body instead of `;`.  Keep that for byte fidelity.
                self.emit(1, f"rpc {mth.name}({cs}{it}) returns ({ss}{ot}) {{}}")
            else:
                self.emit(1, f"rpc {mth.name}({cs}{it}) returns ({ss}{ot});")
        self.emit(0, "}")


def build_pool(fdps):
    """Pool with descriptor.proto plus every extracted file that loads."""
    pool = descriptor_pool.DescriptorPool()
    base = descriptor_pb2.FileDescriptorProto()
    descriptor_pb2.DESCRIPTOR.CopyToProto(base)
    pool.Add(base)
    loaded = {base.name}
    pending = {f.name: f for f in fdps if f.name not in loaded}
    progress = True
    while pending and progress:
        progress = False
        for name in list(pending):
            f = pending[name]
            if all(d in loaded for d in f.dependency):
                try:
                    pool.Add(f)
                    loaded.add(name)
                except Exception as exc:  # conflicting duplicate etc.
                    print(f"pool: skip {name}: {exc}", file=sys.stderr)
                del pending[name]
                progress = True
    return pool, sorted(pending)


# ---------------------------------------------------------------------- main

BIN_MAGICS = (b"\xca\xfe\xba\xbe", b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe")


def find_binaries(root):
    out = []
    for d, _, files in os.walk(root):
        for f in sorted(files):
            p = os.path.join(d, f)
            if os.path.islink(p) or not os.path.isfile(p):
                continue
            with open(p, "rb") as fh:
                magic = fh.read(4)
            if magic in BIN_MAGICS or magic[:2] == b"MZ":
                out.append(p)
    return sorted(out)


def label_for(path, arch):
    base = os.path.basename(path)
    return base if arch in ("pe", "raw") else f"{base}.{arch}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True, help="scratch dir for blobs and manifest.json")
    ap.add_argument("--proto-out", help="dir for regenerated non-third-party .proto files")
    ap.add_argument("--prefer", default="DBEngine.x86_64,FMEngine.x86_64",
                    help="comma list of labels preferred as the rendering source")
    ap.add_argument("--scan-dir", action="append", default=[],
                    help="walk this tree and scan every Mach-O and PE file in it")
    ap.add_argument("binaries", nargs="*")
    args = ap.parse_args()
    for root in args.scan_dir:
        args.binaries.extend(find_binaries(root))
    if not args.binaries:
        ap.error("no binaries given")

    manifest = []
    by_name = defaultdict(list)  # proto name -> list of entries
    for path in args.binaries:
        res = scan_file(path)
        for r in res:
            label = label_for(path, r["arch"])
            r["label"] = label
            r["path"] = path
            blob = r.pop("blob")
            dst = os.path.join(args.out, "blobs", label, r["name"] + ".pb")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, "wb") as fh:
                fh.write(blob)
            r["third_party"] = is_third_party(r["name"])
            manifest.append(r)
            by_name[r["name"]].append((r, blob))
        n_fm = sum(1 for r in res if not is_third_party(r["name"]))
        print(f"{path}: {len(res)} descriptors ({n_fm} non-third-party)", file=sys.stderr)

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=1)

    # Variant report: same proto name with differing bytes across binaries.
    variants = {}
    for name, entries in sorted(by_name.items()):
        hashes = defaultdict(list)
        for r, _ in entries:
            hashes[r["sha256"]].append(r["label"])
        variants[name] = {h: sorted(set(v)) for h, v in hashes.items()}
    with open(os.path.join(args.out, "variants.json"), "w") as fh:
        json.dump(variants, fh, indent=1, sort_keys=True)

    if not args.proto_out:
        return
    prefer = args.prefer.split(",")

    def pick(entries):
        for p in prefer:
            for r, blob in entries:
                if r["label"] == p:
                    return r, blob
        return entries[0]

    chosen = {}
    for name, entries in by_name.items():
        r, blob = pick(entries)
        fdp = descriptor_pb2.FileDescriptorProto()
        fdp.ParseFromString(blob)
        chosen[name] = (r, fdp)
    # Only one variant per name can enter the pool; FileMaker names like
    # common.proto could in theory collide, so pool every chosen variant.
    pool, unresolved = build_pool([f for _, f in chosen.values()])
    if unresolved:
        print(f"pool: unresolved dependencies for {unresolved}", file=sys.stderr)
    symbols = SymbolTable()
    for _, fdp in chosen.values():
        symbols.add_file(fdp)

    for name, (r, fdp) in sorted(chosen.items()):
        if r["third_party"]:
            continue
        labels = sorted({x["label"] for x, _ in by_name[name]})
        nvar = len(variants[name])
        note = [
            f"{r['label']} at slice offset 0x{r['slice_offset']:x} ({r['length']} bytes, sha256 {r['sha256'][:16]}...).",
            f"Present in: {', '.join(labels)}.",
            f"Distinct serialized variants across binaries: {nvar}.",
        ]
        text = Renderer(fdp, symbols, pool, note).render()
        dst = os.path.join(args.proto_out, name)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, "w") as fh:
            fh.write(text)
        print(f"wrote {dst}", file=sys.stderr)


if __name__ == "__main__":
    main()
