"""List PEM blocks embedded in binaries without printing key material.

For each block: file, offset, PEM type, and for certificates the subject,
issuer, validity, key usage, extended key usage, and SHA-256 fingerprint.
Private keys are reported by type, offset, and length only.

Usage: python3 -I tools/pem_inventory.py <file>...
"""

import re
import subprocess
import sys

PEM = re.compile(rb"-----BEGIN ([A-Z0-9 ]+)-----[A-Za-z0-9+/=\s\\n]+?-----END \1-----")


def describe_certificate(block):
    fields = "subject,issuer,keyUsage,extendedKeyUsage,basicConstraints"
    result = subprocess.run(
        ["openssl", "x509", "-noout", "-subject", "-issuer", "-dates",
         "-fingerprint", "-sha256", "-ext", fields],
        input=block, capture_output=True,
    )
    text = result.stdout.decode(errors="replace").strip() or result.stderr.decode(errors="replace").strip()
    return "\n".join("    " + line for line in text.splitlines())


def main():
    for path in sys.argv[1:]:
        data = open(path, "rb").read()
        for match in PEM.finditer(data):
            kind = match.group(1).decode()
            block = match.group(0).replace(b"\\n", b"\n")
            print(f"{path} @0x{match.start():x} len={len(block)} type={kind}")
            if kind == "CERTIFICATE":
                print(describe_certificate(block))


if __name__ == "__main__":
    main()
