#!/bin/bash
# Host side: run one battery file against one .fmp12 file with FileMaker
# Server's engine in a network-less amd64 container.
# usage: runbat.sh [-c] [-d] <battery.txt> <file.fmp12 in $FILES> [results dir] [context]
#   (no flag) evaluate, -c compile, -d with the design functions registered
#   (README.md); context: the stored table occurrence key (hex, e.g.
#   03d00001) the run starts in
# env:   FMS_ROOT  unpacked FileMaker Server .deb opt/FileMaker directory
#        FILES     directory holding the .fmp12 copies (mounted read-only)
set -e
H=$(cd "$(dirname "$0")" && pwd)
: "${FMS_ROOT:?set FMS_ROOT to the unpacked opt/FileMaker directory}"
: "${FILES:?set FILES to a directory with .fmp12 copies}"
MODE=()
while case "$1" in -*) true;; *) false;; esac; do MODE+=("$1"); shift; done
B=$1; F=$2; OUT=${3:-$H/results}; CTX=${4:-}
mkdir -p "$OUT"
out="$OUT/$(basename "$B" .txt).${F%.fmp12}.tsv"
# One container at a time on this machine: parallel runs are killed for
# memory (exit 137) and leave truncated results.
LOCK=${HARNESS_LOCK:-$(dirname "$FILES")/.run.lock}
until mkdir "$LOCK" 2>/dev/null; do sleep 2; done
trap 'rmdir "$LOCK"' EXIT
docker run -i --rm --platform linux/amd64 --network none \
    -v "$FMS_ROOT":/opt/FileMaker:ro -v "$FILES":/files:ro -v "$H":/harness:ro \
    om-fms-calc-harness /harness/run.sh "${MODE[@]}" "$F" $CTX < "$B" 2>/dev/null | grep -av '^open:' > "$out"
echo "$out"
