#!/usr/bin/env python3
"""Fetch and cache the Claris FileMaker Pro help pages for every calculation
function and Get selector.

The help site is MadCap Flare WebHelp2 output. Its table of contents is in
Data/Tocs/main_toc.js (tree of numeric ids) plus main_toc_Chunk<n>.js (id ->
page path and title). This script reads the TOC, takes every page below
"Functions reference" (the 19 category pages and each function and Get
selector page), plus a few context pages, and downloads them sequentially
with a delay.

    python3 -I tools/claris_calc_docs_fetch.py OUTDIR [--delay 0.7] [--refresh]

OUTDIR receives:
  toc/                  raw TOC files as fetched
  content/<page>.html   cached pages (skipped if present unless --refresh)
  toc-functions.tsv     depth, category, title, page for the functions subtree
  manifest.tsv          page, url, http status, bytes, sha256, fetched-at (UTC)
"""
import argparse
import hashlib
import os
import sys
import time
import urllib.error
import urllib.request

BASE = "https://help.claris.com/en/pro-help/"
ROOT_PAGE = "/content/functions-reference.html"
EXTRA_PAGES = [
    "/content/named-constants-keywords.html",
    "/content/functions.html",
    "/content/formulas.html",
    "/content/operators-in-formulas.html",
    "/content/feature-compatibility.html",
    "/content/json-functions.html",
    "/content/custom-functions.html",
    "/content/custom-function-dependency.html",
    "/content/using-variables.html",
    "/content/text-constants-special-characters.html",
    "/content/running-scripts-on-server.html",
    "/content/working-with-formulas-functions.html",
    "/content/calculation-fields.html",
]
UA = "Mozilla/5.0 (OpenMaker clean-room research; sequential fetch)"


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, b""


class JsLiteral:
    """Parser for the JavaScript object literals MadCap writes
    (unquoted keys, single-quoted strings, numbers, arrays, objects)."""

    def __init__(self, s):
        self.s, self.p = s, 0

    def ws(self):
        while self.p < len(self.s) and self.s[self.p] in " \t\r\n":
            self.p += 1

    def value(self):
        self.ws()
        c = self.s[self.p]
        if c == "{":
            return self.obj()
        if c == "[":
            return self.arr()
        if c in "'\"":
            return self.string()
        return self.atom()

    def obj(self):
        out = {}
        self.p += 1
        while True:
            self.ws()
            if self.s[self.p] == "}":
                self.p += 1
                return out
            if self.s[self.p] in "'\"":
                key = self.string()
            else:
                key = self.atom()
            self.ws()
            assert self.s[self.p] == ":", self.s[self.p - 20:self.p + 20]
            self.p += 1
            out[key] = self.value()
            self.ws()
            if self.s[self.p] == ",":
                self.p += 1

    def arr(self):
        out = []
        self.p += 1
        while True:
            self.ws()
            if self.s[self.p] == "]":
                self.p += 1
                return out
            out.append(self.value())
            self.ws()
            if self.s[self.p] == ",":
                self.p += 1

    def string(self):
        q = self.s[self.p]
        self.p += 1
        buf = []
        while self.s[self.p] != q:
            c = self.s[self.p]
            if c == "\\":
                self.p += 1
                c = self.s[self.p]
                if c == "u":
                    c = chr(int(self.s[self.p + 1:self.p + 5], 16))
                    self.p += 4
                else:
                    c = {"n": "\n", "t": "\t", "r": "\r"}.get(c, c)
            buf.append(c)
            self.p += 1
        self.p += 1
        return "".join(buf)

    def atom(self):
        start = self.p
        while self.p < len(self.s) and self.s[self.p] not in ",:}] \t\r\n":
            self.p += 1
        tok = self.s[start:self.p]
        try:
            return int(tok)
        except ValueError:
            return tok


def parse_define(text):
    i = text.index("define(") + len("define(")
    return JsLiteral(text[i:]).value()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("outdir")
    ap.add_argument("--delay", type=float, default=0.7)
    ap.add_argument("--refresh", action="store_true")
    a = ap.parse_args()

    tocdir = os.path.join(a.outdir, "toc")
    cdir = os.path.join(a.outdir, "content")
    os.makedirs(tocdir, exist_ok=True)
    os.makedirs(cdir, exist_ok=True)

    def get_toc(name):
        path = os.path.join(tocdir, name)
        if a.refresh or not os.path.exists(path):
            st, body = fetch(BASE + "Data/Tocs/" + name)
            if st != 200:
                sys.exit(f"TOC fetch failed: {name} -> {st}")
            with open(path, "wb") as f:
                f.write(body)
            time.sleep(a.delay)
        with open(path, encoding="utf-8") as f:
            return f.read()

    toc = parse_define(get_toc("main_toc.js"))
    id2page = {}
    for n in range(toc["numchunks"]):
        chunk = parse_define(get_toc(f"{toc['prefix']}{n}.js"))
        for page, ent in chunk.items():
            for i, t in zip(ent["i"], ent["t"]):
                id2page[i] = (page, t)

    def find(nodes):
        for nd in nodes:
            if id2page.get(nd["i"], ("",))[0] == ROOT_PAGE:
                return nd
            hit = find(nd.get("n", []))
            if hit:
                return hit
        return None

    root = find(toc["tree"]["n"])
    if root is None:
        sys.exit("functions-reference.html not found in the TOC")

    rows = []

    def walk(nd, depth, category):
        page, title = id2page[nd["i"]]
        if depth == 1:
            category = title
        rows.append((depth, category, title, page))
        for ch in nd.get("n", []):
            walk(ch, depth + 1, category)

    walk(root, 0, "")
    with open(os.path.join(a.outdir, "toc-functions.tsv"), "w", encoding="utf-8") as f:
        f.write("depth\tcategory\ttitle\tpage\n")
        for r in rows:
            f.write("\t".join(map(str, r)) + "\n")

    pages = []
    for r in rows:
        if r[3] not in pages:
            pages.append(r[3])
    for p in EXTRA_PAGES:
        if p not in pages:
            pages.append(p)

    man_path = os.path.join(a.outdir, "manifest.tsv")
    manifest = {}
    if os.path.exists(man_path):
        with open(man_path, encoding="utf-8") as f:
            for line in f.read().splitlines()[1:]:
                manifest[line.split("\t")[0]] = line
    for n, page in enumerate(pages, 1):
        name = page.rsplit("/", 1)[1]
        path = os.path.join(cdir, name)
        if os.path.exists(path) and not a.refresh:
            continue
        url = BASE + page.lstrip("/")
        st, body = fetch(url)
        if st == 200:
            with open(path, "wb") as f:
                f.write(body)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        manifest[name] = "\t".join([name, url, str(st), str(len(body)),
                                    hashlib.sha256(body).hexdigest(), stamp])
        print(f"[{n}/{len(pages)}] {st} {name}", file=sys.stderr)
        time.sleep(a.delay)
    with open(man_path, "w", encoding="utf-8") as f:
        f.write("page\turl\tstatus\tbytes\tsha256\tfetched_at\n")
        for k in sorted(manifest):
            f.write(manifest[k] + "\n")
    print(f"{len(rows)} TOC entries below functions reference, {len(pages)} pages", file=sys.stderr)


if __name__ == "__main__":
    main()
