#!/bin/bash
# Runs inside the container: builds scriptrun once, copies the chosen .fmp12
# out of the read-only /files mount and runs scriptrun on stdin.
# usage: run.sh [-l | -x | -p <patch in /work> | -g <file XML in /work>] <file name in /files> < run lines
#   -l  list the file's scripts
#   -x  write FMDeveloperTool --saveAsXML of the file to /work/<file>.xml
#   -p  add the patch's scripts to the private copy with FMUpgradeTool
#       --update first (and keep the patched copy as /work/<patch>.fmp12)
#   -g  run a file FMUpgradeTool --generateDBFile builds from the schema
#       document (make_patch.py --generate) instead of a copy of the file
#       (kept as /work/<document>.fmp12, and after the run, with the probe
#       scripts' block indexes written, as /work/<document>.run.fmp12)
# env: ACCOUNT  full access account of the file, name:password (default Admin:)
set -e
export LD_LIBRARY_PATH=/opt/FileMaker/lib
BIN="/opt/FileMaker/FileMaker Server/Database Server/bin"
ACCOUNT=${ACCOUNT:-Admin:}
USER=${ACCOUNT%%:*}; PW=${ACCOUNT#*:}
mkdir -p /tmp/build
# Only the interposed functions are exported from the executable (see
# scriptrun.cpp); exporting everything would also interpose its inline
# helpers over FileMaker's own definitions.
HOOKS=$(sed -n 's/^[A-Za-z*]* *\(_ZNK\{0,1\}5Draco[A-Za-z0-9_]*\)(.*/\1/p' /harness/scriptrun.cpp)
[ /tmp/build/scriptrun -nt /harness/scriptrun.cpp ] || g++ -O0 -std=c++17 -o /tmp/build/scriptrun /harness/scriptrun.cpp \
    $(for h in $HOOKS; do printf -- '-Wl,--export-dynamic-symbol=%s ' "$h"; done) \
    -L/opt/FileMaker/lib -lFMEngine -lDBEngine -lSupport -Wl,-rpath,/opt/FileMaker/lib -Wl,--allow-shlib-undefined -ldl
MODE=; PATCH=; GEN=
while case "$1" in -*) true;; *) false;; esac; do
    case "$1" in -p) PATCH=$2; shift;; -g) GEN=$2; shift;; *) MODE=$1;; esac
    shift
done
F=$1
cp "/files/$F" /tmp/base.fmp12
if [ "$MODE" = -x ]; then
    "$BIN/FMDeveloperTool" --saveAsXML /tmp/base.fmp12 "$USER" "$PW" -t "/work/${F%.fmp12}.xml" -force >&2
    exit
fi
if [ -n "$GEN" ]; then
    # The generated file is written next to the document, named after it.
    rm -rf /tmp/gen && mkdir /tmp/gen && cp "/work/$GEN" /tmp/gen/file.xml
    "$BIN/FMUpgradeTool" --generateDBFile -src_path /tmp/gen/file.xml -force >&2
    cp /tmp/gen/file.fmp12 /tmp/work.fmp12
    cp /tmp/work.fmp12 "/work/${GEN%.xml}.fmp12"
elif [ -n "$PATCH" ]; then
    "$BIN/FMUpgradeTool" --update -src_path /tmp/base.fmp12 -patch_path "/work/$PATCH" -dest_path /tmp/work.fmp12 \
        -src_account "$USER" -src_pwd "$PW" -force >&2
    cp /tmp/work.fmp12 "/work/${PATCH%.xml}.fmp12"
else
    cp /tmp/base.fmp12 /tmp/work.fmp12
fi
/tmp/build/scriptrun $MODE /tmp/work.fmp12
[ -z "$PATCH" ] || cp /tmp/work.fmp12 "/work/${PATCH%.xml}.run.fmp12"
[ -z "$GEN" ] || cp /tmp/work.fmp12 "/work/${GEN%.xml}.run.fmp12"
[ -n "$PATCH$GEN" ] || [ -n "$MODE" ] || cp /tmp/work.fmp12 "/work/${F%.fmp12}.run.fmp12"
