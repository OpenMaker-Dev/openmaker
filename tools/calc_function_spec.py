#!/usr/bin/env python3
"""Build docs/re/catalogs/calc-function-spec.json and calc-function-vectors.tsv.

    python3 -I tools/calc_function_spec.py \\
        --docs  WORK/docs-extract.json          (tools/claris_calc_docs_parse.py)
        --ops   WORK/dbopcodes.tsv              (tools/dbopcode_table.py)
        --syms  re/work/symbols/linux/libDBEngine.txt \\
        --fmsyms re/work/symbols/linux/libFMEngine.txt \\
        --catalogs docs/re/catalogs --out docs/re/catalogs

Joins, per function and Get selector: the Claris page (format, parameter
names, return type, version, example expressions and results; the page's
prose is not kept), the engine opcode (the page's
`ui-strings-functions.kDB...Op` variable names it), the string-resource
catalog row, the implementing symbol, and the research annotations in
tools/calc_function_annotations.py (tier, host capabilities, compatibility,
server notes, engine notes, extra vectors).
"""
import argparse
import csv
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import calc_function_annotations as A  # noqa: E402

PRODUCTS = ["pro", "go", "webdirect", "server", "data_api", "cwp", "cloud"]
CATEGORY_NAMES = {
    "Text functions": "Text", "Text formatting functions": "Text formatting",
    "Number functions": "Number", "Date functions": "Date", "Time functions": "Time",
    "Timestamp functions": "Timestamp", "Container functions": "Container",
    "Japanese functions": "Japanese", "JSON functions": "JSON", "Aggregate functions": "Aggregate",
    "Repeating functions": "Repeating", "Financial functions": "Financial",
    "Trigonometric functions": "Trigonometric", "Logical functions": "Logical",
    "Artificial intelligence functions": "Artificial intelligence",
    "Miscellaneous functions": "Miscellaneous", "Get functions": "Get",
    "Design functions": "Design", "Mobile functions": "Mobile",
}
KEYWORDS = {"and", "or", "not", "xor", "true", "false", "pi", "jsonstring", "jsonnumber", "jsonobject",
            "jsonarray", "jsonboolean", "jsonnull", "jsonraw", "posixpath", "urlpath", "winpath",
            "plain", "bold", "italic", "underline", "condense", "extend", "strikethrough", "smallcaps",
            "superscript", "subscript", "uppercase", "lowercase", "titlecase", "wordunderline",
            "doubleunderline", "allstyles", "highlightyellow", "lower", "higher", "random",
            "valuenumber", "valuenumberranges", "jsonstringranges", "rowid", "rowmodid", "self",
            "roman", "greek", "cyrillic", "centraleurope", "shiftjis", "traditionalchinese",
            "simplifiedchinese", "oem", "symbol", "other", "databasenames", "layoutobjectuuid"}


def param_type(desc, name):
    d = (desc or "").lower()
    n = name.lower()
    if n in ("month", "day", "year", "hours", "minutes", "seconds", "red", "green", "blue"):
        return "number"
    if n.startswith("rgb"):
        return "number (RGB color)"
    if "variable definitions" in d:
        return "variable definition"
    if d.startswith(("any expression", "any calculation expression")):
        return "any"
    if "set of non-repeating fields" in d or "related field, repeating field" in d:
        return "field or values (related, repeating, or listed)"
    if "repeating field" in d:
        return "field (repeating)"
    if "summary field" in d:
        return "field (summary)"
    if "refers to a field's name" in d:
        return "text (field name)"
    if n == "json" or (re.search(r"\bjson\b", d) and "object or array" in d and "any expression" not in d):
        return "text (JSON)"
    if n == "keyorindexorpath":
        return "text (JSON path) or number"
    if d.startswith("any text expression") or n in ("text", "texttofilter", "filtertext", "searchstring"):
        return "text"
    if "container" in d:
        return "container"
    if re.search(r"numeric|\bnumber\b|integer|0 or false|true or false|boolean|\d+ through \d+", d):
        return "number"
    if "timestamp" in d:
        return "timestamp"
    if re.search(r"\btime\b", d) and "date" not in d:
        return "time"
    if re.search(r"\bdate\b", d) and "time" not in d:
        return "date"
    if re.search(r"\bvariable\b", d) and n.startswith(("var", "initial", "logic")):
        return "variable definition"
    if "expression" in d or "calculation" in d or "result" in d:
        return "any"
    if "field name" in d or re.search(r"\bthe name of\b", d):
        return "text (name)"
    if re.search(r"\b(any|a) (related )?field\b|field of any type|field reference|field object", d):
        return "field"
    if "text" in d or "list" in d or "string" in d or "character" in d:
        return "text"
    return "any"


