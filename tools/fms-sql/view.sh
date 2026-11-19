#!/bin/bash
# Show a results file compactly: SQL (or calculation) <TAB> error <TAB> result
awk -F'\t' '/^#/{print; next} {e=$1; if (e ~ /^ExecuteSQLe \( "/) { sub(/^ExecuteSQLe \( "/,"",e); sub(/" ; "\|" ; "¶"/," ;;",e); gsub(/\\"/,"\"",e) } print e "\t=> " $2 "\t" $4}' "$@"
