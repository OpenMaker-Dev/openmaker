"""TLS-terminating relay for capturing FileMaker client/server plaintext.

An official FileMaker Pro client connects to this relay as if it were the
host. The relay presents a certificate the client trusts (for example a
Let's Encrypt certificate for the relay machine's name), opens a verified
TLS connection to the real FileMaker Server, relays bytes both ways, and
records each direction.

Per connection it writes, under <out>/<session>/conn-<n>/:
  c2s.bin, s2c.bin   raw plaintext byte streams, one per direction
  events.jsonl       one record per read: time, direction, stream offset, length
  meta.json          peers, TLS parameters, open/close times

Captures contain credentials and business data. Keep <out> private.

Usage:
  python3 -I tools/capture/fm_tls_proxy.py --listen <relay address>:5003 \
      --upstream <FileMaker Server host>:5003 \
      --cert .secrets/tls/<relay name>.crt \
      --key .secrets/tls/<relay name>.key \
      --upstream-cert .secrets/probe/selfsigned.crt \
      --upstream-key .secrets/probe/selfsigned.key \
      --out re/captures --session <name>
"""

import argparse
import asyncio
import json
import os
import ssl
import sys
import time
from pathlib import Path

READ_SIZE = 65536


def host_port(value):
    host, _, port = value.rpartition(":")
    if not host or not port.isdigit():
        raise argparse.ArgumentTypeError(f"expected host:port, got {value!r}")
    return host, int(port)


def tls_params(ssl_object):
    if ssl_object is None:
        return None
    cipher = ssl_object.cipher()
    return {
        "version": ssl_object.version(),
        "cipher": cipher[0] if cipher else None,
        "alpn": ssl_object.selected_alpn_protocol(),
        "server_hostname": ssl_object.server_hostname,
    }


class Recorder:
    """Writes the byte streams and read events for one relayed connection."""

    def __init__(self, directory):
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=False)
        self.streams = {
            "c2s": open(directory / "c2s.bin", "wb"),
            "s2c": open(directory / "s2c.bin", "wb"),
        }
        self.offsets = {"c2s": 0, "s2c": 0}
        self.events = open(directory / "events.jsonl", "w", encoding="utf-8")

    def record(self, direction, data):
        stream = self.streams[direction]
        stream.write(data)
        stream.flush()
        self.events.write(json.dumps({
            "t": time.time(),
            "dir": direction,
            "offset": self.offsets[direction],
            "len": len(data),
        }) + "\n")
        self.events.flush()
        self.offsets[direction] += len(data)

    def write_meta(self, meta):
        with open(self.directory / "meta.json", "w", encoding="utf-8") as f:
            json.dump(meta, f, indent=2)

    def close(self):
        for stream in self.streams.values():
            stream.close()
        self.events.close()


async def pump(reader, writer, recorder, direction):
    try:
        while data := await reader.read(READ_SIZE):
            recorder.record(direction, data)
            writer.write(data)
            await writer.drain()
    except (ConnectionError, ssl.SSLError, TimeoutError) as error:
        return f"{type(error).__name__}: {error}"
    finally:
        try:
            if writer.can_write_eof():
                writer.write_eof()
            else:
                writer.close()
        except (ConnectionError, ssl.SSLError, OSError, TimeoutError):
            pass
    return "eof"


class Relay:
    def __init__(self, args):
        self.args = args
        self.session_dir = Path(args.out) / args.session
        self.session_dir.mkdir(parents=True, exist_ok=False)
        os.chmod(self.session_dir, 0o700)
        self.next_id = 0

        self.server_ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        self.server_ctx.load_cert_chain(args.cert, args.key)
        # The server side of an SSLObject does not expose the SNI the client
        # sent, so capture it during the handshake, keyed by the SSLObject.
        self.client_sni = {}
        self.server_ctx.sni_callback = self.remember_sni

        self.upstream_ctx = ssl.create_default_context(ssl.Purpose.SERVER_AUTH)
        # FileMaker Server demands a client certificate on 5003 (TLS alert
        # "certificate required" without one) but accepts a self-signed one.
        if args.upstream_cert:
            self.upstream_ctx.load_cert_chain(args.upstream_cert, args.upstream_key)

    def remember_sni(self, ssl_object, server_name, _context):
        self.client_sni[id(ssl_object)] = server_name

    async def handle(self, client_reader, client_writer):
        try:
            await self.relay(client_reader, client_writer)
        except Exception as error:  # keep serving other connections
            log(f"connection handler failed: {type(error).__name__}: {error}")

    async def relay(self, client_reader, client_writer):
        conn_id = self.next_id
        self.next_id += 1
        upstream_host, upstream_port = self.args.upstream
        recorder = Recorder(self.session_dir / f"conn-{conn_id:03d}")
        client_ssl = client_writer.get_extra_info("ssl_object")
        client_tls = tls_params(client_ssl)
        client_tls["server_hostname"] = self.client_sni.pop(id(client_ssl), None)
        meta = {
            "conn": conn_id,
            "opened": time.time(),
            "client_peer": client_writer.get_extra_info("peername"),
            "client_tls": client_tls,
            "upstream": f"{upstream_host}:{upstream_port}",
        }
        log(f"conn {conn_id}: client {meta['client_peer']} {meta['client_tls']}")
        try:
            upstream_reader, upstream_writer = await asyncio.open_connection(
                upstream_host, upstream_port,
                ssl=self.upstream_ctx, server_hostname=upstream_host,
            )
        except (OSError, ssl.SSLError) as error:
            meta["error"] = f"upstream connect failed: {error}"
            log(f"conn {conn_id}: {meta['error']}")
            client_writer.close()
            recorder.write_meta(meta)
            recorder.close()
            return

        meta["upstream_tls"] = tls_params(upstream_writer.get_extra_info("ssl_object"))
        c2s, s2c = await asyncio.gather(
            pump(client_reader, upstream_writer, recorder, "c2s"),
            pump(upstream_reader, client_writer, recorder, "s2c"),
        )
        meta.update({
            "closed": time.time(),
            "c2s_end": c2s,
            "s2c_end": s2c,
            "c2s_bytes": recorder.offsets["c2s"],
            "s2c_bytes": recorder.offsets["s2c"],
        })
        recorder.write_meta(meta)
        recorder.close()
        log(f"conn {conn_id}: closed c2s={meta['c2s_bytes']}B ({c2s}) s2c={meta['s2c_bytes']}B ({s2c})")


def log(message):
    print(f"{time.strftime('%H:%M:%S')} {message}", flush=True)


async def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--listen", type=host_port, required=True)
    parser.add_argument("--upstream", type=host_port, required=True)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--upstream-cert", help="client certificate to present to the server")
    parser.add_argument("--upstream-key", help="private key for --upstream-cert")
    parser.add_argument("--out", required=True)
    parser.add_argument("--session", required=True)
    args = parser.parse_args()

    relay = Relay(args)
    host, port = args.listen
    server = await asyncio.start_server(relay.handle, host, port, ssl=relay.server_ctx)
    log(f"listening on {host}:{port} -> {args.upstream[0]}:{args.upstream[1]}, writing {relay.session_dir}")
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)
