#!/usr/bin/env bash
# Builds OpenMaker for Ubuntu 24.04 (amd64) and packages it as a .deb and a
# tarball: release om-gui with web viewers, om-server, a desktop entry, the
# icon, and the licenses of what the binaries embed. Everything runs in an
# ubuntu:24.04 container (docker or podman), so the host needs neither
# Ubuntu 24.04 nor WebKitGTK's development files. See docs/development.md.
#
#   tools/package-ubuntu.sh [output folder]     (default: target/package)
#
# ENGINE picks docker or podman (default: whichever is installed); JOBS is
# cargo's job count (default 4). The container keeps its toolchain and
# build in target/ubuntu-24.04, apart from the host's own builds.
set -euo pipefail
cd "$(dirname "$0")/.."
engine=${ENGINE:-$(command -v docker || command -v podman || true)}
if [ -z "$engine" ]; then
    echo "package-ubuntu: needs docker or podman" >&2
    exit 1
fi
out=${1:-target/package}
mkdir -p "$out" target/ubuntu-24.04
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)+git$(git rev-parse --short=8 HEAD)
image=openmaker-package:ubuntu24.04
"$engine" build -q -t "$image" tools/package >/dev/null
# Rootless podman maps the container's root to the caller; docker needs to
# be told to run as the caller, so the build and packages stay theirs.
user=()
[ "$(basename "$engine")" = podman ] || user=(--user "$(id -u):$(id -g)")
cache=/src/target/ubuntu-24.04
"$engine" run --rm "${user[@]}" -v "$PWD:/src" -v "$(realpath "$out"):/out" -w /src \
    -e CARGO_HOME=$cache/cargo -e RUSTUP_HOME=$cache/rustup -e CARGO_TARGET_DIR=$cache/target \
    -e JOBS="${JOBS:-4}" -e VERSION="$version" \
    "$image" bash tools/package/build.sh
ls -l "$out"/openmaker*"$version"*
