"""Decode calceval -c results and compare them with tools/calc_polish.py.

usage: python3 -I decode_results.py <compile results .tsv> [names.tsv]

For each row of `calceval -c` output (expr, error, begin:end, context,
formatted hex, Polish hex) prints the Polish tree as an s-expression, the
formatted tokens that carry information (numbers with their format word,
text literals, fields with flags, symbols, Get selectors, calls), and whether
calc_polish.py's compiler (the calc-encoding.md rules) turns the engine's
formatted stream into the engine's Polish bytes. names.tsv holds the `to` and
`field` rows of `dump_calcs` for the file, used to resolve unqualified names.
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..'))
import calc_polish as cp  # noqa: E402


def name(op):
    row = cp.OPS.get(op)
    if not row:
        return 'op%s' % op
    return row['source_name'] or row['english_name'] or 'op%d' % op


def sexpr(n):
    k = n.kind
    if k == 'num':
        return n.num.text() if n.num.tag == 2 else ('<empty>' if n.num.tag == 0 else '<NaN>')
    if k == 'text':
        return repr(n.text.text)
    if k == 'field':
        f = n.field
        return '%s[%s:%s r%d f%d]' % ('fieldref' if f.op == 23 else 'field', f.to.hex(), f.field.hex(), f.rep, f.flags)
    if k == 'var':
        return 'v%d' % n.index
    if k == 'svar':
        return n.text.text
    if k == 'get':
        return 'Get(%s)' % cp.GET.get(n.sel, n.sel)
    if k == 'key':
        return 'key:' + n.key.hex()
    if k == 'plugin_id':
        return 'plugin:%s/%d' % (n.plugin.decode('latin-1'), n.fid)
    if k == 'empty':
        return '<00>'
    if k == 'symbol':
        return 'symbol(%r flag=%d)' % (n.text.text, n.flag)
    args = ' '.join(sexpr(a) for a in n.args)
    head = name(n.op) if n.op is not None else k
    extra = ''
    if hasattr(n, 'fn') and isinstance(n.fn, cp.Node):
        extra = ' fn=' + sexpr(n.fn)
    for a in ('x', 'split'):
        if hasattr(n, a):
            extra += ' %s=%s' % (a, getattr(n, a))
    return '(%s%s%s%s)' % (head, extra, ' ' if args else '', args)


def tokens(toks):
    out = []
    for t in toks:
        k = t.kind
        if k == 'num':
            out.append('num %s fmt=%04x' % (t.num.text(), t.fmt))
        elif k == 'text':
            out.append('text %r' % t.text.text)
        elif k == 'symbol':
            out.append('symbol %r flag=%d' % (t.text.text, t.flag))
        elif k == 'field':
            out.append('field %s:%s r%d flags=%d' % (t.field.to.hex(), t.field.field.hex(), t.field.rep, t.field.flags))
        elif k == 'svar':
            out.append('var %r' % t.text.text)
        elif k == 'getsel':
            out.append('getsel %s' % cp.GET.get(t.sel, t.sel))
        elif k == 'call':
            out.append('call key=%s' % t.key.hex())
        elif k == 'plugin':
            out.append('plugin %s/%d count=%d' % (t.plugin.decode('latin-1'), t.fn, t.count))
        elif k == 'layout':
            out.append('%s %r' % ({12: 'ws', 13: 'comment', 14: 'linecomment'}[t.op], t.text.text))
        elif k == 'op':
            out.append(name(t.op))
        else:
            out.append(k)
    return out


def main():
    cp.load_catalogs()
    names = cp.Names()
    if len(sys.argv) > 2:
        for line in open(sys.argv[2], encoding='utf-8'):
            row = line.rstrip('\n').split('\t')
            if row[0] in ('to', 'field', 'cf'):
                names.add(row)
    for line in open(sys.argv[1], encoding='utf-8'):
        line = line.rstrip('\n')
        if not line or line.startswith('#'):
            print(line)
            continue
        f = line.split('\t')
        if len(f) < 6:
            print('?? ' + line)
            continue
        expr, err, pos, ctx, fmt, pol = f[:6]
        print('%s\terr=%s pos=%s' % (expr, err, pos))
        if err != '0':
            continue
        try:
            tree = cp.decode_polish(bytes.fromhex(pol))
            print('  P: ' + sexpr(tree))
        except cp.Bad as e:
            print('  P: undecodable (%s) %s' % (e, pol))
            tree = None
        try:
            toks = cp.decode_formatted(bytes.fromhex(fmt))
            print('  F: ' + ' | '.join(tokens(toks)))
        except cp.Bad as e:
            print('  F: undecodable (%s) %s' % (e, fmt))
            toks = None
        if tree is not None and toks is not None:
            file = names.tos and next(iter(names.tos))[0]
            try:
                comp = cp.Compiler(toks, (), cp.resolver(names, file, ctx) if file else None).compile()
                same = cp.encode_polish(comp) == bytes.fromhex(pol)
                print('  spec compiler: ' + ('same' if same else 'DIFFERS: ' + sexpr(comp)))
            except (cp.Bad, RecursionError, KeyError, IndexError, AttributeError) as e:
                print('  spec compiler: ERROR %s' % e)


if __name__ == '__main__':
    main()