def merge_params(rec):
    fmt = rec.get("format_params") or []
    docp = rec.get("parameters_doc") or []
    by = {}
    for p in docp:
        for nm in p["names"] + [p["name"]]:
            by.setdefault(nm.strip().lower(), p)
    out = []
    for p in fmt:
        base = re.sub(r"\s*=.*$", "", p["name"]).strip()
        dp = by.get(base.lower()) or by.get(re.sub(r"\d+$", "", base).lower())
        desc = dp["description"] if dp else ""
        # The page's parameter text only informs the type; it is not kept.
        out.append({"name": p["name"], "type": param_type(desc, base), "optional": p["optional"],
                    "repeatable": p["repeatable"]})
    return out


def strip_strings(expr):
    return re.sub(r'"(?:\\.|[^"\\])*"', '""', expr)


def example_conditions(expr, ctx, fnames):
    """Classify a documented example: '' when it is self-contained, else what it needs."""
    e = strip_strings(expr)
    e = re.sub(r"/\*.*?\*/|//[^\n]*", "", e, flags=re.S)
    bound = set(m.group(1).lower() for m in re.finditer(r"([A-Za-z_~$][\w~$]*)\s*=(?!=)", e))
    needs = []
    for m in re.finditer(r"(\$\$?[\w.]+|[A-Za-z_~][\w~]*(?:::[\w ]+)?)(\s*\()?", e):
        tok, call = m.group(1), m.group(2)
        low = tok.lower()
        if call or low in KEYWORDS or low in fnames or low in bound:
            continue
        if re.fullmatch(r"\d+[eE]\d+", tok):
            continue
        needs.append(tok)
    if re.search(r"\bwhen\b|\bif the\b|\bcontains\b|\bfield\b|\brecord\b|\bcurrent\b", ctx or "", re.I):
        # The page's prose says what the example needs; it is not copied.
        needs.append("context described on the Claris page")
    return sorted(set(needs))


