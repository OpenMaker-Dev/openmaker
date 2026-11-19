#!/bin/bash
# Runs inside the container: builds calceval once, copies the chosen .fmp12 out
# of the read-only /files mount and runs calceval on stdin.
# usage: run.sh [-c|-r] [-d] <file name in /files> [context table occurrence, stored form, hex] < input
set -e
export LD_LIBRARY_PATH=/opt/FileMaker/lib
mkdir -p /tmp/build
[ /tmp/build/calceval -nt /harness/calceval.cpp ] || g++ -O0 -std=c++17 -o /tmp/build/calceval /harness/calceval.cpp \
    -L/opt/FileMaker/lib -lDBEngine -lSupport -Wl,-rpath,/opt/FileMaker/lib -Wl,--allow-shlib-undefined -ldl
MODE=()
while case "$1" in -*) true;; *) false;; esac; do MODE+=("$1"); shift; done
cp "/files/$1" /tmp/work.fmp12
shift
/tmp/build/calceval "${MODE[@]}" /tmp/work.fmp12 "$@"
