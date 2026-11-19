#!/usr/bin/env python3
"""List the calculation opcode constants (`const Draco::DBOpcode kDB...`)
declared in DBOpTbl.h and DBCalcEngine.h, from the DWARF of libDBEngine.so.

    python3 -I tools/dbopcode_table.py LIBDBENGINE OUT.tsv

Columns: file, line, name, value (empty when the DIE has no constant value).
Takes about 15 seconds (one llvm-dwarfdump --regex pass).
"""
import re
import shutil
import subprocess
import sys


def tool(name):
    try:
        return subprocess.run(["xcrun", "--find", name], capture_output=True,
                              text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return shutil.which(name) or name


def main():
    lib, out = sys.argv[1], sys.argv[2]
    dump = subprocess.run([tool("llvm-dwarfdump"), "--regex", "--name=^kDB.*", lib],
                          capture_output=True, text=True).stdout
    rows = {}
    for die in re.split(r"\n(?=0x[0-9a-f]+: DW_TAG)", dump):
        name = re.search(r'DW_AT_name\s+\("([^"]+)"\)', die)
        typ = re.search(r'DW_AT_type\s+\(0x[0-9a-f]+ "([^"]+)"\)', die)
        if not name or not typ or "DBOpcode" not in typ.group(1):
            continue
        f = re.search(r'DW_AT_decl_file\s+\("([^"]+)"\)', die)
        ln = re.search(r"DW_AT_decl_line\s+\((\d+)\)", die)
        v = re.search(r"DW_AT_const_value\s+\(([-\d]+)\)", die)
        key = (f.group(1).rsplit("/", 1)[-1], int(ln.group(1)), name.group(1))
        if v or key not in rows:
            rows[key] = v.group(1) if v else ""
    with open(out, "w", encoding="utf-8") as fh:
        fh.write("file\tline\tname\tvalue\n")
        for (f, ln, n), v in sorted(rows.items()):
            fh.write(f"{f}\t{ln}\t{n}\t{v}\n")
    print(f"{len(rows)} opcode constants", file=sys.stderr)


if __name__ == "__main__":
    main()