def compat_from_sentences(sents, cur):
    notes = []
    txt = " ".join(sents)

    def setp(prods, val, note):
        for p in prods:
            cur[p] = val
        notes.append(note)

    for s in sents:
        m = re.search(r"In FileMaker WebDirect(?: and FileMaker Go)?, (?:this function|Get\(\w+\)) (?:is not supported|isn't supported)(?: and returns ([^.]+))?", s)
        if m:
            prods = ["webdirect"] + (["go"] if "and FileMaker Go" in s else [])
            setp(prods, "no", s)
            continue
        if re.search(r"not supported (?:in|for) FileMaker WebDirect", s):
            setp(["webdirect"], "no", s)
            continue
        if re.search(r"For server-side scripts, FileMaker WebDirect, (?:the )?FileMaker Data API, and Custom Web Publishing, this function is (?:not supported|unsupported)", s):
            setp(["server", "webdirect", "data_api", "cwp", "cloud"], "no", s)
            continue
        if re.search(r"In FileMaker WebDirect, the FileMaker Data API, and Custom Web Publishing, this function is not supported", s):
            setp(["webdirect", "data_api", "cwp"], "no", s)
            continue
        if re.search(r"In FileMaker WebDirect, server-side scripts, the FileMaker Data API, and Custom Web Publishing, this function returns", s):
            setp(["webdirect", "server", "data_api", "cwp", "cloud"], "partial", s)
            continue
        if re.search(r"supported only in FileMaker Pro and FileMaker Go", s):
            setp(["webdirect", "server", "data_api", "cwp", "cloud"], "no", s)
            continue
        if re.search(r"supported only in FileMaker Pro\.", s):
            setp(["go", "webdirect", "server", "data_api", "cwp", "cloud"], "no", s)
            continue
        if re.search(r"In FileMaker Go, this function is not supported", s):
            setp(["go"], "no", s)
            continue
        if re.search(r"only supported in FileMaker Go and Windows", s):
            setp(["webdirect", "server", "data_api", "cwp", "cloud"], "no", s)
            cur["pro"] = "partial"
            continue
        if re.search(r"On platforms other than iOS and iPadOS, this function is not supported", s):
            setp(["pro", "webdirect", "server", "data_api", "cwp", "cloud"], "no", s)
            continue
        if re.search(r"not supported in FileMaker Cloud", s):
            setp(["cloud"], "no", s)
            continue
        if re.search(r"In FileMaker WebDirect, this function is supported only for", s):
            setp(["webdirect"], "partial", s)
            continue
        if re.search(r"not supported and returns an empty string:\s*in FileMaker WebDirect", s):
            setp(["webdirect"], "no", s)
            continue
        if re.search(r"when evaluated by a Linux host", s):
            setp(["server", "data_api", "cwp", "cloud"], "partial", "Linux hosts: not supported, returns empty")
            continue
    if "In FileMaker WebDirect, this function returns" in txt and cur["webdirect"] == "yes":
        cur["webdirect"] = "partial"
    return notes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--docs", required=True)
    ap.add_argument("--ops", required=True)
    ap.add_argument("--syms", required=True)
    ap.add_argument("--fmsyms", required=True)
    ap.add_argument("--catalogs", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    docs = json.load(open(a.docs, encoding="utf-8"))
    pages = [d for d in docs if d.get("depth") == 2]
    ops = {}
    for r in csv.DictReader(open(a.ops, encoding="utf-8"), delimiter="\t"):
        if r["value"] != "":
            ops.setdefault(r["name"], int(r["value"]))
    dbmethods = set()
    for line in open(a.syms, encoding="utf-8", errors="replace"):
        m = re.search(r" Draco::DBDataArg::(\w+)\(", line)
        if m:
            dbmethods.add(m.group(1))
    fmfuncs = set()
    for line in open(a.fmsyms, encoding="utf-8", errors="replace"):
        m = re.search(r" Draco::(\w+)\(short, Draco::DBExpressionEnv const&, Draco::DBDataVect const&, Draco::DBData&\)", line)
        if m:
            fmfuncs.add(m.group(1))
    cat = list(csv.DictReader(open(os.path.join(a.catalogs, "calc-functions.csv"), encoding="utf-8")))
    catfn = {r["name"]: r for r in cat if r["kind"] == "function"}
    gets = {r["selector"]: r for r in csv.DictReader(open(os.path.join(a.catalogs, "get-selectors.csv"), encoding="utf-8")) if r["selector"]}
    fnames = set(n.lower() for n in catfn) | {"get"}

    get_group = {}
    for g, (tier, caps, names) in A.GET_GROUPS.items():
        for n in names:
            get_group[n] = (g, tier, caps)

    def impl_for(opname, name):
        if opname in A.IMPL_OVERRIDES:
            return A.IMPL_OVERRIDES[opname], "symbol"
        if opname in A.AGGREGATE_OPS:
            meth = opname[3:-2]
            meth = {"Stdev": "Stdev", "Stdevp": "Stdevp"}.get(meth, meth)
            return f"DBCalcSolver::SolveAggregates, DBDataArg::{meth}", "symbol"
        if name in A.DESIGN_IMPL:
            f = A.DESIGN_IMPL[name]
            return f"FMEngine Draco::{f} (registered external function)", ("symbol" if f in fmfuncs else "missing")
        if opname and opname.startswith("kDB"):
            base = re.sub(r"Op$", "", opname[3:])
            for cand in (base, name):
                if cand in dbmethods:
                    return f"DBDataArg::{cand}", "symbol"
            low = {m.lower(): m for m in dbmethods}
            if base.lower() in low:
                return f"DBDataArg::{low[base.lower()]}", "symbol"
        return None, "none"

    entries, vectors = [], []
    seen_ops = set()

    for rec in pages:
        category = CATEGORY_NAMES[rec["category"]]
        is_get = category == "Get"
        title = rec["title"]
        selector = None
        if is_get:
            selector = re.match(r"Get\s*\((\w+)\)", title).group(1)
            name = f"Get({selector})"
        else:
            name = title
        docvar = rec["engine_string_var"].split(".", 1)[1]
        opname = A.DOCVAR_TO_OPCODE.get(docvar, docvar)
        engine = {"doc_string_var": rec["engine_string_var"]}
        if opname in ops:
            engine["opcode_name"], engine["opcode"] = opname, ops[opname]
            seen_ops.add(opname)
        if is_get:
            row = gets.get(selector)
            if row:
                engine["catalog"] = {"resource": 131, "index": int(row["index"])}
                if "opcode" not in engine:
                    engine["opcode"] = 1000 + int(row["index"])
                    engine["opcode_source"] = "inferred: kDBBaseGetParameterOp (1000) + resource 131 index; no DWARF constant in the server build"
            engine["implementation"] = "DBCalcSolver::SolveGetFunction -> host environment (virtual call)"
        else:
            row = catfn.get(name)
            if row:
                engine["catalog"] = {"resource": int(row["resource_id"]), "index": int(row["index"])}
            impl, how = impl_for(opname, name)
            if impl:
                engine["implementation"] = impl
            if docvar.startswith("kDesignTable_") or docvar.startswith("kMobileTable_"):
                engine["registration"] = "FMEngine external function table (resource %s)" % ("134" if "Design" in docvar else "156")
        # tier / caps
        if is_get:
            g, tier, caps = get_group.get(selector, ("unclassified", 2, ["session"]))
            core = selector in {"ScriptParameter", "ScriptResult", "LastError", "FoundCount", "RecordNumber",
                                "CurrentDate", "CurrentTime", "CurrentTimestamp", "AccountName", "LayoutName",
                                "UUID", "ApplicationVersion", "SystemPlatform", "WindowMode", "RecordID",
                                "CurrentTimeUTCMilliseconds", "LastErrorDetail", "ScriptName"}
        else:
            if category in ("Aggregate", "Repeating", "Design"):
                tier = 2
            elif category == "Mobile":
                tier = 3
            else:
                tier = 1
            if name in A.TIER2_FUNCS:
                tier = 2
            if name in A.TIER3_FUNCS:
                tier = 3
            caps = A.HOST_CAPS.get(name, [])
            core = (tier == 1 and name not in A.TIER1_NONCORE) or name in A.CORE_TIER2
            g = None
        # compatibility
        compat = {p: "yes" for p in PRODUCTS}
        basis = ["inferred: every product links the same calculation engine (DBEngine)"]
        if is_get and g in ("ui", "device", "printing"):
            for p in ("server", "data_api", "cwp", "cloud"):
                compat[p] = "partial"
            basis.append("inferred: host engines have no window, device or printer; expect empty or 0")
        if is_get and g == "filesystem":
            for p in ("data_api", "cwp"):
                compat[p] = "partial"
        notes = compat_from_sentences(rec.get("platform_sentences", []), compat)
        ov = A.COMPAT_OVERRIDES.get(name if not is_get else selector, {})
        for k, v in ov.items():
            if k != "_note":
                compat[k] = v
        if "_note" in ov:
            notes.append(ov["_note"])
        if notes:
            basis.append("docs: product-specific statements on the function page")
        # examples
        # Examples without expression/result pairs are prose and are not kept.
        examples = []
        for ex in rec.get("examples", []):
            for pr in ex["pairs"]:
                usable = "[image:" not in pr["expression"] and pr["expression"].count("(") == pr["expression"].count(")")
                cond = example_conditions(pr["expression"], pr.get("context", ""), fnames) if usable else ["not machine-checkable (image or fragment)"]
                item = {"expression": pr["expression"], "expected": pr["expected"],
                        "example": ex["title"], "self_contained": usable and not cond}
                if cond:
                    item["needs"] = cond
                if pr.get("other_samps"):
                    item["other_results"] = pr["other_samps"]
                if pr.get("nbsp_normalized"):
                    item["nbsp_normalized"] = True
                examples.append(item)
                vectors.append((name, pr["expression"], pr["expected"], "doc",
                                "; ".join(cond) if cond else "", rec["url"]))
        entry = {
            "name": name, "kind": "get-selector" if is_get else "function", "category": category,
            "documented": True, "doc_url": rec["url"],
            "format": rec.get("format", ""), "parameters": [] if is_get else merge_params(rec),
            "returns": rec.get("returns", ""), "version_introduced": rec.get("version", ""),
            "compatibility": compat, "compatibility_basis": basis, "compatibility_notes": notes,
            "client_server": A.GET_SERVER_NOTES.get(selector, "") if is_get else "",
            "host_capabilities": caps, "tier": tier, "core": bool(core), "engine": engine,
            "examples": examples,
        }
        if is_get:
            entry["selector"] = selector
            entry["get_group"] = g
        if name in A.ENGINE_NOTES:
            entry["engine_notes"] = A.ENGINE_NOTES[name]
        if name in A.TIER2_NOTES:
            entry["tier_note"] = A.TIER2_NOTES[name]
        entries.append(entry)

    documented = {e["name"] for e in entries}
    # undocumented catalog functions
    for nm, row in catfn.items():
        if nm in documented or nm == "Get":
            continue
        opname = {"JSONUniqueValues": "kDBJsonUniqueValuesOp", "JSONRemoveValues": "kDBJsonRemoveValuesOp",
                  "JSONValueExists": "kDBJsonValueExistsOp", "JSONValueCount": "kDBJsonValueCountOp",
                  "JSONAnyValuesExist": "kDBJsonAnyValuesExistOp"}.get(nm, "")
        seen_ops.add(opname)
        fname, fparams = None, []
        sig = row["signature"]
        params = []
        for tok in re.sub(r"^\w+\s*\(|\)\s*$", "", sig).split(";"):
            t = tok.strip()
            if t:
                params.append({"name": t, "type": param_type("json" if t == "json" else "", t),
                               "optional": False, "repeatable": False})
        entries.append({
            "name": nm, "kind": "function", "category": row["category"], "documented": False,
            "doc_url": None, "purpose": row["description"], "format": sig, "parameters": params,
            "returns": "text (JSON)" if "Values" in nm and "Exist" not in nm and "Count" not in nm else "number",
            "version_introduced": "unknown (present in 26.0.3)", "compatibility": {p: "yes" for p in PRODUCTS},
            "compatibility_basis": ["inferred: shared engine; undocumented, not in the Specify Calculation list (inferred)"],
            "compatibility_notes": [], "client_server": "", "host_capabilities": [], "tier": 1, "core": False,
            "engine": {"opcode_name": opname, "opcode": ops.get(opname),
                       "catalog": {"resource": int(row["resource_id"]), "index": int(row["index"])},
                       "implementation": A.IMPL_OVERRIDES.get(opname, "")},
            "examples": [],
        })
    for sel in ("DBEngineVersion", "StudioDatasource"):
        row = gets[sel]
        opn = "kDBDBEngineVersionOp" if sel == "DBEngineVersion" else None
        if opn:
            seen_ops.add(opn)
        g, tier, caps = get_group[sel]
        entries.append({
            "name": f"Get({sel})", "kind": "get-selector", "category": "Get", "selector": sel,
            "documented": False, "doc_url": None, "purpose": "", "format": f"Get ( {sel} )", "parameters": [],
            "returns": "unknown", "version_introduced": "unknown", "compatibility": {p: "unknown" for p in PRODUCTS},
            "compatibility_basis": ["undocumented; resource 131 has the name but no signature (hidden from the Specify Calculation list, inferred)"],
            "compatibility_notes": [], "client_server": "", "host_capabilities": caps, "tier": tier, "core": False,
            "get_group": g,
            "engine": {"opcode_name": opn, "opcode": ops.get(opn) if opn else 1000 + int(row["index"]),
                       "catalog": {"resource": 131, "index": int(row["index"])},
                       "implementation": "DBCalcSolver::SolveGetFunction -> host environment (virtual call)"},
            "examples": [],
        })
    entries.append({
        "name": "IndexItems", "kind": "function", "category": "internal", "documented": False, "doc_url": None,
        "purpose": "engine-only opcode with no name in the string resources (resource 140 index 0 is #UnUsed#)",
        "format": "", "parameters": [], "returns": "unknown", "version_introduced": "unknown",
        "compatibility": {p: "unknown" for p in PRODUCTS}, "compatibility_basis": ["engine only"],
        "compatibility_notes": [], "client_server": "", "host_capabilities": [], "tier": 3, "core": False,
        "engine": {"opcode_name": "kDBIndexItems", "opcode": ops.get("kDBIndexItems"),
                   "implementation": "DBDataArg::IndexItems"}, "examples": [],
    })
    seen_ops.add("kDBIndexItems")
    # keyword constants
    for r in cat:
        if r["kind"] != "keyword-or-constant":
            continue
        nm = r["name"]
        cands = [f"kDB{nm}Op", f"kDB{nm[0].upper() + nm[1:]}Op", f"kDB{nm.upper()}Op"]
        special = {"Extend": "kDBExtendStyleOp", "Lower": "kDBLookupLowerOp", "Higher": "kDBLookupHigherOp",
                   "ShiftJIS": "kDBShiftJisOp", "Symbol": "kDBSymbolCodeSetOp", "HighlightYellow": "kDBHighlightOp",
                   "JSONStringRanges": "kDBJSONStringRangesOp", "JSONString": "kDBJsonStringOp",
                   "JSONNumber": "kDBJsonNumberOp", "JSONObject": "kDBJsonObjectOp", "JSONArray": "kDBJsonArrayOp",
                   "JSONBoolean": "kDBJsonBooleanOp", "JSONNull": "kDBJsonNullOp", "JSONRaw": "kDBJsonRawOp",
                   "not": "kDBNotOp", "and": "kDBAndOp", "or": "kDBOrOp", "xor": "kDBXorOp"}
        opn = special.get(nm) or next((c for c in cands if c in ops), None)
        entries.append({"name": nm, "kind": "constant", "category": "keyword", "documented": True,
                        "doc_url": "https://help.claris.com/en/pro-help/content/named-constants-keywords.html",
                        "engine": {"opcode_name": opn, "opcode": ops.get(opn),
                                   "catalog": {"resource": int(r["resource_id"]), "index": int(r["index"])}},
                        "tier": 1, "core": nm in ("and", "or", "not", "xor", "True", "False")})
        if opn:
            seen_ops.add(opn)

    # extra vectors
    for fn, expr, exp, src, note in A.EXTRA_VECTORS:
        vectors.append((fn, expr, exp, src, note, ""))

    out_json = os.path.join(a.out, "calc-function-spec.json")
    meta = {
        "description": ("FileMaker Pro 26 calculation functions and Get selectors: names, formats, "
                        "parameters, examples and versions from the Claris pages (without the pages' prose), joined with the "
                        "26.0.3 engine. See docs/re/calc-functions.md."),
        "generated_by": "tools/calc_function_spec.py",
        "products": PRODUCTS,
        "compatibility_values": {"yes": "supported", "partial": "supported with documented or inferred differences (see notes)",
                                 "no": "not supported (returns empty, 0 or the noted value)", "unknown": "not established"},
        "tiers": {"1": "pure", "2": "needs record, schema, session or script context", "3": "host-, platform- or service-bound"},
        "counts": {
            "documented_functions": sum(1 for e in entries if e["kind"] == "function" and e["documented"]),
            "documented_get_selectors": sum(1 for e in entries if e["kind"] == "get-selector" and e["documented"]),
            "undocumented_functions": sum(1 for e in entries if e["kind"] == "function" and not e["documented"]),
            "undocumented_get_selectors": sum(1 for e in entries if e["kind"] == "get-selector" and not e["documented"]),
            "constants": sum(1 for e in entries if e["kind"] == "constant"),
        },
    }
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump({"meta": meta, "entries": entries}, f, ensure_ascii=False, indent=1)
        f.write("\n")

    def esc(s):
        return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\r", "\\r").replace("\n", "\\n")

    out_tsv = os.path.join(a.out, "calc-function-vectors.tsv")
    with open(out_tsv, "w", encoding="utf-8") as f:
        f.write("function\texpression\texpected\tsource\tconditions\tdoc_url\n")
        for v in vectors:
            f.write("\t".join(esc(x or "") for x in v) + "\n")
    print(json.dumps(meta["counts"]), f"{len(vectors)} vectors", file=sys.stderr)
    unmatched = sorted(n for n in ops if n not in seen_ops)
    print("engine opcodes not attached to an entry:", len(unmatched), file=sys.stderr)


if __name__ == "__main__":
    main()
