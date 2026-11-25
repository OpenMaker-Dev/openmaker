#!/bin/bash
# Generate a battery from its .sql source and run it with
# tools/fms-calc-harness/runbat.sh: runq.sh <name> <file.fmp12> [context] [results dir]
# (FMS_ROOT and FILES as for runbat.sh)
set -e
H=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$H/../.." && pwd)
python3 -I "$H/make_battery.py" "$H/batteries/$1.sql" > "$H/batteries/$1.txt"
"$H/../fms-calc-harness/runbat.sh" "$H/batteries/$1.txt" "$2" "${4:-$ROOT/re/work/fms-sql}" "${3:-}"
