#!/usr/bin/env bash
# Import and auto-analyze one thinned FileMaker binary into its own persistent
# Ghidra project, so REA and subagents can reuse it via REA_GHIDRA_PROJECT_PATH
# instead of re-importing a 60 MB binary per query.
#
# Usage: tools/ghidra-preanalyze.sh <name> <heap>   e.g. DBEngine 10G
# Output: re/work/ghidra/<name>/<name>.gpr (+ .rep), log in re/work/ghidra/<name>.log
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
name="$1"
heap="$2"
bin="$root/re/work/thin/$name"
proj_dir="$root/re/work/ghidra/$name"

[[ -f "$bin" ]] || { echo "missing $bin" >&2; exit 1; }
if [[ -f "$proj_dir/$name.gpr" ]]; then
  echo "project already exists: $proj_dir/$name.gpr" >&2
  exit 0
fi
mkdir -p "$proj_dir"

export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
export GHIDRA_HEADLESS_MAXMEM="$heap"

start=$(date +%s)
"$HOME/tools/ghidra_12.1.4_PUBLIC/support/analyzeHeadless" \
  "$proj_dir" "$name" \
  -import "$bin" \
  -max-cpu 4 \
  -log "$root/re/work/ghidra/$name.ghidra.log" \
  > "$root/re/work/ghidra/$name.log" 2>&1
echo "$name analyzed in $(( $(date +%s) - start ))s"
