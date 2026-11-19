#!/bin/sh
# Print one function from a DecompileList output. Usage: getfn.sh <file.c> <hexaddr>
awk -v A="//// $2 " 'index($0, A)==1 {p=1; print; next} /^\/\/\/\/ / {p=0} p' "$1"
