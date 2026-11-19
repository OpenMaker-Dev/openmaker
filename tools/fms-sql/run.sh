#!/bin/bash
# Host side: build and run fqlgrammar in the om-fms-calc-harness image
# (tools/fms-calc-harness/Dockerfile), network-less, FileMaker's libraries
# mounted read-only.
# usage: run.sh atn > grammar-dump.txt
#        run.sh tree < statements.txt > trees.txt
# env:   FMS_ROOT  unpacked FileMaker Server .deb opt/FileMaker directory
#        HARNESS_LOCK  machine-wide lock directory shared with runbat.sh
#                      (default re/work/fms-harness/.run.lock in this repository)
set -e
H=$(cd "$(dirname "$0")" && pwd)
: "${FMS_ROOT:?set FMS_ROOT to the unpacked opt/FileMaker directory}"
LOCK=${HARNESS_LOCK:-$H/../../re/work/fms-harness/.run.lock}
mkdir -p "$(dirname "$LOCK")"
until mkdir "$LOCK" 2>/dev/null; do sleep 2; done
trap 'rmdir "$LOCK"' EXIT
docker run -i --rm --platform linux/amd64 --network none \
    -v "$FMS_ROOT":/opt/FileMaker:ro -v "$H":/tool:ro om-fms-calc-harness \
    bash -c 'export LD_LIBRARY_PATH=/opt/FileMaker/lib; mkdir -p /tmp/b &&
        g++ -O0 -std=c++17 -o /tmp/b/fqlgrammar /tool/fqlgrammar.cpp -L/opt/FileMaker/lib -lDBEngine -lantlr4-runtime -lSupport \
            -Wl,-rpath,/opt/FileMaker/lib -Wl,--allow-shlib-undefined -ldl && /tmp/b/fqlgrammar "$@"' fqlgrammar "$@"
