#!/usr/bin/env python3
"""Extract structured data from the cached Claris function pages
(see claris_calc_docs_fetch.py) into one JSON file.

    python3 -I tools/claris_calc_docs_parse.py CACHEDIR OUT.json

Per page: title, category, URL, the engine string-resource variable that
holds the one-line purpose (ui-strings-functions.kDB...Op, which names the
engine opcode), purpose, format, parameters, return type, version, the
description/notes text (tables flattened to rows), sentences that mention
other FileMaker products, and examples. Each example keeps its full text
plus the (expression, expected) pairs that could be read from its markup:
`<code>EXPR</code> returns <samp>RESULT</samp>`, a code snippet followed by
"returns <samp>RESULT</samp>", result tables, and "Returns <samp>X</samp>"
on Get pages (expression = the format). Line breaks inside <samp> are kept
as "\\n" (FileMaker itself returns a carriage return there).
"""
import html
import json
import os
import re
import sys

NBSP = " "
PRODUCTS = re.compile(
    r"FileMaker\s+(Go|WebDirect|Server|Cloud|Data API|Pro)|Custom Web Publishing|"
    r"Data API|OData|Claris Studio|runtime|Perform Script on Server|server-side|on the server|"
    r"iOS|iPadOS|Windows|macOS|Linux", re.I)


def text_of(frag, keep_breaks=True):
    """HTML fragment -> plain text."""
    s = frag
    s = re.sub(r"<img[^>]*\balt=\"([^\"]*)\"[^>]*>", lambda m: "[image: " + m.group(1) + "]", s)
    s = re.sub(r"<a class=\"codeSnippetCopyButton\".*?</a>", "", s, flags=re.S)
    if keep_breaks:
        s = re.sub(r"<br\s*/?>", "\n", s)
        s = re.sub(r"</(p|li|tr|pre|h\d|div)>", "\n", s)
        s = re.sub(r"</td>", "\t", s)
    s = re.sub(r"<[^>]+>", "", s)
    s = html.unescape(s)
    if keep_breaks:
        lines = [re.sub(r"[ \t\r\f\v]+", " ", ln).strip() for ln in s.split("\n")]
        out = []
        for ln in lines:
            if ln or (out and out[-1]):
                out.append(ln)
        return "\n".join(out).strip()
    return re.sub(r"\s+", " ", s).strip()


def code_text(frag):
    """Text of a <code>/<samp>/<pre> body: keep line breaks, collapse nothing
    else except markup and indentation added by the HTML generator."""
    s = re.sub(r"<br\s*/?>", "\n", frag)
    s = re.sub(r"<img[^>]*\balt=\"([^\"]*)\"[^>]*>", lambda m: "[image: " + m.group(1) + "]", s)
    s = re.sub(r"<[^>]+>", "", s)
    s = html.unescape(s)
    return s.strip("\n")


def norm(s):
    return s.replace(NBSP, " ") if s is not None else s


def split_sections(main):
    parts = re.split(r"(<h2[^>]*>.*?</h2>)", main, flags=re.S)
    head = parts[0]
    secs = []
    for i in range(1, len(parts), 2):
        h = parts[i]
        cls = re.search(r'class="([^"]*)"', h)
        auto = re.search(r'data-mc-autonum="([^"]*)"', h)
        title = text_of(h, False)
        secs.append({
            "class": cls.group(1) if cls else "",
            "title": html.unescape(auto.group(1)) if auto else title,
            "heading": title,
            "html": parts[i + 1],
        })
    return head, secs


def parse_params(sec_html):
    params = []
    for m in re.finditer(r"<(p|li)>\s*(.*?)</\1>", sec_html, re.S):
        body = m.group(2)
        pm = re.match(r"\s*<code>(.*?)</code>\s*(?:[-–—:]|is|&#8211;|&#8212;)?\s*(.*)", body, re.S)
        if pm:
            names = [n.strip() for n in re.split(r",|\bor\b", text_of(pm.group(1), False)) if n.strip()]
            params.append({"name": norm(text_of(pm.group(1), False)), "names": names,
                           "description": norm(text_of(pm.group(2), False))})
    return params


def parse_format(fmt):
    """Split a documented format into parameters with optional/repeatable flags."""
    m = re.match(r"^\s*([A-Za-z_][\w]*)\s*(.*)$", fmt, re.S)
    if not m:
        return None, []
    name, rest = m.group(1), m.group(2).strip()
    if not rest:
        return name, []
    rest = rest.strip()
    # WindowNames {( fileName )} : whole parameter list optional
    whole_opt = rest.startswith("{(")
    inner = rest
    inner = re.sub(r"^\{?\(\s*", "", inner)
    inner = re.sub(r"\s*\)\}?\s*$", "", inner)
    out, depth, cur, paren = [], (1 if whole_opt else 0), "", 0
    tok_opt = None
    tokens = []
    for ch in inner:
        if ch == "(":
            paren += 1
        elif ch == ")":
            paren -= 1
        if paren > 0 and ch in ";\n":
            cur += ch
            continue
        if ch == "{":
            depth += 1
            continue
        if ch == "}":
            depth -= 1
            continue
        if ch in "[]":
            continue
        if ch == ";" or ch == "\n":
            tokens.append((cur, bool(tok_opt)))
            cur, tok_opt = "", None
            continue
        if tok_opt is None and not ch.isspace():
            tok_opt = depth > 0
        cur += ch
    tokens.append((cur, bool(tok_opt)))
    for tok, opt in tokens:
        t = tok.strip()
        if not t:
            continue
        rep = "..." in t or "…" in t
        t = t.replace("...", "").replace("…", "").strip()
        if not t:
            if out:
                out[-1]["repeatable"] = True
            continue
        out.append({"name": t, "optional": bool(opt), "repeatable": rep})
    return name, out


