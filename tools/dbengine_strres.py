#!/usr/bin/env python3
"""Export the DBEngine string-resource directory (Mac x86_64 slice).

The directory is an array of {u64 resource_id, ptr records, u64 count}
entries; each record is 4 C-string pointers. Output TSV columns:
resource_id, index, f0, f1, f2, f3. Reads the file as data only.

Usage: python3 -I tools/dbengine_strres.py <thin-DBEngine> <dir-va-hex> <out.tsv>
"""
import struct
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from macho_ptrscan import load, v2o, decode_ptr  # noqa: E402
from macho_strtables import cstr, esc  # noqa: E402


def main():
    path, dva, out = sys.argv[1], int(sys.argv[2], 16), sys.argv[3]
    d, sects = load(path)
    n = 0
    with open(out, "w", encoding="utf-8") as f:
        va = dva
        while True:
            rid, p, cnt = struct.unpack_from("<QQQ", d, v2o(sects, va))
            if not (0 < rid < 100000 and 0 < cnt < 100000):
                break
            base = decode_ptr(p)
            for i in range(cnt):
                fields = []
                for k in range(4):
                    w = struct.unpack_from("<Q", d, v2o(sects, base + 32 * i + 8 * k))[0]
                    t = decode_ptr(w)
                    fields.append(esc(cstr(d, sects, t)).replace("\u2014", "\\u2014") if t else "")
                f.write(f"{rid}\t{i}\t" + "\t".join(fields) + "\n")
            n += 1
            va += 24
    print(f"{n} resources exported")


if __name__ == "__main__":
    main()
