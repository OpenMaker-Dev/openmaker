#!/bin/bash
# Host side: run one battery against one .fmp12 file with FileMaker Server's
# script engine in a network-less amd64 container (the image of
# tools/fms-calc-harness).
# usage: runbat.sh [-l] <battery.txt> <file.fmp12 in $FILES> [results dir]
#   -l lists the file's scripts instead (battery ignored, use /dev/null)
# A battery's "== Name" blocks (before a "--- run" line) are probe scripts:
# make_patch.py turns them into an FMUpgradeTool patch against the file's
# saveAsXML schema (made once, cached in $WORK), and the container adds them
# to its private copy before running. A battery with "@trigger" lines or
# schema directives ("@layout", "@validate", ...) runs a file FMUpgradeTool
# generates from that schema with the probes and changes added instead
# (make_patch.py --generate; patches cannot change layouts).
# The run lines follow "--- run"; when there is none, every probe script is
# run once without a parameter.
# env:   FMS_ROOT     unpacked FileMaker Server .deb opt/FileMaker directory
#        FILES        directory holding the .fmp12 copies (mounted read-only)
#        WORK         writable scratch for schema XML, patches and patched
#                     copies (default: script-harness-work next to $FILES)
#        ACCOUNT      full access account of the file (default Admin:)
#        HARNESS_LOCK machine-wide lock directory shared with fms-calc-harness
#        STEP_LIMIT, FILE_TRIGGERS  passed to scriptrun (README)
#        SCHEMA       a schema document to generate the file from instead of
#                     the file's own saveAsXML (implies a generated file)
#        BEFORE       a command started in the container before the run, in
#                     the background (a loopback server for Insert from URL;
#                     the container has no other network)
#        DOCUMENTS    a writable directory mounted as the Documents folder
#                     (Get ( DocumentsPath ): the home folder's Documents,
#                     /root/Documents in the container); Export Records then
#                     runs as fmsased runs it (SASE_EXPORT, README)
set -e
H=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$H/../.." && pwd)
: "${FMS_ROOT:?set FMS_ROOT to the unpacked opt/FileMaker directory}"
: "${FILES:?set FILES to a directory with .fmp12 copies}"
WORK=${WORK:-$(dirname "$FILES")/script-harness-work}
MODE=()
while case "$1" in -*) true;; *) false;; esac; do MODE+=("$1"); shift; done
B=$1; F=$2; OUT=${3:-$ROOT/re/work/fms-script-harness}
mkdir -p "$OUT" "$WORK"
name=$(basename "$B" .txt); base=${F%.fmp12}
out="$OUT/$name.$base.tsv"
# One container at a time on this machine: parallel runs are killed for
# memory (exit 137) and leave truncated results.
LOCK=${HARNESS_LOCK:-$(dirname "$FILES")/.run.lock}
until mkdir "$LOCK" 2>/dev/null; do sleep 2; done
trap 'rmdir "$LOCK"' EXIT
dock() {
    docker run -i --rm --platform linux/amd64 --network none -e ACCOUNT="${ACCOUNT:-Admin:}" \
        ${STEP_LIMIT:+-e STEP_LIMIT="$STEP_LIMIT"} ${FILE_TRIGGERS:+-e FILE_TRIGGERS="$FILE_TRIGGERS"} \
        -v "$FMS_ROOT":/opt/FileMaker:ro -v "$FILES":/files:ro -v "$H":/harness:ro -v "$WORK":/work \
        ${BEFORE:+-e BEFORE="$BEFORE"} \
        ${DOCUMENTS:+-e SASE_EXPORT=1 -v "$DOCUMENTS":/root/Documents} \
        om-fms-calc-harness bash -c '[ -n "$BEFORE" ] && { sh -c "$BEFORE" & sleep 1; }; exec /harness/run.sh "$@"' run "$@"
}
if grep -q '^==' "$B"; then
    [ -s "$WORK/$base.xml" ] || dock -x "$F" < /dev/null 2> "$out.stderr"
    awk '/^--- run/ { exit } { print }' "$B" > "$WORK/$name.scripts.txt"
    if [ -n "$SCHEMA" ] || grep -q '^@' "$WORK/$name.scripts.txt"; then
        python3 -I "$H/make_patch.py" --generate "$WORK/$name.scripts.txt" "${SCHEMA:-$WORK/$base.xml}" > "$WORK/$name.$base.gen.xml"
        PROBES=(-g "$name.$base.gen.xml")
    else
        python3 -I "$H/make_patch.py" "$WORK/$name.scripts.txt" "$WORK/$base.xml" > "$WORK/$name.$base.patch.xml"
        PROBES=(-p "$name.$base.patch.xml")
    fi
    # the probe scripts' names; "@scan" makes scriptrun write their block
    # indexes first (FMUpgradeTool's importer leaves them 0)
    sed -n 's/^==[[:space:]]*\(.*[^[:space:]]\)[[:space:]]*$/\1/p' "$B" | sed 's/ *\[full access\]$//' > "$WORK/$name.names.txt"
    { printf '@scan'; while IFS= read -r n; do printf '\t%s' "$n"; done < "$WORK/$name.names.txt"; printf '\n'
      if grep -q '^--- run' "$B"; then awk 'p { print } /^--- run/ { p = 1 }' "$B"; else cat "$WORK/$name.names.txt"; fi
    } > "$WORK/$name.run.txt"
    dock "${MODE[@]}" "${PROBES[@]}" "$F" < "$WORK/$name.run.txt" 2>> "$out.stderr" > "$out" || echo "exit $?" >> "$out.stderr"
else
    dock "${MODE[@]}" "$F" < "$B" 2> "$out.stderr" > "$out" || echo "exit $?" >> "$out.stderr"
fi
echo "$out"
