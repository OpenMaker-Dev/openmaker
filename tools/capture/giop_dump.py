"""Split a captured FileMaker byte stream into GIOP messages and describe them.

Prints each message's header and, for Requests and Replies, the GIOP 1.2
request header (request id, target object key, operation, service contexts,
reply status) followed by a hex dump of the body.

Before printing, these are masked as "*": the test password from .env
(UTF-8, UTF-16LE/BE, and FileMaker's XOR 0x5A string obfuscation), anything
shaped like a license key, and authID token values. A password transformed
any other way (hashed, encrypted) cannot be detected, and MAC addresses and
hostnames are not masked, so treat client->server output as sensitive anyway.

Usage: python3 -I tools/capture/giop_dump.py <session-conn-dir> [--env .env] [--max-body N]
"""

import argparse
import re
import struct
from pathlib import Path

XOR_KEY = 0x5A
LICENSE_KEY = re.compile(rb"[0-9A-Z]{5}(?:-[0-9A-Z]{5}){4,}")
AUTH_ID = re.compile(rb"authID=([^;\x00]*)")

MESSAGE_TYPES = {0: "Request", 1: "Reply", 2: "CancelRequest", 3: "LocateRequest",
                 4: "LocateReply", 5: "CloseConnection", 6: "MessageError", 7: "Fragment"}
REPLY_STATUS = {0: "NO_EXCEPTION", 1: "USER_EXCEPTION", 2: "SYSTEM_EXCEPTION",
                3: "LOCATION_FORWARD", 4: "LOCATION_FORWARD_PERM", 5: "NEEDS_ADDRESSING_MODE"}


def load_secret(env_path):
    for line in Path(env_path).read_text().splitlines():
        if line.startswith("FM_TEST_PASS="):
            return line.split("=", 1)[1].strip()
    return None


def mask(data, secret):
    out = bytearray(data)
    ranges = []
    for view in (data, bytes(b ^ XOR_KEY for b in data)):
        if secret:
            for encoded in (secret.encode(), secret.encode("utf-16-le"), secret.encode("utf-16-be")):
                ranges += [(m.start(), m.end()) for m in re.finditer(re.escape(encoded), view)]
        ranges += [(m.start(), m.end()) for m in LICENSE_KEY.finditer(view)]
        ranges += [(m.start(1), m.end(1)) for m in AUTH_ID.finditer(view)]
    for start, end in ranges:
        out[start:end] = b"*" * (end - start)
    return bytes(out)


class Reader:
    """CDR reader; alignment is relative to the start of the GIOP message."""

    def __init__(self, data, little, pos):
        self.data, self.little, self.pos = data, little, pos

    def align(self, n):
        self.pos += (-self.pos) % n

    def u32(self):
        self.align(4)
        value = struct.unpack_from("<I" if self.little else ">I", self.data, self.pos)[0]
        self.pos += 4
        return value

    def u16(self):
        self.align(2)
        value = struct.unpack_from("<H" if self.little else ">H", self.data, self.pos)[0]
        self.pos += 2
        return value

    def octets(self, n):
        value = self.data[self.pos:self.pos + n]
        self.pos += n
        return value

    def sequence(self):
        return self.octets(self.u32())

    def string(self):
        return self.sequence().rstrip(b"\0").decode("latin-1")


def hexdump(data, base=0, limit=None):
    shown = data if limit is None else data[:limit]
    lines = []
    for i in range(0, len(shown), 16):
        chunk = shown[i:i + 16]
        hexpart = " ".join(f"{b:02x}" for b in chunk)
        text = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        lines.append(f"      {base + i:06x}  {hexpart:<47}  {text}")
    if limit is not None and len(data) > limit:
        lines.append(f"      ... {len(data) - limit} more bytes")
    return "\n".join(lines)


def service_contexts(r):
    contexts = []
    for _ in range(r.u32()):
        context_id = r.u32()
        contexts.append((context_id, len(r.sequence())))
    return contexts


def describe(message, max_body):
    little = bool(message[6] & 1)
    kind = message[7]
    lines = [f"  GIOP {message[4]}.{message[5]} {MESSAGE_TYPES.get(kind, kind)} "
             f"flags=0x{message[6]:02x} size={len(message) - 12}"]
    r = Reader(message, little, 12)
    try:
        if kind == 0 and message[5] >= 2:
            request_id = r.u32()
            response_flags = r.octets(1)[0]
            r.octets(3)
            disposition = r.u16()
            key = r.sequence() if disposition == 0 else b"<non-key addressing>"
            operation = r.string()
            contexts = service_contexts(r)
            lines.append(f"    request_id={request_id} response_flags={response_flags} "
                         f"key={key!r} op={operation!r} contexts={contexts}")
            r.align(8)
        elif kind == 1 and message[5] >= 2:
            request_id = r.u32()
            status = r.u32()
            contexts = service_contexts(r)
            lines.append(f"    request_id={request_id} status={REPLY_STATUS.get(status, status)} "
                         f"contexts={contexts}")
            r.align(8)
    except struct.error:
        lines.append("    (header truncated)")
        return "\n".join(lines)
    body = message[r.pos:]
    lines.append(f"    body {len(body)} bytes at message offset 0x{r.pos:x}")
    lines.append(hexdump(body, r.pos, max_body))
    return "\n".join(lines)


def split_messages(stream):
    pos = 0
    while pos + 12 <= len(stream):
        if stream[pos:pos + 4] != b"GIOP":
            yield pos, None, stream[pos:]
            return
        little = bool(stream[pos + 6] & 1)
        size = struct.unpack_from("<I" if little else ">I", stream, pos + 8)[0]
        yield pos, stream[pos:pos + 12 + size], None
        pos += 12 + size
    if pos < len(stream):
        yield pos, None, stream[pos:]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("conn_dir")
    parser.add_argument("--env", default=".env")
    parser.add_argument("--max-body", type=int, default=512)
    args = parser.parse_args()
    secret = load_secret(args.env)
    for direction in ("c2s", "s2c"):
        stream = (Path(args.conn_dir) / f"{direction}.bin").read_bytes()
        print(f"== {direction}: {len(stream)} bytes ==")
        # Split before masking, and mask each message on its own: an authID
        # value runs to the next ';' or NUL of its XOR view, which can lie
        # many messages later, and masking it across the stream would
        # overwrite the GIOP headers in between.
        for offset, message, trailing in split_messages(stream):
            if message is None:
                print(f"  non-GIOP bytes at stream offset {offset}: {len(trailing)} bytes")
                print(hexdump(mask(trailing, secret), 0, args.max_body))
                continue
            print(f"@{offset}")
            print(describe(mask(message, secret), args.max_body))


if __name__ == "__main__":
    main()
