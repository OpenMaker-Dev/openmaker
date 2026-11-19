"""Send one recorded client->server byte stream to a FileMaker host over TLS and
record whatever comes back, to tell server behavior apart from relay behavior.

Non-destructive: it replays bytes the official client already sent (for example
the GIOP _is_a probe). Do not feed it captured login frames; those carry
credentials. Reply bytes are written next to stdout only.

Usage:
  python3 -I tools/probe/giop_probe.py --host <FileMaker Server host> --port 5003 \
      --send re/captures/<session>/conn-000/c2s.bin --read-seconds 8
"""

import argparse
import socket
import ssl
import sys
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=5003)
    parser.add_argument("--send", required=True, help="file of bytes to send")
    parser.add_argument("--read-seconds", type=float, default=8.0)
    parser.add_argument("--insecure", action="store_true",
                        help="skip certificate verification")
    parser.add_argument("--client-cert", help="PEM client certificate to present")
    parser.add_argument("--client-key", help="PEM private key for --client-cert")
    args = parser.parse_args()

    payload = open(args.send, "rb").read()
    ctx = ssl.create_default_context()
    if args.insecure:
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
    if args.client_cert:
        ctx.load_cert_chain(args.client_cert, args.client_key)

    raw = socket.create_connection((args.host, args.port), timeout=10)
    tls = ctx.wrap_socket(raw, server_hostname=args.host)
    print(f"connected {args.host}:{args.port} {tls.version()} {tls.cipher()[0]}",
          file=sys.stderr)
    tls.sendall(payload)
    print(f"sent {len(payload)} bytes, reading for {args.read_seconds}s",
          file=sys.stderr)

    received = bytearray()
    tls.settimeout(args.read_seconds)
    deadline = time.time() + args.read_seconds
    try:
        while time.time() < deadline:
            chunk = tls.recv(65536)
            if not chunk:
                print("server closed connection", file=sys.stderr)
                break
            received += chunk
    except (TimeoutError, socket.timeout):
        print("read window elapsed", file=sys.stderr)
    except (ConnectionError, ssl.SSLError) as error:
        print(f"{type(error).__name__}: {error}", file=sys.stderr)
    finally:
        try:
            tls.close()
        except OSError:
            pass

    print(f"received {len(received)} bytes", file=sys.stderr)
    sys.stdout.buffer.write(bytes(received))


if __name__ == "__main__":
    main()
