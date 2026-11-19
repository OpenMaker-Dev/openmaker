"""Rebuild the deterministic input corpus; never invent expected answers."""
import json
from pathlib import Path
exprs = '''1/3
2^0.5
10^400
SetPrecision(1/3;50)
"abc12d3.4" + 0
GetAsNumber("1e3")
"10" < "9"
10 < "9"
Date(2;30;2024)
Time(25;0;0)
GetAsText(Timestamp(Date(1;1;2024);Time(1;2;3.5)))
Substitute("ababa";["a";"b"];["b";"c"])
PatternCount("aAaAa";"aa")
Position("abcabcabc";"abc";9;-2)
Proper("mCDONALD o'BRIEN déjà VU")
Trim("  a  b  ")
ValueCount("a¶b¶")
GetValue("a¶b¶";3)
JSONSetElement("{}";["z";1;JSONNumber];["a";"x¶y";JSONString])
JSONFormatElements("{\\"z\\":1,\\"a\\":2}")
JSONListKeys("{\\"z\\":1,\\"a\\":2}";"")
1/0
Evaluate("1+")
Let(x=1;y)
While([i=0;s=0];i<10;[i=i+1;s=s+i];s)
Let([x=2;y=x+1];x*y)
Case(0;1/0;1;42;99)
Choose(1;1/0;42;99)
If(1;42;1/0)
Get(ApplicationVersion)
"tab" & Char(9) & "return" & Char(13) & "LF" & Char(10) & "😀"
'''.strip().splitlines()
for n in [-10,-2,-1,0,1,2,3,9,10,100]:
    exprs += [f'{n}/3', f'{n}^2', f'Round({n}/7;4)', f'Mod({n};3)',
              f'Div({n};3)', f'Abs({n})', f'Ceiling({n}/3)', f'Floor({n}/3)']
for text in ['""', '"a"', '"a¶b¶"', '"a¶¶b"', '"  abc  "', '"é😀"']:
    exprs += [f'Length({text})', f'ValueCount({text})', f'Trim({text})', f'Left({text};2)',
              f'Right({text};2)', f'Middle({text};0;2)', f'GetValue({text};-1)',
              f'GetValue({text};1)', f'GetValue({text};2)', f'GetValue({text};3)']
for text in ['"1,234.56"','"-12.3e-4"','"abc12d3.4"','"1e3"','".5"','""','"?"','"1/2/2024"']:
    exprs += [f'GetAsNumber({text})', f'GetAsDate({text})', f'GetAsTime({text})']
for a,b in [('10','"9"'),('"10"','"9"'),('0','""'),('"a"','"A"'),('"é"','"e"')]:
    exprs += [f'{a} {op} {b}' for op in ['=','≠','<','>','≤','≥']]
exprs += ['Date(0;1;2024)','Date(13;1;2024)','Date(2;29;2023)','Date(1;0;2024)',
          'Time(-1;0;0)','Time(0;0;0.1234567)','Time(0;61;0)',
          'If(0;1/0;7)','Choose(-1;10;20)','Choose(9;10;20)',
          'Case(0;1;0;2)','Let([x=1;x=x+1];x)',
          'While([i=0];i<0;[i=i+1];i)', 'EvaluationError(Evaluate("1+"))',
          'JSONGetElement("null";"")','JSONGetElement("true";"")',
          'JSONGetElement("{}";"missing")','JSONFormatElements("[1,2,3]")',
          'Position("abcabc";"abc";1;0)', 'PatternCount("aaaa";"aa")']
Path(__file__).with_name('smoke-cases.json').write_text(json.dumps(
    [{'id':f'c{i:03}', 'expr':expr} for i,expr in enumerate(exprs,1)],ensure_ascii=False,indent=2)+'\n')
print(len(exprs))
