#!/bin/sh
# Print disassembly lines whose address is in [start, end). Usage: disrange.sh <dis> <start-hex> <end-hex>
awk -v s="$2" -v e="$3" 'BEGIN{S=strtonum("0x" s); E=strtonum("0x" e)} /^ *[0-9a-f]+:/ {a=strtonum("0x" substr($1,1,length($1)-1)); if (a>=E) exit; if (a>=S) print substr($0,1,200)}' "$1"
