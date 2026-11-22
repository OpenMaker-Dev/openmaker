#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == --help ]]; then
    echo 'Install om-author from this checkout and configure a hosted file for the CLI and MCP.'
    echo 'Usage: tools/setup-om-author.sh'
    exit 0
fi
if [[ $# -ne 0 ]]; then
    echo 'Usage: tools/setup-om-author.sh' >&2
    exit 2
fi

for dependency in cargo python3; do
    if ! command -v "$dependency" >/dev/null; then
        printf 'Setup needs %s on PATH. See docs/authoring.md.\n' "$dependency" >&2
        exit 1
    fi
done

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
install_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
[[ $install_root == /* ]] || install_root="$PWD/$install_root"
binary="$install_root/bin/om-author"
if ! ( : </dev/tty ) 2>/dev/null; then
    if [[ ! -x $binary ]] || ! config_path=$("$binary" config-path 2>/dev/null) || [[ ! -f $config_path ]]; then
        echo 'Run setup in an interactive terminal, or see docs/authoring.md#manual-configuration.' >&2
        exit 1
    fi
fi

(cd -- "$repo_dir" && cargo install --locked --path crates/om-author --root "$install_root")
config_path=$("$binary" config-path)

python3 - "$binary" "$config_path" <<'PY'
import getpass
import json
import os
from pathlib import Path
import shlex
import sys
import tempfile
import warnings

warnings.filterwarnings("error", category=getpass.GetPassWarning)


def create_config(path):
    # Read from the terminal because stdin carries this script.
    with open("/dev/tty") as terminal_in, open("/dev/tty", "w") as terminal_out:
        def prompt(label, default=""):
            while True:
                suffix = f" [{default}]" if default else ""
                print(f"{label}{suffix}: ", end="", file=terminal_out, flush=True)
                answer = terminal_in.readline()
                if not answer:
                    raise EOFError("The setup prompt was closed.")
                answer = answer.strip() or default
                if answer:
                    return answer

        host = prompt("Server host", "localhost")
        while True:
            port = prompt("Server port", "5003")
            if port.isascii() and port.isdecimal() and 0 < int(port) <= 65535:
                break
            print("Enter a port from 1 to 65535.", file=terminal_out)
        config = {
            "host": host,
            "port": int(port),
            "file": prompt("Hosted file name"),
            "account": prompt("File account", "Admin"),
        }
        ca = prompt("Server CA or self-signed certificate PEM", "none")
        if ca != "none":
            ca_path = Path(ca).expanduser().absolute()
            if not ca_path.is_file():
                raise ValueError(f"Cannot find certificate {ca_path}.")
            config["ca_cert"] = str(ca_path)
        password = getpass.getpass("File password (Enter for empty): ", stream=terminal_out)
        license_key = getpass.getpass(
            "FileMaker client license key (Enter to skip): ", stream=terminal_out
        ).strip()

    os.umask(0o077)
    path.parent.mkdir(parents=True, exist_ok=True)
    secrets = []
    published = False
    try:
        for name, value in [("password", password), ("license", license_key)]:
            if name == "license" and not value:
                continue
            with tempfile.NamedTemporaryFile(
                mode="w", encoding="utf-8", dir=path.parent, prefix=f"{path.stem}-{name}-", delete=False
            ) as secret:
                secret_path = Path(secret.name)
                secrets.append(secret_path)
                secret.write(value + "\n")
                config[f"{name}_file"] = secret_path.name
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent) as draft:
            json.dump(config, draft, indent=2)
            draft.write("\n")
            draft.flush()
            # Publish the config only after its credentials are ready. Never overwrite it.
            os.link(draft.name, path)
            published = True
    finally:
        if not published:
            for secret_path in secrets:
                secret_path.unlink(missing_ok=True)


binary = Path(sys.argv[1]).absolute()
config_path = Path(sys.argv[2]).absolute()
try:
    if config_path.is_file():
        print(f"Using existing configuration at {config_path}")
        print("Edit that file for a new host or account, or edit the referenced password or license file to update a secret.")
    else:
        create_config(config_path)
        print(f"Saved configuration to {config_path}")
except (EOFError, KeyboardInterrupt):
    sys.exit("Setup canceled.")
except (OSError, ValueError, getpass.GetPassWarning) as error:
    sys.exit(f"Could not complete om-author setup: {error}")

command = shlex.join([str(binary), "--config", str(config_path)])
print(f"\nCheck access to the hosted file:\n  {command} capabilities")
print(f"\nRegister with Codex:\n  codex mcp add openmaker -- {command} mcp")
print(f"\nRegister with Claude Code:\n  claude mcp add --scope user openmaker -- {command} mcp")
PY