def first_samp(frag):
    m = re.search(r"<samp>(.*?)</samp>", frag, re.S)
    return code_text(m.group(1)) if m else None


def example_pairs(blk, fmt):
    pairs = []
    # results shown as images (Japanese text) become "[image: alt]" results
    blk = re.sub(r"<img[^>]*\balt=\"([^\"]*)\"[^>]*>", lambda m: "<samp>[image: " + m.group(1) + "]</samp>", blk)
    # "<code>EXPR</code> returns:" followed by a code block holding the result
    for m in re.finditer(r"<p>\s*<code>(.*?)</code>\s*(?:returns|results in)\s*:?\s*</p>\s*<div class=\"codeSnippet\">.*?<pre><code>(.*?)</code></pre>", blk, re.S):
        pairs.append({"expression": code_text(m.group(1)), "expected": code_text(m.group(2)),
                      "context": "", "form": "returns-block"})
    if pairs:
        return [code_text(c) for c in re.findall(r"<pre><code>(.*?)</code></pre>", blk, re.S)], pairs
    # tables: a row with code in one cell and samp (or plain text) in another
    for tr in re.findall(r"<tr[^>]*>(.*?)</tr>", blk, re.S):
        cells = re.findall(r"<t[dh][^>]*>(.*?)</t[dh]>", tr, re.S)
        codes = [c for c in cells if "<code>" in c]
        if len(cells) >= 2 and codes:
            expr = code_text(re.search(r"<code>(.*?)</code>", codes[0], re.S).group(1))
            rest = [c for c in cells if c is not codes[0]]
            exp = first_samp(rest[0]) if "<samp>" in rest[0] else text_of(rest[0], True)
            pairs.append({"expression": expr, "expected": exp, "context": "", "form": "table"})
    noTables = re.sub(r"<table.*?</table>", "", blk, flags=re.S)
    # inline: <code>EXPR</code> ... returns ... <samp>RESULT</samp> inside one paragraph
    for p in re.findall(r"<p>(.*?)</p>", noTables, re.S):
        for m in re.finditer(r"<code>(.*?)</code>(.{0,80}?)<samp>(.*?)</samp>(.*?)(?=<code>|$)", p, re.S):
            link = text_of(m.group(2), False).lower()
            if not re.search(r"\b(returns?|results? in|displays?|is|evaluates to|gives|produces)\b|^\s*=\s*$", link):
                continue
            expr = code_text(m.group(1))
            extra = re.findall(r"<samp>(.*?)</samp>", m.group(4), re.S)
            ctx = text_of(m.group(4), False)
            pairs.append({"expression": expr, "expected": code_text(m.group(3)),
                          "context": ctx, "form": "inline",
                          **({"other_samps": [code_text(x) for x in extra]} if extra else {})})
    # a function call written without <code>: "Name(...) returns <samp>R</samp>"
    if not pairs:
        for p in re.findall(r"<p>(.*?)</p>", noTables, re.S):
            if "<code>" in p:
                continue
            m = re.match(r"\s*([A-Z][A-Za-z]*\s*\(.*\))\s*(?:returns|=)\s*<samp>(.*?)</samp>(.*)", p, re.S)
            if m:
                pairs.append({"expression": code_text(m.group(1)), "expected": code_text(m.group(2)),
                              "context": text_of(m.group(3), False), "form": "inline-uncoded"})
    # snippet followed by a paragraph containing "returns <samp>"
    snippets = [code_text(c) for c in re.findall(r"<div class=\"codeSnippetBody\"[^>]*>\s*<pre><code>(.*?)</code></pre>", blk, re.S)]
    if snippets and not pairs:
        after = blk.split("</pre>")[-1]
        before = blk.split("<div class=\"codeSnippet\"")[0]
        for region in (after, before):
            m = re.search(r"\b(returns?|result is|displays?)\b[^<]{0,40}<samp>(.*?)</samp>(.*?)(</p>|$)", region, re.S | re.I)
            if m and len(snippets) == 1:
                pairs.append({"expression": snippets[0], "expected": code_text(m.group(2)),
                              "context": text_of(m.group(3), False), "form": "snippet"})
                break
    # "Returns <samp>X</samp> when ..." with no code: the function itself (Get pages)
    if not pairs and not snippets:
        for p in re.findall(r"<p>(.*?)</p>", noTables, re.S):
            m = re.match(r"\s*Returns\s+<samp>(.*?)</samp>(.*)", p, re.S)
            if m and "<code>" not in p.split("<samp>")[0]:
                pairs.append({"expression": fmt, "expected": code_text(m.group(1)),
                              "context": text_of(m.group(2), False), "form": "returns",
                              **({"other_samps": [code_text(x) for x in re.findall(r"<samp>(.*?)</samp>", m.group(2), re.S)]}
                                 if "<samp>" in m.group(2) else {})})
    for pr in pairs:
        for k in ("expression", "expected", "context"):
            if pr.get(k) is not None and NBSP in pr[k]:
                pr[k] = pr[k].replace(NBSP, " ")
                pr["nbsp_normalized"] = True
    return snippets, pairs


