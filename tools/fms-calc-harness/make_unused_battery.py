"""Generate batteries/c02-unused.txt from the opcode catalogs.

usage: python3 -I make_unused_battery.py docs/re/catalogs/calc-tokens.csv \
    docs/re/catalogs/get-selectors.csv > batteries/c02-unused.txt

One minimal call per calc-tokens.csv row with corpus_all = 0 (functions,
operators, constants, Get selectors) and per built-in plug-in function, and a
second call with every optional argument. Field parameters get Table::Input
(the calc-oracle file's context table occurrence "Table").
"""
import csv, re, sys
tok = list(csv.DictReader(open(sys.argv[1])))
sel = {r['index']: r['selector'] for r in csv.DictReader(open(sys.argv[2]))}
FIELD = re.compile(r'^(field|repeatingField|summaryField|breakField|sourceField)$')
def arg(p):
    p = p.strip()
    if FIELD.search(p): return 'Table::Input'
    if re.search(r'date', p, re.I): return 'Date ( 1 ; 2 ; 2024 )'
    if re.search(r'number|count|size|seed|precision|day|flag|accuracy|timeout|major|minor|option|dimension|ignore|choices|periods|rate|payment|principal|term|angle|lineEndings|characterType|separator$|trim', p, re.I): return '1'
    return '"a"'
out = []
FIXED = {
    'LunarDate': ['LunarDate ( 1 )'],
    'SolarDate': ['SolarDate ( 1 ; 1 )'],
    'TextColorRemove': ['TextColorRemove ( "a" )', 'TextColorRemove ( "a" ; RGB ( 1 ; 2 ; 3 ) )'],
    'WindowNames': ['WindowNames', 'WindowNames ( "a" )'],
    'ExecuteSQLe': ['ExecuteSQLe ( "a" ; "b" ; "c" )', 'ExecuteSQLe ( "a" ; "b" ; "c" ; 1 ; 2 )'],
    'Average': ['Average ( Table::Input )', 'Average ( 1 ; 2 ; 3 )'],
    'NPV': ['NPV ( Table::Input ; 1 )'],
}
for r in tok:
    if r['kind'] == 'builtin-plugin' or (r['corpus_all'] == '0' and r['kind'] in ('function', 'operator', 'constant', 'get-selector')):
        k, sig, name = r['kind'], r['signature'], r['source_name'] or r['english_name']
        if k == 'get-selector':
            s = sel.get(str(int(r['opcode']) - 1000), '')
            out.append('# %s %s' % (r['opcode'], r['stored']))
            out.append('Get ( %s )' % s if s else '# (no selector name)')
            continue
        if k == 'operator':
            out.append('# %s %s operator' % (r['opcode'], r['stored'])); out.append('1 %s 0' % name); continue
        if k == 'constant':
            out.append('# %s %s constant' % (r['opcode'], r['stored'])); out.append(name); continue
        out.append('# %s %s %s nargs=%s min=%s max=%s' % (r['opcode'], r['stored'], name, r['nargs'], r['min_args'], r['max_args']))
        m = re.match(r'\s*([^(]+?)\s*(\{?\s*\((.*)\)\s*\}?)?\s*$', sig) if sig else None
        if name in FIXED:
            out.extend(FIXED[name]); continue
        if not m or not m.group(3):
            out.append(name)
            continue
        body = m.group(3)
        req = body.split('{')[0]
        req_args = [a for a in req.split(';') if a.strip()]
        allargs = [a for a in re.sub(r'[{}]', '', body).split(';') if a.strip() and '...' not in a]
        out.append('%s ( %s )' % (name, ' ; '.join(arg(a) for a in req_args)) if req_args else name)
        if len(allargs) > len(req_args):
            out.append('%s ( %s )' % (name, ' ; '.join(arg(a) for a in allargs)))
print("# Compile mode (calceval -c -d), calc-oracle.fmp12, context table occurrence \"Table\" (03d00001).")
print("# One minimal call per calc-tokens.csv row with corpus_all = 0 and per built-in plug-in")
print("# function; a second line calls it with every optional argument.")
print("\n".join(l for l in out if l != "# (no selector name)"))
