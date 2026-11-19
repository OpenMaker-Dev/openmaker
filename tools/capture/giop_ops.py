"""List the RCFMP12 operations in one captured connection: headers only.

For each GIOP Request it prints the direction, request id, operation name, and,
for Perform, the RCTransaction opcode (byte 0 of the inout octet sequence) and
its length. Replies are matched by request id and their status shown. No
payload bytes or string arguments are printed.

Usage: python3 -I tools/capture/giop_ops.py <conn-dir> [--limit N]
"""

import argparse
import collections
import struct
from pathlib import Path

STATUS = {0: "OK", 1: "USER_EXC", 2: "SYS_EXC", 3: "FWD"}


def messages(stream):
    pos = 0
    while pos + 12 <= len(stream) and stream[pos:pos + 4] == b"GIOP":
        little = bool(stream[pos + 6] & 1)
        size = struct.unpack_from("<I" if little else ">I", stream, pos + 8)[0]
        yield stream[pos:pos + 12 + size]
        pos += 12 + size


def u32(data, pos, little):
    pos += (-pos) % 4
    return struct.unpack_from("<I" if little else ">I", data, pos)[0], pos + 4


def request(message):
    little = bool(message[6] & 1)
    request_id, pos = u32(message, 12, little)
    pos += 4                      # response_flags + reserved
    pos += (-pos) % 2
    disposition = struct.unpack_from("<H" if little else ">H", message, pos)[0]
    pos += 2
    if disposition != 0:
        return request_id, "?", None
    key_len, pos = u32(message, pos, little)
    pos += key_len
    op_len, pos = u32(message, pos, little)
    operation = message[pos:pos + op_len].rstrip(b"\0").decode("latin-1")
    pos += op_len
    count, pos = u32(message, pos, little)
    for _ in range(count):
        _, pos = u32(message, pos, little)
        length, pos = u32(message, pos, little)
        pos += length
    pos += (-pos) % 8
    return request_id, operation, (message, pos, little)


def perform_opcode(body):
    """Perform(ulong conn, string s, ulong len, inout OctetSeq). Return (opcode, len)."""
    message, pos, little = body
    _, pos = u32(message, pos, little)            # connection id
    slen, pos = u32(message, pos, little)         # string
    pos += slen
    _, pos = u32(message, pos, little)            # len argument
    seq_len, pos = u32(message, pos, little)      # octet sequence length
    if seq_len == 0 or pos >= len(message):
        return None, seq_len
    return message[pos], seq_len


def reply_status(message):
    little = bool(message[6] & 1)
    request_id, pos = u32(message, 12, little)
    status, _ = u32(message, pos, little)
    return request_id, status


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("conn_dir")
    parser.add_argument("--limit", type=int, default=0)
    args = parser.parse_args()
    calls = []
    replies = {}
    for direction in ("c2s", "s2c"):
        stream = (Path(args.conn_dir) / f"{direction}.bin").read_bytes()
        for message in messages(stream):
            kind = message[7]
            if kind == 0:
                request_id, operation, body = request(message)
                opcode = size = None
                if operation == "Perform" and body:
                    opcode, size = perform_opcode(body)
                calls.append((direction, request_id, operation, opcode, size))
            elif kind == 1:
                request_id, status = reply_status(message)
                replies[(direction, request_id)] = status

    opposite = {"c2s": "s2c", "s2c": "c2s"}
    counts = collections.Counter()
    for index, (direction, request_id, operation, opcode, size) in enumerate(calls):
        status = replies.get((opposite[direction], request_id))
        key = f"{direction} {operation}" + (f" op{opcode}" if opcode is not None else "")
        counts[key] += 1
        if args.limit and index < args.limit:
            print(f"{index:4d} {direction} id={request_id:<5d} {operation:<10s}"
                  f" op={'' if opcode is None else opcode!s:<4s} seq={size!s:<8s}"
                  f" {STATUS.get(status, status)}")
    print("== totals ==")
    for key, value in sorted(counts.items(), key=lambda item: -item[1]):
        print(f"  {value:5d}  {key}")


if __name__ == "__main__":
    main()