def parse_page(path, meta):
    s = open(path, encoding="utf-8").read()
    i = s.find("<h1")
    j = s.find('<h2 class="related-head"', i)
    if j < 0:
        j = s.find('<p class="feedback"', i)
    main = s[i:j]
    title = text_of(re.search(r"<h1[^>]*>(.*?)</h1>", main, re.S).group(1), False)
    head, secs = split_sections(main)
    rec = {"title": norm(title), "page": os.path.basename(path), **meta}
    pv = re.search(r'class="ref-purpose-func">\s*<span class="mc-variable ([\w.-]+) variable">(.*?)</span>', head, re.S)
    if pv:
        rec["engine_string_var"] = pv.group(1)
        rec["purpose"] = norm(text_of(pv.group(2), False))
    else:
        pp = re.search(r'<p class="ref-purpose[^"]*">(.*?)</p>', head, re.S)
        rec["purpose"] = norm(text_of(pp.group(1), False)) if pp else norm(text_of(head, False).replace(title, "", 1))
    desc, notes, examples, other = [], [], [], []
    for sec in secs:
        c, h = sec["class"], sec["html"]
        if c == "ref-format-head":
            fm = re.search(r"<pre class=\"ref-format\">(.*?)</pre>", h, re.S)
            rec["format"] = norm(code_text(fm.group(1))) if fm else norm(text_of(h))
        elif c == "ref-param-head":
            rec["parameters_doc"] = parse_params(h)
            t = text_of(h)
            rec["parameters_text"] = norm(t)
        elif c == "ref-return-head":
            rec["returns"] = norm(text_of(h, False))
        elif c == "ref-orig-head":
            rec["version"] = norm(text_of(h, False))
        elif c == "ref-desc-head":
            desc.append(norm(text_of(h)))
        elif c == "notes-head":
            notes.append(norm(text_of(h)))
        elif c.startswith("ref-example"):
            snippets, pairs = example_pairs(h, rec.get("format", title))
            examples.append({"title": sec["title"], "text": norm(text_of(h)),
                             "code": [norm(x) for x in snippets], "pairs": pairs})
        else:
            other.append({"heading": norm(sec["heading"]), "text": norm(text_of(h))})
    rec["description"] = "\n\n".join(desc)
    rec["notes"] = "\n\n".join(notes)
    rec["other_sections"] = other
    rec["examples"] = examples
    if "format" in rec:
        fname, fparams = parse_format(rec["format"])
        rec["format_name"], rec["format_params"] = fname, fparams
    alltext = " ".join([rec["description"], rec["notes"]] + [o["text"] for o in other])
    sentences = re.split(r"(?<=[.!?])\s+|\n", alltext)
    rec["platform_sentences"] = [x.strip() for x in sentences if PRODUCTS.search(x)]
    return rec


def main():
    cache, out = sys.argv[1], sys.argv[2]
    toc = []
    with open(os.path.join(cache, "toc-functions.tsv"), encoding="utf-8") as f:
        for line in f.read().splitlines()[1:]:
            depth, cat, title, page = line.split("\t")
            toc.append((int(depth), cat, title, page))
    recs = []
    for depth, cat, title, page in toc:
        name = page.rsplit("/", 1)[1]
        path = os.path.join(cache, "content", name)
        meta = {"toc_title": title, "category": cat, "depth": depth,
                "url": "https://help.claris.com/en/pro-help" + page}
        if not os.path.exists(path):
            recs.append({**meta, "page": name, "missing": True})
            continue
        if depth < 2:
            s = open(path, encoding="utf-8").read()
            recs.append({**meta, "page": name, "title": title,
                         "category_page": True})
            continue
        recs.append(parse_page(path, meta))
    with open(out, "w", encoding="utf-8") as f:
        json.dump(recs, f, ensure_ascii=False, indent=1)
    n = sum(1 for r in recs if r.get("examples") is not None)
    ne = sum(len(r.get("examples", [])) for r in recs)
    npair = sum(len(e["pairs"]) for r in recs for e in r.get("examples", []))
    print(f"{len(recs)} pages, {n} function pages, {ne} examples, {npair} pairs", file=sys.stderr)


if __name__ == "__main__":
    main()
