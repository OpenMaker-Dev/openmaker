# The word-break probe of query-program.md 7.2 (#300): contacts-en's
# saveAsXML with Contacts::V (text, not indexed) and one stored, fully
# indexed calculation K<id> = V per index language, and a battery that
# enters each probe value as a record of its own; then the catalog rows of
# docs/re/catalogs/word-break-vectors.tsv from the run copy's indexes.
#   python3 -I wordbreaks.py schema <contacts-en.xml> <out.xml> docs/re/catalogs/locale-types.csv
#   python3 -I wordbreaks.py battery <out.txt>
#   SCHEMA=<out.xml> runbat.sh <out.txt> contacts-en.fmp12 <results>
#   cargo run -p om-hbam --example index_entries -- <$WORK/out.contacts-en.gen.run.fmp12> Contacts V > entries.tsv
#   python3 -I wordbreaks.py rows entries.tsv > rows.tsv
import csv
import sys

SEPARATORS = ['.', ',', ':', '/', '-', "'", '\u2019', '\u00b7', '_', ';', '\u00a0']
LANGUAGES = [1, 2, 3, 4] + list(range(16, 66)) + [67, 76]
TO = ('<TableOccurrenceReference id="1065090" name="Contacts" '
      'UUID="06EEB6C5-DA8C-475E-A69F-EA5B69C6A96D"></TableOccurrenceReference>')


def values():
    out = []
    for c in SEPARATORS:
        out += [f'1{c}2', f'12{c}30', f'1{c}234', f'1{c}2{c}3', f'ab{c}cd', f'l{c}eau',
                f'1{c}b', f'a{c}1', f'{c}1', f'1{c}', f'{c}a', f'a{c}']
    return out + ['25/12/2026', '25.12.2026', '2026-12-25', '12:30:45', '12.30',
                  '1 234,5', '1.234,5', "1'234.5"]


def schema(src, dst, catalog):
    s = open(src, encoding='utf-8').read()
    names = {int(r['type']): r['name'] for r in csv.DictReader(open(catalog, encoding='utf-8'))}
    tail = '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>'
    fields = [
        '<Field id="14" name="V" fieldtype="Normal" datatype="Text" comment="">'
        '<AutoEnter type="" prohibitModification="False"></AutoEnter>'
        '<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" '
        'notEmpty="False" unique="False" existing="False"></Validation>'
        '<Storage autoIndex="False" index="None" global="False" maxRepetitions="1">'
        '<LanguageReference name="English" id="21"></LanguageReference></Storage>' + tail
    ]
    for i, lang in enumerate(LANGUAGES):
        fields.append(
            f'<Field id="{15 + i}" name="K{lang}" fieldtype="Calculated" datatype="Text" comment="">'
            '<AutoEnter alwaysEvaluate="False"></AutoEnter>'
            '<Storage storeCalculationResults="True" autoIndex="True" index="All" global="False" '
            f'maxRepetitions="1"><LanguageReference name="{names[lang]}" id="{lang}"></LanguageReference>'
            f'</Storage><Calculation>{TO}<Text><![CDATA[V]]></Text></Calculation>' + tail)
    i = s.find('name="ModificationTimestamp"')
    end = s.find('</Field>', i) + len('</Field>')
    s = s[:end] + ''.join(fields) + s[end:]
    k = s.rfind('<key>13</key>', 0, i)
    keys = ''.join(f'<key>{n}</key>' for n in range(13, 15 + len(LANGUAGES)))
    s = s[:k] + keys + s[k + len('<key>13</key>'):]
    open(dst, 'w', encoding='utf-8').write(s)


def escape(v):
    out = []
    for ch in v:
        if ch == '\\':
            out.append('\\\\')
        elif ch == '\t':
            out.append('\\t')
        elif ord(ch) < 0x20 or ord(ch) > 0x7e:
            out.append('\\u%04x' % ord(ch))
        else:
            out.append(ch)
    return ''.join(out)


def battery(dst):
    lines = ['@generate', '', '== Fill', 'Go to Layout [ "Contact Details" ]', 'New Record/Request',
             'Set Field [ Contacts::V ; Get ( ScriptParameter ) ]',
             'Commit Records/Requests [ With dialog: Off ]', 'Exit Script [ Contacts::V ]', '', '--- run']
    lines += [f'Fill\t{escape(v)}\tGet ( LastError )' for v in values()]
    open(dst, 'w', encoding='utf-8').write('\n'.join(lines) + '\n')


def rows(entries):
    """The catalog's rows: every value in English (21); another language
    only where its keys differ from English's, System (1) only where they
    differ from Unicode standard's (3); in probe order."""
    keys = {}
    for line in open(entries, encoding='utf-8'):
        name, lang, _record, value, value_keys, word_keys = line.rstrip('\n').split('\t')
        if name.startswith('K') and name[1:] == lang:
            keys[(int(lang), value)] = (value_keys, word_keys)
    order = [escape_row(v) for v in values()]
    out = []
    for lang in [21] + [l for l in LANGUAGES if l != 21]:
        base = 3 if lang == 1 else 21
        for v in order:
            k = keys[(lang, v)]
            if lang == 21 or k != keys[(base, v)]:
                out.append(f'text\t{lang}\t{v}\t{k[0]}\t{k[1]}')
    sys.stdout.write('\n'.join(out) + '\n')


def escape_row(v):
    """A value as `index_entries` writes it."""
    out = []
    for ch in v:
        if ch in '\r\n\t\\':
            out.append({'\r': '\\r', '\n': '\\n', '\t': '\\t', '\\': '\\\\'}[ch])
        elif ord(ch) < 0x20 or ch in '\u00a0\u00ad\ufeff' or 0x200b <= ord(ch) <= 0x200f:
            out.append('\\u{%x}' % ord(ch))
        else:
            out.append(ch)
    return ''.join(out)


if __name__ == '__main__':
    {'schema': lambda: schema(*sys.argv[2:5]), 'battery': lambda: battery(sys.argv[2]),
     'rows': lambda: rows(sys.argv[2])}[sys.argv[1]]()
