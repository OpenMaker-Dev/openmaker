"""Turn a .sql probe list into a calceval battery (tools/fms-calc-harness).

Source lines:
  # ...            copied (section marker)
  @ ...            copied (record context line of calceval)
  = <expression>   a calculation copied as is
  <sql>            ExecuteSQLe ( "<sql>" ; "|" ; "¶" ) with the SQL turned
                   into a FileMaker text literal
  <sql> ;; a ;; b  the same with the arguments a, b (calculation text) bound
                   to the query's ? parameters
  E <sql> ...      ExecuteSQL instead of ExecuteSQLe
  %sep <f> <r>     use the text literals <f> and <r> (FileMaker text, e.g.
                   "|" and "‖") as field and row separators from here on
The SQL is written as FileMaker should receive it: " and \\ are escaped for
the text literal here; calceval's own escapes (\\n \\r \\t \\uXXXX) still
work, a backslash for the SQL itself is written \\\\.

usage: python3 -I make_battery.py probes.sql > battery.txt
"""
import sys


def literal(sql):
    out = []
    i = 0
    while i < len(sql):
        c = sql[i]
        if c == '\\' and i + 1 < len(sql) and sql[i + 1] in 'nrtu':
            out.append(sql[i:i + 2])        # calceval escape, decoded before FileMaker
            i += 2
            continue
        if c == '\\' and i + 1 < len(sql) and sql[i + 1] == '\\':
            out.append('\\\\\\\\')          # calceval -> \\ -> FileMaker -> \
            i += 2
            continue
        out.append('\\"' if c == '"' else c)
        i += 1
    return '"' + ''.join(out) + '"'


def main():
    seps = '"|" ; "¶"'
    for line in open(sys.argv[1], encoding='utf-8'):
        line = line.rstrip('\n')
        if not line.strip():
            continue
        if line.startswith('%sep '):
            f, r = line[5:].split(' ', 1)
            seps = '%s ; %s' % (f, r)
            continue
        if line.startswith('#') or line.startswith('@'):
            print(line)
            continue
        if line.startswith('='):
            print(line[1:].lstrip())
            continue
        fn = 'ExecuteSQLe'
        if line.startswith('E '):
            fn, line = 'ExecuteSQL', line[2:]
        parts = line.split(' ;; ')
        args = ''.join(' ; ' + a for a in parts[1:])
        print('%s ( %s ; %s%s )' % (fn, literal(parts[0]), seps, args))


if __name__ == '__main__':
    main()
