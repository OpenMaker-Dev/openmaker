"""Compile stored formulas again with FileMaker Server's engine and compare.

usage: python3 -I roundtrip.py <calcs.tsv> <dir with the .fmp12 files> <out dir> [jobs]

calcs.tsv is the output of `cargo run -p om-hbam --example dump_calcs`
(rows `calc file owner path context formatted polish`, hex). For every
formula that has a formatted stream (custom function bodies excepted, their
parameters would need a binding environment), calceval -r turns the
formatted stream into text with DBCalcScan::UnTokenize, compiles that text in
the stored context table occurrence (DBCalcParse::DoParse, Optimize,
WriteAsPolish, Tokenize) and reports whether both streams come out byte for
byte as stored.
Writes <out>/<file>.tsv per file and prints a summary per owner kind.
Environment: FMS_ROOT as for runbat.sh.
"""
import collections
import concurrent.futures
import os
import subprocess
import sys

H = os.path.dirname(os.path.abspath(__file__))


def run(fmsroot, files, name, rows, out):
    inp = "".join("%s\t%s\t%s\t%s\n" % r for r in rows)
    cmd = ["docker", "run", "-i", "--rm", "--platform", "linux/amd64", "--network", "none",
           "-v", fmsroot + ":/opt/FileMaker:ro", "-v", files + ":/files:ro", "-v", H + ":/harness:ro",
           "om-fms-calc-harness", "/harness/run.sh", "-r", name]
    p = subprocess.run(cmd, input=inp.encode(), capture_output=True)
    text = p.stdout.decode("utf-8", "replace")
    with open(os.path.join(out, name.replace(".fmp12", ".tsv")), "w", encoding="utf-8") as f:
        f.write(text)
    if p.returncode:
        sys.stderr.write("%s: exit %d\n%s\n" % (name, p.returncode, p.stderr.decode("utf-8", "replace")[-2000:]))
    return name, text


def main():
    calcs, files, out = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
    jobs = int(sys.argv[4]) if len(sys.argv) > 4 else 3
    fmsroot = os.environ["FMS_ROOT"]
    os.makedirs(out, exist_ok=True)
    byfile, owner = collections.defaultdict(list), {}
    for line in open(calcs, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        # skip stripped formulas (no formatted stream) and custom function
        # bodies (their parameter names would need a binding environment)
        if f[0] != "calc" or f[5] in ("", "-") or f[2].startswith("custom-function"):
            continue
        rid = "%s|%s" % (f[1], f[3])
        owner[rid] = f[2].split("/")[0]
        byfile[f[1]].append((rid, f[4], f[5], f[6]))
    stats = collections.defaultdict(lambda: [0, 0, 0, 0])
    with concurrent.futures.ThreadPoolExecutor(jobs) as ex:
        for name, text in ex.map(lambda kv: run(fmsroot, files, kv[0], kv[1], out), sorted(byfile.items())):
            seen = set()
            for line in text.splitlines():
                f = line.split("\t")
                if len(f) < 4 or f[0] not in owner:
                    continue
                seen.add(f[0])
                s = stats[owner[f[0]]]
                s[0] += 1
                s[1] += f[1] == "0"
                s[2] += f[2] == "1"
                s[3] += f[3] == "1"
            missing = len(byfile[name]) - len(seen)
            if missing:
                sys.stderr.write("%s: %d formulas without output\n" % (name, missing))
    print("owner\tformulas\tcompiled\tsame formatted\tsame Polish")
    tot = [0, 0, 0, 0]
    for k in sorted(stats):
        print("%s\t%s" % (k, "\t".join(map(str, stats[k]))))
        tot = [a + b for a, b in zip(tot, stats[k])]
    print("all\t%s" % "\t".join(map(str, tot)))


if __name__ == "__main__":
    main()
