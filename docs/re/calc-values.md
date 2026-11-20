# Calculation values and evaluation semantics

The values FileMaker calculations operate on and the rules the evaluator
applies to them: numbers, text, dates, times, timestamps, the `?` result,
conversions, comparisons, errors, evaluation order, variables and limits, and
the boundary between the evaluator and its host. The stored formula encoding
is `calc-encoding.md`'s; per-function behavior is `calc-functions.md`'s. This
document owns what every function shares.

Reference build: FileMaker Server 26.0.3.309 for Ubuntu, `libDBEngine.so`
(calc engine) and `libSupport.so` (`Draco::FixPt`, text, `DateTime`), the
same engine FileMaker Pro 26.0.3 links. Addresses are Linux virtual addresses
in those libraries.

## 0. Evidence and how to read it

Markers, as in the other specs:

- **[O-run]** observed by running FileMaker Server's own engine (below).
- **[O-code]** observed in code or data: symbols, DWARF, disassembly, tables.
- **[D]** Claris documentation.
- **[I]** inference; needs the FileMaker Pro oracle.

Most claims here are [O-run]. `tools/fms-calc-harness` loads
`libDBEngine.so`/`libSupport.so` in a network-less amd64 container, opens a
copy of a shipped template the way FMDeveloperTool does
(`ApplicationInfo::SetApplicationInfo`, `DBEngineController::Start`, a
`kSecurityByPass` `DBSession`, `DBSession::OpenFileUser`) and calls
`DBExpressionEnv::Evaluate(Text const&, DBData&)` (`0xdb51d0`), the entry
that compiles formula text and solves it with the same `DBCalcSolver` stored
formulas use. It prints the returned error code, the result's types and its
text. About 2,000 expressions were run (batteries in
`tools/fms-calc-harness/batteries/`); the results are the test vectors in
`catalogs/calc-value-vectors.tsv` (section 11).

The context of most runs is "file open, no table, no record, script level
0, server app type". The harness can also evaluate in a table occurrence on
one record of a sample file (`DBRecordCursor`, section 12.1), which covers
field references, repetitions of fields, related sets and custom functions
in files that already have them. Still not covered, so this document falls
back to code reading or inference there: `Self` (its parse needs a field
context `Evaluate` does not give), summary field values, sorted
relationships, scripts (`Get(LastError)`), windows and layouts, FileMaker
Pro's "use system formats" option, and the client app type.

Text in result columns uses the catalog escapes `\r`, `\n`, `\t`, `\\`.
`¶` in an expression is FileMaker's return literal.

## 1. The value: `DBData`

A calculation value is a `Draco::DBData` (DWARF, 0x140 bytes) [O-code]:

| Offset | Member | Meaning |
|---|---|---|
| 0x00 | `mCachedType` | type of the cached representation (`DBDataType`) |
| 0x04 | `mNativeType` | the value's type |
| 0x08 | `mCalcNativeType` | type a calculation assigned |
| 0x0d, 0x0e | `mTextIsCurrent`, `mNativeIsCurrent` | which representation is valid |
| 0x10 | `mTextDataPtr` | the text (`Draco::Text`, styled) |
| 0x18 | `mNumericData` | `FixPt` |
| 0x58, 0x88, 0xb8 | `mDateData`, `mTimeData`, `mTimeStampData` | `DateTime` |
| 0xf0 | `mJsonData` | parsed JSON (`Json::Value`, shared) |
| 0x100 | `mBinaryData` | container data |
| 0x118 | `mLocale` | locale used to convert between text and native forms |

`DBDataType` [O-code]: 0 invalid, 1 text, 2 number, 3 date, 4 time,
5 timestamp, 6 binary (container), 7 boolean, 8 JSON value, 9 unknown.

A value keeps a text form and a native form side by side and converts
lazily (`ConvertToText`, `ConvertToNumber`, `ConvertToDate`, ...). The text
form is what `&`, text functions and text comparisons see; the native form is
what arithmetic sees. A value parsed from text keeps details of the text: a
date typed as `1-2-2003` displays with `-` (section 4.5). [O-code, O-run]

Values a calculation can produce [O-run]:

- **text** (possibly styled, section 5.2), **number**, **date**, **time**,
  **timestamp**, **container**; boolean results are numbers 0/1; JSON
  functions return text.
- **empty**: every type has an empty state (`FixPt::mIsEmpty`,
  `DateTime::mIsEmpty`, empty text). Section 6.4 gives the operator rules.
- **`?`**: the invalid result. Its text is `?`; as a number it is NaN
  (`FixPt` parse result `kNaNResult`, `DBDataArg::SetToNaN` `0xdef3a0`,
  `SetToQuestion` `0xdbfa10`). Section 7.

## 2. Numbers (`Draco::FixPt`)

### 2.1 Representation [O-code]

`Draco::FixPt` (libSupport, DWARF, 0x40 bytes) is a sign-magnitude decimal
number held in base 10^9 words (`Digit` = `uint32`, values 0..999,999,999):

| Offset | Member | Meaning |
|---|---|---|
| 0x00 | `mPrecision` (i16) | total fractional digits = display precision + extra |
| 0x02 | `mExtraPrecision` (i16) | guard digits on top of the display precision |
| 0x04 | `mFractSize` (u16) | fraction words allocated |
| 0x06 | `mFractUsed` (i16) | fraction words in use |
| 0x08 | `mWholeSize` (u16) | integer words allocated |
| 0x0a | `mUsed` (i16) | integer words in use |
| 0x0c | `mNegative` | sign |
| 0x10, 0x18 | `mWholeBegin[2]`, `mFractBegin[3]` | inline words |
| 0x28, 0x30 | `mWhole`, `mFract` | word arrays when the inline ones are too small |
| 0x38 | `mIsEmpty` | empty number |
| 0x3c | `mLastParsingResults` | `kEmpty`, `kPure`, `kImpure`, `kInvalid`, `kNaNResult` |

Integer word 0 is the least significant; fraction word 1 is the first
nine digits after the point. `FixPt::SetPrecision(int p)` (`0x166a30`)
clamps p to [16, 800], stores p + extra in `mPrecision` and allocates
ceil((p + extra) / 9) fraction words. `SetExtraPrecision` (`0x16c520`)
applies the same clamp. There is no negative zero (section 2.7).

A model that reproduces every observation below [O-run, I for the model]:

- A number has a **display precision** p (fractional digits) and holds up to
  **p + 2** fractional digits (two guard digits).
- Default p is 16. `DBCalcFormula::Solve` (`0xdaf890`) sets 16 at the start
  of every evaluation [O-code].
- A numeric literal or text converted to a number gets p = max(16, number of
  fractional digits written) and holds exactly the digits written:
  `0.12345678901234567` (17 digits) displays all 17; `GetAsNumber("0.1234567890123456789")`
  keeps 19.
- An arithmetic result gets p = max(p of the operands) and holds p + 2
  digits: `2/3` holds `0.666666666666666667`, so `(2/3)*10^18` is
  `666666666666666667` and `(1/3)*10^20` is `33333333333333333300`.
- `SetPrecision(expr; n)` (section 2.6) evaluates `expr` at p = clamp(n, 16,
  400) and returns its value rounded to exactly p digits:
  `SetPrecision(2/3;16)*10^18` = `666666666666666700`,
  `SetPrecision(2/3;17)*10^18` = `666666666666666670`.
- Display (number to text, section 2.7) and comparison (section 2.5) round to
  p; truthiness (section 2.8) and further arithmetic use all held digits:
  `If(1/10^17;1;0)` = 1 while `1/10^17` displays `0`, and
  `(10^-17)*10^17` = 1.

### 2.2 Stored literal layout [O-code]

`FixPt::WriteAsPolish` (`0x16bfc0`) and `ReadAsPolish` (`0x16c180`) write a
number in a formula stream (and wherever else the engine serializes a
`FixPt`) as:

| Bytes | Content |
|---|---|
| 1 | 0 = empty, 1 = NaN (`?`), 2 = value; nothing follows 0 or 1 |
| 2 | integer word count `mUsed` as big-endian i16, negated when the number is negative |
| 2 | display precision (`mPrecision - mExtraPrecision`), big-endian u16 |
| 4 each | `mUsed` integer words then `mFractUsed` fraction words, most significant first, each a big-endian u32 in 0..999,999,999 |

An integer literal at default precision is 1 + 2 + 2 + 3 × 4 = 17 bytes:
one integer word and two (zero) fraction words. How many fraction words a
literal of a given precision carries is for `calc-encoding.md` to confirm
on the corpus [I: ceil((p + 2) / 9)].

### 2.3 Arithmetic [O-run]

Result types are in section 6.1. For numbers:

| Operation | Rule |
|---|---|
| `+` `-` | exact on the held digits; p = max; but operands of opposite sign that are equal at the larger display precision give exactly 0 (`(1/3 - .3333333333333333)*10^20` = 0; 14.2) |
| `*` | p = max; the product is formed row by row and truncated to the held digits (below and 14.2) |
| `/` | p = max; quotient rounded half-up at p + 2 digits: `1/6*10^18` = `166666666666666667`, `-2/3*10^18` = `-666666666666666667` (divisors of more than nine significant digits go through Newton division, which agrees except within about 10^-6 units of a tie; 14.2) |
| `^` | section 2.4 |
| unary `-` | negation; `-0` is 0 |
| `Mod(x;y)` | x − y·floor(x/y), sign of the divisor: `Mod(-7;3)` = 2, `Mod(7;-3)` = -2, `Mod(-7.5;2)` = .5; `Mod(x;0)` = x, no error |
| `Div(x;y)` | floor(x/y): `Div(-7;3)` = -3, `Div(-7.5;2)` = -4; `Div(x;0)` is error 15 |
| `Int(x)` | truncation toward zero: `Int(-2.9)` = -2 |
| `Round(x;n)` | half away from zero at n digits (n truncated toward zero, negative n rounds left of the point): `Round(-2.5;0)` = -3, `Round(1.005;2)` = 1.01, `Round(1250;-2)` = 1300, `Round(1.55;1.9)` = 1.6 |
| `Truncate(x;n)` | toward zero; `Truncate(2/3;17)*10^18` = `666666666666666700` shows it first rounds x to p |

Row product [O-code, O-run; 14.2 has the exact rule]: with R = ceil((p + 2)
/ 9) fraction words kept, each word of the right operand times the whole
left operand is formed exactly (with its carries), the words of that row
after fraction word R are dropped, and the rows are summed and truncated to
p + 2 digits. This is why `(1/3)*(1/3)*10^18` is `111111111111111109`
(exact truncation would give `...110`) while
`0.333333333333333333*0.333333333333333333*10^18` (p = 18, R = 3) is
`111111111111111110.88`. Carries out of dropped words do reach word R, and
the result depends on the operand order: `(2/3)*(1/7)*10^18` is
`95238095238095237`, `(1/3)*(2/3)*10^18` is `222222222222222221` but
`(2/3)*(1/3)*10^18` is `222222222222222222` (battery `t13-arith`; a short
product that adds only the high word of the products just past word R
misses these). `FixPt::Multiply` is `0x169dc0`; the vectors in batteries
`14-mul` and `t13-arith` check it.

Magnitude limits [O-run]: results of `*`, `+`, `-` reach at least 10^800
(`(10^400)*(10^400)` = `1.0E+800`) and `Exp(1400)` (about 10^608) succeeds;
`^` fails with error 14 when the result reaches 10^405 (`10^404` succeeds,
`10^405` and `2^1350` do not); `Factorial(300)` fails with 14. Literals of
magnitude 10^800 or more, and below 10^-799, are parse error 1218
(`1e800`, `1e-800`); text that large converts to `?` without an error
(`GetAsNumber("1e800")`). Results smaller than the held digits underflow to 0
(`10^-500` = 0, `Exp(-1000)` = 0).

Division by zero: `/`, `Div` and `0^-1` give `?` with error 15; empty or
text without digits divides as 0 (`1/""` is error 15).

### 2.4 Powers, roots and transcendental functions [O-run, O-code]

Two families:

- **Decimal** (computed in `FixPt`): `Sqrt`, `Exp`, `Ln`, `Log`, `Lg`,
  `^`, `Factorial`, `FV`, `PMT`, `PV`, `NPV`. Section 14 specifies the
  algorithms digit for digit (validated on 7,504 engine results). In short:
  `Exp` reduces x by k = round(x / ln 10) with a 45-digit ln 10 and sums
  the Taylor series at max(45, p + 4 + k) digits; `Ln` scales into
  [0.6, 1.4] and sums ln(1 + z) at p + 4 digits; `Log` and `Lg` multiply
  `Ln` by 1/ln 10 or 1/ln 2; `x^y` takes the integer part of y by binary
  exponentiation at p + 10 digits (every product truncated) and the
  fraction as `Exp(f · Ln(x))`; `Sqrt(x)` is `x^0.5`. Results carry guard
  digits by function (`Exp` and `Lg` 4, `Ln` 3, `Log` 5), so
  `Exp(1)*10^18` shows `2718281828459045235.36`. They are not exact in the
  last digits: `Sqrt(10^30)` is `999999999999999.9999999999999345`,
  `Exp(100)` is right to 43 significant digits, `10^15.5` to about 29,
  `1.5^100` holds `…041643` (exact `…0416710…`); at high precision `Exp`,
  `Sqrt` and fractional powers are right to only about 45 significant
  digits once x reaches ln 10 / 2 (14.3), while `SetPrecision(Pi;400)`,
  `SetPrecision(Exp(1);400)`, `SetPrecision(Ln(2);400)` are right to all
  digits. Integer powers are exact while the exact power fits in p + 12
  fractional digits (`3^40`, `7^25`, `2^70`); a negative exponent divides 1
  by the power at that precision (`0.1^-404` is error 15 because `0.1^404`
  is 0 there).
- **Binary** (libm on a C `double`, 14.8): `Sin`, `Cos`, `Tan`, `Atan`,
  `Acos`, `Asin`, `Atan2`. `FixPt::Sin` (`0x1759c0`) reduces the argument
  with `Mod2Pi`, calls `sin`, and the solver assigns the double back with
  `FixPt::operator=(double)` (`0x1670b0`), which formats it with
  `snprintf("%.20g")` and parses that [O-code]. So these results carry 20
  significant digits of the double:
  `SetPrecision(Sin(1);50)` = `.84147098480789650488`, `Tan(Pi/2)` =
  `16331239353195370`, `Atan(1)*4` = `3.1415926535897931`, `Sin(Pi)` =
  `1.0E-16`.

Domain errors [O-run]: `Sqrt` of a negative, `Ln` and `Log` of zero or a
negative, a negative base with a fractional exponent (`(-8)^(1/3)`), and
`Factorial` of a negative are error 14; `Asin`/`Acos` outside [-1, 1] are
error 1218. `0^0` = 1, `0^0.5` = 0.

### 2.5 Comparing numbers [O-run]

Two numbers are compared after rounding both to the larger of their display
precisions: `1/3 = 0.33333333333333333` (17 digits) is 1,
`1/3 = 0.3333333333333333333` (19) is 0, `1 = 1.00000000000000001` is 0,
`2/3 = 0.6666666666666667` is 1, `1/3*3 = 1` is 1, `(1/7)*(1/7)*49 = 1` is 0
(it displays `.9999999999999999`). `FixPt::SetMaxPrecision` (`0x16c9d0`),
`operator==` (`0x169440`) and `operator<` (`0x168a60`) are the code [O-code;
the rounding rule is I from the vectors].

### 2.6 `SetPrecision` [O-run, O-code]

`SetPrecision(expr; n)` clamps n to [16, 400] (`SolveSetPrecision`
`0xe46ab0` compares with 400; values below 16 and empty give 16; n is truncated, `"20"` and `20.9` give 20). The precision applies
dynamically to everything evaluated inside `expr`, including `Let`, `If`,
`Evaluate` and nested arithmetic; the innermost `SetPrecision` wins
(`DBPolishEnv::mPrecisionStk`). Values computed inside keep their precision
outside: `SetPrecision(1/3;30)*3` has 30 digits, and
`1/3 + SetPrecision(1/3;30)` = `.666666666666666666333333333333`. An integral
result loses the raised precision: `SetPrecision(1;20)/3` has 16.

### 2.7 Number to text [O-run, O-code]

`FixPt::GetText` (`0x179030`) as used for `&`, text functions,
`GetAsText` and display:

1. Round to the display precision p, half away from zero.
2. If the rounded value is zero, the text is `0` (never `-0`; `-0`,
   `0*-1`, `Int(-0.5)`, `Round(-0.4;0)` all give `0`).
3. Decide plain or scientific (`FixPt::ShouldUseScientific`, `0x16d150`):
   scientific when |x| ≥ 10^9 and the lowest nine integer digits are all
   zero, or when 0 < |x| < 10^-9 (the first nine fractional digits are all
   zero). Otherwise plain. So `10^6` is `1000000`, `10^9` is `1.0E+9`,
   `12345678901*10^6` is `12345678901000000`, `10^15+0.5` is
   `1.0000000000000005E+15`, `10^-9` is `.000000001`, `10^-10` is `1.0E-10`.
4. Plain: optional `-`, the integer digits without leading zeros and with no
   digit at all when the integer part is 0 (`.5`, `-.5`), then the decimal
   separator and the fractional digits with trailing zeros removed; no
   separator when nothing remains. No thousands separators.
5. Scientific: optional `-`, the first significant digit, the separator,
   the remaining significant digits with trailing zeros removed or `0` if
   none, `E`, `+` or `-`, and the decimal exponent without padding:
   `1.0E+20`, `1.5E+20`, `9.313226E-10`, `-1.0E-15`.

The decimal separator is the locale's: `,3333333333333333` and `1,0E+20` in
de-DE, fr-FR and sv-SE files. Lengths and positions of text functions apply
to this text: `Length(10^20)` = 7, `Right(10^15;5)` = `0E+15`.

### 2.8 Text to number [O-run]

`FixPt::Set(unistring const&, Locale const&)` (`0x176d70`) and the parsers
behind `GetAsNumber`, arithmetic on text and truthiness. Two modes
(`ParsingResults` `kPure`, `kImpure`):

- **Pure**: the whole text, after trimming spaces, is an optional sign,
  digits with optional thousands separators, an optional decimal separator
  and digits, and an optional exponent `e`/`E` with optional sign and digits.
  It converts exactly: `1e3` = 1000, `1e-3` = .001, `1,5e3` = 15000 in en-US
  (`,` is a thousands separator there) and 1500 in de-DE.
- **Impure** (anything else): every digit is collected in order; the first
  decimal separator places the point and later ones are ignored; thousands
  separators, letters, spaces, punctuation and `e` are ignored; the number is
  negative if a `-` appears before the first digit or the text contains
  `(` ... `)` around the digits (the exact rules, with the decimal
  separator and parenthesised pure numbers, are in 13, value-core
  follow-ups).

| Text (en-US) | Number | Text (en-US) | Number |
|---|---|---|---|
| `abc12d3.4` | 123.4 | `1.2.3` | 1.23 |
| `1-2` | 12 | `a-1` | -1 |
| `--12` | -12 | `12-` | 12 |
| `(12)` | -12 | `$1,234.56` | 1234.56 |
| `1,5` | 15 | `1.234,56` | 1.23456 |
| `1.5e2x` | 1.52 | `1e3e4` | 134 |
| `1/2/2003` | 122003 | `12:30` | 1230 |
| `5.` | 5 | `.5` | .5 |
| `１２３` (fullwidth) | 123 | `١٢٣`, `¼` | empty |
| `abc`, ` `, `-`, `.` | empty number | `?` | NaN (`?`) |
| `1e800` | `?`, no error | `0.000000000000000000001` | 1.0E-21 (p = 21) |

In de-DE, fr-FR and sv-SE the roles of `.` and `,` swap (`1,5` = 1.5,
`1.5` = 15, `1.234,56` = 1234.56). ja-JP behaves like en-US.

A text without digits is an empty number; in arithmetic an empty operand
counts as 0 when the other operand is not empty (section 6.4). `GetAsNumber("")`
returns its argument unchanged, an empty *text*.

### 2.9 Truthiness [O-run]

`If`, `Case`, `and`, `or`, `xor`, `not`, `While` conditions and
`GetAsBoolean` (`DBData::GetAsBoolean` `0xe9e1b0`) treat a value as true when
its number is not zero, using all held digits:

- numbers: `If(0.0001)` true, `If(1/10^17)` true, `If(10^-20)` false (it
  underflowed to 0);
- text: the impure text-to-number result: `"a1"`, `"1x"`, `"e5"` true;
  `"abc"`, `"0.0"`, `"x0"`, `""`, `" "` false;
- `?` (and the text `"?"`, NaN) is **true**: `If(1/0;"t";"f")` = `t` (with
  error 15), `Case(1/0;1;2)` = 1, `not (1/0)` = 0;
- dates are true (day numbers start at 1), `Time(0;0;0)` is false
  (timestamps are false only at 1/1/0001 00:00:00 [I]).

## 3. The text form of non-numbers

Covered with each type: dates and times in section 4.5, styled text in 5.2,
the `?` result in 7.2.

## 4. Dates, times and timestamps

### 4.1 Representation [O-code]

`Draco::DateTime` (libSupport, DWARF, 0x30 bytes): `mYear`, `mMonth`,
`mDay` (i16), `mGMTOffset` (i16), `mHour` (i32, unbounded for elapsed
times), `mMinute` (i16), `mSec` (i32), `mMicroSec` (i32), `mIsEmpty`,
`mOutputGMTOffset`, `mOutputMicroSecDigits` (fractional digits to display),
`mDateStyle`, `mTimeStyle` (`kNoTime`, `kElapsed`, `kHourOnly`,
`kTimeOfDay`, `kAmbiguous`), `mSepChar` (date separator to display),
`mLastParsingResults`.

Numeric values [O-run]:

| Type | As a number | Range |
|---|---|---|
| date | day number, 1 = 1/1/0001, proleptic Gregorian (`Date(1;1;1600) - Date(1;1;1500)` = 36524; 10/5-10/14/1582 exist; 1/1/0001 is a Monday) | 1 (1/1/0001) .. 1460970 (12/31/4000); 730486 = 1/1/2001, 719163 = 1/1/1970 |
| time | signed seconds, 6 fractional digits (microseconds) | unbounded hours: `Time(1000000;0;0)` = `1000000:00:00` |
| timestamp | seconds since 1/1/0001 00:00:00 (= 0), 6 fractional digits | 0 .. 12/31/4000 23:59:59.999999; 1/1/2001 00:00 = 63113904000, 1/1/1970 = 62135596800 |

A date as a timestamp is (day − 1) × 86400 seconds. Fractional seconds are
truncated (not rounded) to microseconds in the value itself:
`GetAsNumber(Time(0;0;1/3))` = .333333, `Time(0;0;0.9999999)` =
`0:00:00.999999`, `Time(0;0;0.0000009)` = `0:00:00`.

Out of range dates and timestamps are `?` with error 500 (dates: `Date(1;1;4001)`,
`Date(1;1;0)`, `Date(12;31;4000) + 1`, `GetAsDate(0)`, `GetAsTimestamp(-1)`);
time text that cannot be parsed is error 501.

### 4.2 Construction [O-run]

`Date(m;d;y)`, `Time(h;m;s)`, `Timestamp(date;time)` normalize overflowing
and negative components: `Date(2;29;2023)` = 3/1/2023, `Date(13;1;2020)` =
1/1/2021, `Date(0;1;2020)` = 12/1/2019, `Date(1;0;2020)` = 12/31/2019,
`Date(1;400;2020)` = 2/3/2021; `Time(0;90;0)` = 1:30:00, `Time(-1;30;0)` =
-0:30:00, `Time(1.5;0;0)` = 1:30:00. Fractional months, days and years are
truncated (`Date(1.9;1.9;2020.9)` = 1/1/2020). Years are taken literally
(`Date(1;1;99)` = 1/1/0099). Empty or non-numeric text arguments to `Date`
are error 500; to `Time` they count as 0. `Timestamp(date;"")` is empty;
`Timestamp("";time)` is error 500. Details belong to `calc-functions.md`.

### 4.3 Arithmetic

By the result-type table (section 6.1) [O-code, O-run]:

| Expression | Result |
|---|---|
| date ± number | date, number in days, fraction truncated (`Date(1;1;2001) + 1.5` = 1/2/2001, `+ 0.5` = 1/1/2001) |
| date − date | number of days |
| date ± time | timestamp (`Date(1;1;2001) - Time(1;0;0)` = 12/31/2000 11:00 PM) |
| date ± timestamp | time: timestamp seconds ± date-as-timestamp seconds |
| time ± number, time ± time | time, in seconds (`Time(1;0;0) - Time(2;0;0)` = -1:00:00, `Time(23;59;59) + 1` = 24:00:00) |
| time ± timestamp | timestamp |
| timestamp ± number | timestamp, in seconds |
| timestamp − timestamp, timestamp ± date | time (`Timestamp(1/1/2001 0:00) - Timestamp(1/1/2000 0:00)` = 8784:00:00) |
| any date/time type `*` `/` `^`, unary `-` | number (`Time(1;0;0) * 2` = 7200, `-Date(1;1;2001)` = -730486) |

### 4.4 Conversions [O-run]

| From \ to | number | date | time | timestamp | text |
|---|---|---|---|---|---|
| date | day number | itself | elapsed (day − 1) × 86400 s (`GetAsTime(Date(1;1;2001))` = 17531640:00:00) | midnight of that day | locale date format |
| time | seconds | `?` | itself | 1/1/0001 + time | elapsed format |
| timestamp | seconds since 0001 | its date | its time of day (12/24-hour per locale) | itself | locale timestamp format |
| number | itself | day number, truncated; < 1 or > 1460970 is 500 | seconds (`GetAsTime(100000)` = 27:46:40, `GetAsTime(-1)` = -0:00:01) | seconds since 0001 | section 2.7 |
| text | section 2.8 | section 4.6 | section 4.6 | section 4.6 | itself |

`GetAsDate`, `GetAsTime`, `GetAsTimestamp`, `GetAsNumber`, `GetAsText`, the
declared result type of a calculation (section 6.5) and the operators all use
these conversions.

### 4.5 Display formats [O-run]

Formats come from the locale stored in the file (section 9.3):

| Value | en-US | de-DE | fr-FR | sv-SE | ja-JP |
|---|---|---|---|---|---|
| `Date(1;2;2003)` | `1/2/2003` | `02.01.2003` | `02/01/2003` | `2003-01-02` | `2003/01/02` |
| `Timestamp(Date(1;2;2003);Time(13;5;7))` | `1/2/2003 1:05:07 PM` | `02.01.2003 13:05:07` | `02/01/2003 13:05:07` | `2003-01-02 13:05:07` | `2003/01/02 13:05:07` |
| `Time(13;5;7)` | `13:05:07` | `13:05:07` | `13:05:07` | `13:05:07` | `13:05:07` |
| 1.5 seconds (`GetAsTime(1.5)` en-US, ja-JP; `GetAsTime("1,5")` others) | `0:00:01.5` | `00:00:01,5` | `00:00:01,5` | `00:00:01,5` | `0:00:01.5` |

Rules seen in en-US [O-run]:

- Years always have four digits (`1/1/0099`); en-US months and days have no
  leading zeros; de-DE, fr-FR and sv-SE pad hours to two digits, ja-JP and
  en-US do not.
- A time from `Time()`, arithmetic or a number has the elapsed style:
  `h:mm:ss`, hours unpadded and unbounded, a leading `-` for negatives
  (`-0:00:01`), seconds always shown.
- A time of day (from a timestamp, or parsed from text with AM/PM) uses the
  locale's time-of-day format and omits zero seconds: `GetAsTime(Timestamp(...23:00))`
  = `11:00 PM`, timestamps `1/1/2001 1:00 AM` and `1/1/2001 12:00:01 AM`.
- Fractional seconds show as many digits as needed, up to six:
  `1:02:03.456`, `12:00:00.5 AM`.
- A date or time parsed from text keeps its separator and style for display
  (`DateTime::mSepChar`, `mTimeStyle`): `GetAsDate("01-02-2003")` displays
  `1-2-2003`, `GetAsTime("1:30:00 pm")` displays `1:30 PM`; after arithmetic
  the default style returns (`GetAsDate("1-2-2003") + 0` = `1/2/2003`,
  `GetAsTime("1:30 PM") + 0` = `13:30:00`).

### 4.6 Parsing text [O-run]

Dates follow the locale's field order (M/D/Y en-US, D.M.Y de-DE, D/M/Y
fr-FR, Y-M-D sv-SE, Y/M/D ja-JP). `/`, `.` and `-` were accepted as
separators wherever tried (`2.1.03` is 2002-01-03 in sv-SE and ja-JP); ISO `2003-01-02` is not a date in en-US or de-DE
(the first field is out of range for the order) but is in sv-SE and ja-JP.
Month names, weekday names and spaces as separators are rejected
(`Jan 2, 2003` is error 500). Leading/trailing spaces are allowed; other
characters around the date are not (`x1/2/2003y` is 500). A missing year is
the current year (`GetAsDate("1/2")` = 1/2/2026 on 2026-10-28). Out-of-range
fields are rejected, not normalized (`2/30/2003`, `13/1/2003` are 500). A
date followed by a time converts to the date part.

Years with one or two digits are windowed; three or four digits are literal
(`1/2/203` = 1/2/0203). On 2026-10-28, `00`-`56` mean 2000-2056 and `57`-`99`
mean 1957-1999, a window of [current year − 69, current year + 30] [O-run
for the observed bounds; I that the window moves with the clock].

Times: `h:mm`, `h:mm:ss`, `h:mm:ss.ffffff`; AM/PM (any case, with or without
space) in en-US, rejected in de-DE, fr-FR, sv-SE and ja-JP; minutes and
seconds over 59 carry (`1:75` = 2:15:00, `1:30:75` = 1:31:15); a leading `-`
negates; a bare integer is hours (`"90"` = 90:00:00, `"1"` = 1:00:00) but a
bare decimal is seconds (`"1.5"` = 0:00:01.5); more than three fields or
surrounding characters are error 501. Timestamps are a date, a space and a
time; ISO with `T` is rejected in en-US; an hour of 24 or more is 500.

## 5. Text

### 5.1 Characters and separators [O-run]

Text is UTF-16 (`Draco::unistring` derives from `std::u16string`;
`Draco::Text` = a `TextBuffer` plus style buffers) [O-code]. Text
functions count and index **grapheme clusters**: `Length("😀")` = 1,
`Length("e" & Char(769))` = 1, `Position("a😀b";"b";1;1)` = 3,
`Left("😀a";1)` = `😀`. `Code` reports UTF-16 code units
(`Code("😀")` = 5683255357, low surrogate first), and `Char` takes
concatenated five-digit codes. Case mapping is simple per character
(`Upper("straße")` = `STRAßE`).

`¶` is U+000D. `"a" & ¶ & "b"` is `a\rb`; `List` joins with U+000D.
Value lists also split on U+000A: `ValueCount("a" & Char(10) & "b")` = 2 and
CR LF counts as two separators (`ValueCount("a\r\nb")` = 3). A trailing
separator does not add a value (`ValueCount("a¶b¶")` = 2,
`ValueCount("a¶b¶¶")` = 3). `Char(0)` is dropped (`Length(Char(0))` = 0).
In formula source, a raw return inside a text literal becomes a space
(`Evaluate("\"a¶b\"")` = `a b`), `\"` is a quote and `\¶` a literal `¶`.

### 5.2 Styled text [O-run]

Text carries character style runs (font, size, color, style flags;
`Text::mCharacterStyleBuffer`). Observed through `GetAsCSS`:

- `&` concatenates runs; adjacent identical runs merge
  (`TextStyleAdd("a";Bold) & TextStyleAdd("b";Bold)` is one bold run).
  Some operations keep neighbouring runs of equal style apart (13,
  value-core follow-ups).
- `Left`, `Middle`, `Right`, `Upper`, `Substitute`, `Replace`, `Trim`,
  `GetValue`, `GetAsText`, `If`, `Let` and variables keep the styles of the
  characters they keep or copy; replacement text takes the style at the
  replaced position (`Replace(TextStyleAdd("abc";Bold);2;1;"X")` is all
  bold).
- `List` gives each separator the style of the value before it.
- A number with a style is styled text (`TextStyleAdd(1/3;Bold)`), and
  arithmetic drops styles (`TextStyleAdd(1/3;Bold) + 1` is plain).
- Comparison ignores styles (`TextStyleAdd("ab";Bold) = "ab"` is 1).

### 5.3 Comparison and collation [O-run, O-code]

Equality of text is `DBDataArg::IsEqual` → `Draco::EqualityComparator` →
`TextBuffer::IsMatchWithLocale` (`0x211ea0`): both texts are walked grapheme
by grapheme and each pair of graphemes must have equal collation keys
(`KeyBuilder::MakeKey`) [O-code]. So equality ignores what the key ignores
and never matches a grapheme against two:

| en-US file | Result | | Result |
|---|---|---|---|
| `"abc" = "ABC"` | 1 | `"é" = "e"` | 1 |
| `"e" & Char(769) = "é"` | 1 | `"Ａ" = "A"`, `"ア" = "ｱ"`, `"か" = "カ"`, `"①" = "1"` | 1 |
| `"ss" = "ß"` | 0 | `"æ" = "ae"`, `"ﬁ" = "fi"` | 0 |
| `"a" = "a "` | 0 | `"a" = "a" & Char(0)` | 1 |
| `"ı" = "i"` | 0 | `"ǅ" = "ǆ"` | 1 |

Ordering (`<`, `≤`, ...) is `TextBuffer::SortCompare` (`0x214d90`) with
the locale's `International` [O-code]: `"a" < "B"`, `"1" < "a"`,
`"-" < "a"`, `"a-b" < "ab"` (punctuation is not ignored), `"10" < "2"`,
`"a" < "a "`.

The language comes from the left operand's `DBData::mLocale` (`IsEqual`
passes it to the comparator) [O-code], which is the file's locale in the
harness. It changes results [O-run]:

| | en-US | de-DE | fr-FR | sv-SE | ja-JP |
|---|---|---|---|---|---|
| `"ä" = "a"` | 1 | 0 | 1 | 0 | 1 |
| `"z" < "ä"` | 0 | 0 | 0 | 1 | 0 |
| `"Ö" = "O"` | 1 | 0 | | | |

`Exact` compares code points (`Exact("é";"e" & Char(769))` = 0). The keys
are those of `om-collate`'s index keys (DUCET primary weights for English,
language tailorings otherwise; `query-program.md` 7.2) [I: same
`KeyBuilder`, to verify by comparing om-collate keys with these vectors];
per-grapheme matching is what makes `ß` ≠ `ss` even though DUCET expands
`ß` to `ss`. Which language a calculation field uses when the field's
"default language" differs from the file's is unknown (section 12).

## 6. Types and operators

### 6.1 Result types [O-code, O-run]

`+` and `-` take their result type from
`DBDataArg::GetReturnNativeType(DBDataType, DBDataType)::nativeTypeMatrix`
(`0xdf16c0`, table at `0x2e567f0`, 8 × 8 u32, types ≥ 7 count as number).
Rows are the left operand, columns the right:

| left \ right | text | number | date | time | timestamp | container |
|---|---|---|---|---|---|---|
| text | number | number | date | time | timestamp | number |
| number | number | number | date | time | timestamp | number |
| date | date | date | number | timestamp | time | number |
| time | time | time | timestamp | time | timestamp | number |
| timestamp | timestamp | timestamp | time | timestamp | time | number |
| container | number | number | number | number | number | number |

The operands are converted to the unit of the result (days for a date
result, seconds for time and timestamp, section 4.3) [O-run].

Other operators [O-code: each `DBDataArg` operator stores a number;
O-run]:

| Operator | Result |
|---|---|
| `*` `/` `^`, unary `-` | number |
| unary `+` | the operand unchanged [I] |
| `&` | text (`DBDataArg::Concat` `0xdcdfa0`), styles kept |
| `=` `≠` `<>` `<` `≤` `<=` `>` `≥` `>=` | number 0/1 |
| `and` `or` `xor` `not` | number 0/1 |

Comparison operators are left associative and yield 0/1, so
`1 < 2 < 3` = 1 and `2 = 2 = 2` = 0. `==` is a syntax error (1204).

Functions that pass a value through keep its type: `If`, `Case`, `Choose`,
`Let`, `Evaluate`, `Max`/`Min` (the chosen argument, `Max(Date(1;1;2001);5)`
is a date), `Abs` of a time; `Abs` of a date, `Int`, `Round`, `Sum`,
`Average` give numbers (`calc-functions.md`).

### 6.2 Comparison types [O-code, O-run]

`DBDataArg::IsEqual` (`0xdcc0f0`), `IsLessThan` (`0xdcc5b0`) and
`IsLessThanOrEql` pick the comparison type from
`getCompareNativeType(...)::compareTypeMatrix` (table at `0x2e56b00`):

| left \ right | text | number | date | time | timestamp | container |
|---|---|---|---|---|---|---|
| text | text | text | text | text | text | text |
| number | text | number | number | number | number | number |
| date | text | number | number | number | number | number |
| time | text | number | number | number | invalid | number |
| timestamp | text | number | number | invalid | number | number |
| container | text | number | number | number | number | container |

- **text**: both sides as text (section 5.3). A number compares as its
  display text: `10 < "9"` = 1, `1 = "1.0"` = 0, `1/3 = ".3333333333333333"`
  = 1; a date as its formatted text: `Date(1;2;2003) = "1/2/2003"` = 1 but
  `= "01/02/2003"` = 0. `"" = 0` is 0 and `"" < 0` is 1.
- **number**: both converted to numbers (date → day, time → seconds,
  timestamp → seconds since 0001), except that a date against a time or a
  timestamp is first converted to timestamp seconds: `Date(1;1;2001) =
  Timestamp(Date(1;1;2001);Time(0;0;0))` = 1, `Date(1;1;1) = Time(0;0;0)`
  = 1, `Date(1;1;2) = 86400` = 0.
- **invalid** (time against timestamp): every comparison, `≠` included,
  is 0.
- **container**: binary comparison of the data.

### 6.3 Conversion matrix

Section 4.4 for date and time types, 2.7 and 2.8 for number and text.
Containers convert to text as their reference text and to numbers as 0 or
by the text form [I; `containers.md`].

### 6.4 Empty operands [O-run]

`DBDataArg::Add`, `Subtract`, `Multiply`, `Divide`, `Power`, `Negate`
check `DBData::IsEmpty` on both operands first [O-code]:

- both operands empty: the result is empty (`"" + ""`, `"" - ""`,
  `GetAsNumber("") + GetAsNumber("")`, `-""`, `Sum("";"")`);
- one empty: it counts as 0 (`"" + 1` = 1, `"" * 2` = 0, `"" / 1` = 0,
  `1 / ""` is error 15, `"" + Date(1;1;2001)` = the date);
- single-argument numeric functions return empty for empty input
  (`Abs("")`, `Int("")`, `Round("";2)`, `Sign("")`), but text without
  digits gives 0 (`Abs("abc")` = 0);
- an empty *number* compares as 0 (`GetAsNumber(" ") = 0` = 1) while an
  empty *text* compares as text (`"" = 0` = 0);
- aggregates skip empty arguments (`Average("";2;4)` = 3, `Count("";2)` = 1).

Empty number *fields* follow the same rules [O-run, record context 12.1]:
with n and m empty number fields, `n = 0` = 1, `n = ""` = 1, `n < 1` = 1,
`n + 1` = 1, `n - 1` = -1, `n * 2` = 0, `n + m` = empty (type number),
`-n` and `Abs(n)` = empty, `n & "x"` = `x`, `If(n;…)` takes the else
branch, `not n` = 1, `Count(n)` = 0, `Sum(n;2)` = 2, `Average(n;2)` = 2.

### 6.5 Declared result type [O-run, I]

A calculation's result type converts the final value with
`DBData::ConvertData(DBDataType)` (`0xe9b6d0`). The stored-calculation path
does: `DBDependencyList::EvalOneRep` (`0xec3890`) calls
`DBCalcFormula::Solve`, then `DBField::GetFieldDefinition` and
`ConvertData` with the field's result type, then stores [O-code];
`DBCalcFormula::Solve` itself converts nothing. In the harness:

| Value | number | date | time | timestamp | text |
|---|---|---|---|---|---|
| `Date(1;1;2001)` | 730486 | 1/1/2001 | 17531640:00:00 | 1/1/2001 12:00 AM | 1/1/2001 |
| `Time(1;0;0)` | 3600 | `?` | 1:00:00 | 1/1/0001 1:00:00 | |
| `Timestamp(1/1/2001 1:00)` | 63113907600 | 1/1/2001 | 1:00 AM | itself | |
| `1/3` | itself | `?` | 0:00:00.333333 | | .3333333333333333 |
| `1.5` | | | 0:00:01.5 | 1/1/0001 12:00:01.5 AM | |
| `"12abc"` | text kept: `12abc`, value 12 | | | | |
| `"1/2/2003"` | | 1/2/2003 | | | |
| `"2003-01-02"`, `"730486"` | | `?` | | | |
| `"90"` | | | 90:00:00 | | |

A number result keeps the original text (a number field may hold `12abc`
whose value is 12). Since the stored path converts with the same
`ConvertData`, a stored number calculation whose formula gives `"12abc"`
holds text `12abc` with value 12 [O-code for the call sequence, O-run for
`ConvertData`; I that the record write keeps the text form, section 12].

## 7. Errors

### 7.1 Codes [O-run]

`DBExpressionEnv::Evaluate` returns the `Error::Code` of the evaluation, and
`EvaluationError(expr)` returns the same number (catalog
`error-codes.csv`):

| Code | Cause (examples) |
|---|---|
| 14 Out of range | `Sqrt(-1)`, `Ln(0)`, `Log(-10)`, `(-8)^(1/3)`, `10^405`, `Factorial(300)`, `Factorial(-1)`; a repetition index below 1 or above the field's repetitions (`f[0]`, `f[10]` of a 9-repetition field, `Rel::f[2]` of a non-repeating one); `GetNthRecord` past the set (12.1) |
| 15 Can't divide by zero | `1/0`, `Div(1;0)`, `0^-1`, `1/""`, `0.1^-404` |
| 7 Running out of memory | a `While` over its iteration limit, or more custom function calls than the limit (8.4, 8.5) |
| 102 Field is missing | `Contacts::NoSuchField` |
| 106 Table is missing | an unknown name with no table context (`NoSuchField`, `Let(x=1;Evaluate("x"))`, `1.5` in a de-DE file) |
| 500 Date value ... | dates/timestamps out of range or unparsable |
| 501 Time value ... | unparsable time text |
| 1202 Too many parameters | `Abs(1;2)` |
| 1204 Number, text constant, field name or "(" expected | `1+`, `Abs()`, `1 == 1`, `Let(1x=1;1x)` |
| 1205 Comment not terminated | `/* x` |
| 1206 Text constant must end with a quotation mark | `"abc` |
| 1207 Unbalanced parenthesis | `(1` |
| 1208 Operator missing, function not found | `Foo(1)` |
| 1211 List usage not allowed | `Let(["x"=1];1)` |
| 1212 An operator is expected | `1)`, `1;2`, `1 2`, `1,5` in en-US |
| 1214 A field is required | `GetRepetition(5;1)`, `List("")` |
| 1215 Invalid Get parameter | `Get(Foo)` |
| 1218 Cannot evaluate the number | `5.`, `1e800`, `Asin(2)`, `Acos(2)` |
| 1225 Not supported in this context | `Self` outside a field |

Not errors: `Mod(x;0)` (= x), `Date(2;30;2020)` (normalized),
`GetValue("a";0)`, `Middle("abc";0;1)`, `Round(1;1000)`,
`SetPrecision(1/3;1000)` (clamped), `Char(-1)`, `JSONGetElement("{";"a")`
(JSON functions report problems in their result text, `calc-functions.md`),
`Let([x=1;x=2];x)`.

### 7.2 Propagation [O-run]

- The first error of an evaluation is kept and reported even when the
  result does not depend on it: `Let(x=1/0;"ok")` returns `ok` with error
  15. Branches not evaluated raise nothing (`If(1;1;1/0)` has no error).
- An error value is `?`. As text it is the character `?`
  (`1/0 & "x"` = `?x`, `Length(1/0)` = 1, `IsEmpty(1/0)` = 0,
  `List(1/0;2)` = `?\r2`); as a number it is NaN, and NaN arithmetic is
  `?` (`(1/0) + 1`, `Abs(1/0)`, `"?" + 1` with no error, `GetAsNumber("?")`).
  NaN is truthy (2.9), equal to itself (`(1/0) = (1/0)` = 1,
  `"?" = 1/0` = 1) and not less than anything (`(1/0) < 1` = 0).
  `Choose(1/0;...)` is `?`.
- `EvaluationError(expr)` reports the first error inside `expr` and
  returns normally; `IsValid(1/0)` = 0 without error;
  `IsValidExpression(text)` = 0 for syntax errors.
- A parse error inside `Evaluate(text)` is that call's error; the result is
  `?` (`Evaluate("1+") & "x"` = `?x`, error 1204).
- Error 7 (iteration or custom function call limit) aborts the whole
  evaluation: the result is `?` and not even `EvaluationError` around it
  returns.
- `Get(LastError)` belongs to the script runtime (`script-runtime.md`); in
  the harness it is empty.

## 8. Evaluation rules

### 8.1 Order and short-circuit [O-run]

Operands and the arguments of fixed-arity functions are evaluated left to
right (observed through `Let($a=...)` side effects in `+`, `&` and
function arguments); functions stored with a count (`Max`, `List`,
`Substitute`, custom functions, ...) evaluate theirs last first (section
13, evaluator answers).
`and` skips its right side when the left is false, `or` when the left is
true; `xor` evaluates both. `If` evaluates the condition and one branch;
`Case` evaluates conditions in order up to the first true one, then its
result, and the default only when no condition holds; `Choose` evaluates
the index and the chosen value only. `Case` and `If` without a default and
no match return empty text; `Choose` with an index out of range returns
empty; the index is truncated (`Choose(1.9;"a";"b";"c")` = `b`) and empty
counts as 0. [`SolveAnd` `0xe44cf0`, `SolveOr` `0xe44f10`, `SolveIf`
`0xe4a9f0` O-code]

### 8.2 `Let` [O-run]

Bindings are evaluated eagerly and in order, even when unused, and each sees
the previous ones (`Let([x=1;y=x+1];y)` = 2). Rebinding a name in the same
list replaces it (`Let([x=1;x=2];x)` = 2, no error 1213). An inner `Let`
shadows an outer one for its body only (`Let(x=1;Let(x=2;x) + x)` = 3).
Names are case-insensitive, may be function names or keywords
(`Let(Abs=1;Abs)`, `Let(and=1;and)`, `Let(Pi=1;Pi)` work), may contain
`.` and non-ASCII letters, may not start with a digit (`1x` is 1204), and
`${x y}` quotes a name with spaces. `Let` names are not visible inside
`Evaluate` text.

### 8.3 Variables [O-run, O-code]

`$name` (script-local) and `$$name` (file-global) live in `DBVarMap`s that
the host returns from `DBFileUser::GetScopedVars()` and `GetGlobalVars()`
(section 9.2); a map is name → `DBData*` (`DBVarMap::SetVariableValue`
`0xf52ee0`, `GetVariableValue` `0xf40a20`).

- Names are case-insensitive (`$Case1` = `$case1`).
- An unset variable is empty text.
- `Let([$x=1];...)` assigns the variable in the host's map; it outlives the
  `Let` (`Let([$a=1];"") & $a` = 1). Outside a running script `$` variables
  belong to script level 0 and persist between evaluations (they persisted
  from line to line in the harness) [O-run; script levels are
  `script-runtime.md`'s].
- Repetitions: `$x[n]` with n evaluated, converted with the lenient
  text-to-number rule and truncated (`$x[1.5]` is `$x[1]`, `$x["3abc"]` is
  `$x[3]`, `$x[""]` is `$x[1]`); `$x` is `$x[1]`; `$x[0]` and negative
  repetitions are separate values (`Let([$r[-1]=5];$r[-1])` = 5); large
  indexes are allowed (10^9); an index that is `?` is error 15 and assigns
  nothing. The repetition is formatted into the map key
  (`SolveSetVariable` `0xe45b40` calls `FixPt::Truncate` and
  `GetFormattedText`) [O-code]. Only `$` and `$$` variables take `[n]`
  (`Let(x[2]=5;x)` is 1204).
- Variables store the whole `DBData`: type, styles, precision
  (`Let([$x=SetPrecision(1/3;30)];$x)` keeps 30 digits).
- `$` and `$$` are visible inside `Evaluate`.

### 8.4 `While` and `SetRecursion` [O-run, O-code]

`While([init]; condition; [logic]; result)` evaluates the init bindings,
then repeats: evaluate the condition, stop if false, run the logic bindings
in order. The default limit is 50,000 iterations: 50,000 iterations
succeed, 50,001 fail with error 7 (`DBPolishEnv::DBPolishEnv` `0xe38620`
and `Clear` initialize 50,000) [O-code]. `SetRecursion(expr; n)` sets the
limit for loops evaluated inside `expr` to n (n iterations allowed, n + 1
fail); n of 0, negative or empty allows none; values up to at least 10^9
are accepted; the innermost `SetRecursion` wins
(`DBPolishEnv::mIterationStk`). The limit is per `While`: 300 iterations of
an inner loop inside each of 300 outer ones succeed. A condition that is `?`
is true, so `While([i=0];1/0;...)` runs into the limit. Variables bound in
logic are visible in the result (`While([i=0];i<3;[x=i;i=i+1];x)` = 2), the
result may rebind them, and a name never bound is an unknown field (106 in
the harness). `SetRecursion` also sets the custom function call limit
(8.5); the two counts are separate.

### 8.5 Custom functions [O-run, O-code]

`DBPolishEnv::EnterFunction` (`0xe39870`) pushes a binding frame and counts
calls in `mEnterFunctionCount`; `DBCalcSolver::SolveCustomFunction` and
`InitForCustomFunctionSolving` run the function body [O-code]. Observed
with the recursive custom functions of an MBS example file (12.1):

- The limit counts custom function **calls per evaluation**, not nesting
  depth, and tail calls count like other calls. By default 49,999 calls
  succeed and the 50,000th fails with error 7, which aborts the whole
  calculation like the `While` limit (`EvaluationError` around it does not
  return). A non-tail recursion 49,999 deep works; six calls of a function
  that recurses 9,000 deep inside one `While` (54,000 calls) fail. Claris
  documents 10,000 for non-tail recursion; this engine has no such depth
  limit (FileMaker Pro's thread stack may still impose one [I]).
- `SetRecursion ( expr ; n )` allows n calls inside `expr` (n + 1 fail; 0
  allows none), up to at least 60,000, for tail and non-tail recursion.
- The call count and the `While` iteration count are separate (40,000 loop
  iterations plus 40,000 calls succeed in one formula); `Evaluate` text
  starts its own count (30,000 calls outside plus 30,000 inside succeed).
- Calls compile as in calc-encoding.md 5.2; names match case-insensitively.

### 8.6 Repetitions of fields [O-run, O-code]

`DBCalcSolver::SolveField` (`0xe44660`) reads a field through
`DBFieldIterator::Reset`/`GetNextRepetition`, consults
`DBField::GetMaxRepetitions`, and selects the record with
`DBCursor::Choose`; `DBExpressionEnv::mRepetition` (+0x20) and
`mAutoExtendRepetition` (+0x22) carry the repetition being calculated
[O-code]. Observed in record context (12.1), with r a repeating field:

- Outside a repetition (a non-repeating calculation, `mRepetition` 0) `r`
  reads repetition 1.
- In repetition n of a repeating calculation `r` reads repetition n, and
  empty when n is beyond r's repetitions (no error); a **non-repeating**
  field reads empty in repetitions above 1 unless wrapped in `Extend`
  (`Extend(f)` gives its value in every repetition). `Extend(r)` of a
  repeating field gives repetition 1. A non-repeating calculation field
  referenced from repetition 2 is likewise empty.
- `r[n]` and `GetRepetition(r;n)` read repetition n in any context; n is
  truncated (`r[1.9]` is repetition 1); n below 1 or above the field's
  repetitions is error 14 (`?`).
- `Get(CalculationRepetitionNumber)` is the repetition being evaluated: 0 in
  a bare `Evaluate`, i while repetition i of a calculation field is computed.
- `Last(r)` is the last non-empty repetition.
- An aggregate whose **only** argument is r walks every non-empty repetition
  (`List`, `Count`, `Sum`, `Average`, `Max`); an index there is ignored
  (`List(r[3])` lists all repetitions). With **two or more** arguments each
  field contributes its current repetition only (`Sum(r;5)` = repetition 1
  + 5, `List(r;s)` = repetition 1 of each).
- Repeating calculations aggregate the same way: `Count`, `List` and
  `Last` of a 9-repetition calculation with 3 non-empty repetitions see
  those 3 (whether its empty repetitions come from the "do not evaluate if
  all referenced fields are empty" option or from storage was not checked).

### 8.7 Related fields and aggregates [O-run, O-code]

Code: `DBFieldRefArg::Lookup` (`0xdc0000`), `DBCalcSolver::SolveLookups`
and `SolveAggregates`, `DBDataArg::Sum` (`0xdff2b0`), `DBRelatedSet`,
`DBFileUser::GetLayoutPortalRelatedSet` for portal contexts [O-code].
Observed in record context (12.1) through an unsorted self-join with 12
related records and a relationship with none:

- `Rel::f` is the first related record's value; without a relationship sort
  the related set is in creation (record ID) order, in both directions of
  the relationship. Sorted relationships were not available [D: the sort
  order].
- An aggregate whose only argument is `Rel::f` walks every related record
  (`Sum` 78, `Count` 12, `Average` 6.5, `Min`, `Max`, `StDev`, `List` in
  related-set order). With two or more arguments each related field gives
  its first related record only: `Sum(Rel::x;100)` = first + 100,
  `Sum(Rel::x;x)` = first + the current record's x.
- `Last(Rel::f)` is the last related record; `GetNthRecord(Rel::f;n)` the
  nth; n outside 1..count is error 14. `Rel::f[2]` of a non-repeating field
  is error 14.
- With no related records: `Rel::f` is empty (`Rel::n + 1` = 1), `Count` is
  0, `Sum`, `Average`, `Max`, `List`, `Last` are empty (not 0;
  `Sum(Rel::n) + 1` = 1), `GetNthRecord(Rel::f;1)` is error 14.
- `GetNthRecord(f;n)` of a field of the context table reads record n of the
  current found set; `Get(RecordNumber)` is the record's position there and
  `Get(FoundCount)` its size (on a record cursor these are the table's
  records in creation order).

### 8.8 Context, stored and unstored calculations [O-code, I]

Every calculation is solved relative to a table occurrence
(`DBExpressionEnv::mTableContext`, +0x08): field names resolve through that
occurrence's relationships, and its current record is the parent cursor
(`mParentCursor`, +0x10; `mChildCursor` +0x18 for portal rows). The
formula's own context is stored with it (`auto-entry.md`: child 4 of the
calculation subtree). Stored calculations are recomputed by
`DBDependencyList` (`EvalOneRep`, `AutoEnterData`) when a field they depend
on changes in the same record; unstored calculations, and stored ones that
`DBCalcFormula::CalcMustBeUnstored` rejects (related fields, globals,
summaries, unstored calcs, some `Get` functions), are evaluated when read.
Details belong to `auto-entry.md` and a future dependency spec [I].
Unstored calculation fields evaluate when referenced in record context
(the oracle file's `Output`, the server sample's `Time Frame`) [O-run]. In
the harness a field reference with a table occurrence but no record cursor
is error 1224 and, with no table occurrence, 103 or 106; these are harness
states FileMaker does not produce for a field calculation [O-run, I].

### 8.9 `Evaluate` [O-run]

`Evaluate(text)` compiles `text` with the current file's function names and
locale, then solves it in the caller's context (same table, record,
repetition, precision and variables; not `Let` names). The decimal separator
of number literals in the text is the file locale's: in de-DE, fr-FR and
sv-SE `Evaluate("1,5")` = 1.5 and `Evaluate("1.5")` is error 106 (the
harness parses whole formulas the same way, so `List(1,5;2)` is `1,5\r2`
there and `1\r5\r2` in en-US) [O-run; whether FileMaker Pro's formula
editor also accepts `,` in such files is for the oracle]. `Evaluate` of an
empty or blank text is empty; a constant in place of the optional trigger
field list is a syntax error (`Evaluate("1+1";[1])` is 1204). Comments (`//`, `/* */`) are
allowed. A number argument is converted to text first (`Evaluate(1+1)` =
2). Nested `Evaluate` works.

## 9. The host boundary

### 9.1 `DBExpressionEnv` [O-code]

The solver's context object (DWARF, 0x60 bytes): `mFileUser` (+0x00), the
`DBFileUser` that owns the formula and answers environment questions;
`mTableContext` (+0x08), the table occurrence key (`HBSerialKey<5>`, a
length byte plus up to five key bytes); `mParentCursor` (+0x10) and
`mChildCursor` (+0x18), the current record and portal row; `mRepetition`
(+0x20) and `mAutoExtendRepetition` (+0x22); `mSelfEvaluator` (+0x28,
`DBSelfEvaluator`: the value, field spec and field that `Self` refers to);
`mLOC_LayoutKey`/`mLOC_ObjectDataKey` (+0x50, +0x58), the layout object a
calculation runs for (tooltips, hide conditions, conditional formatting).

### 9.2 What the evaluator asks the host [O-code]

The host is a `DBFileUser` subclass: FileMaker Pro's and FileMaker Server's
FMEngine override it as `Draco::FMFileUser` (`libFMEngine.so`), and a hosted
file on the server uses `NetHostInternalFileUser`. Virtual methods
(`vtable for Draco::DBFileUser` at `0x3548d00`) the calculation code calls:

| Slot | Method | Used for |
|---|---|---|
| 11 | `GetGlobalVars() const` → `DBVarMap&` | `$$` variables |
| 12 | `GetScopedVars() const` → `DBVarMap&` | `$` variables of the running script level |
| 13 | `GetValue(u16 const& op, DBCursor const*, short rep, DBData&) const` | every `Get()` selector (op = selector + 1000, `SolveGetFunction` `0xe451d0`) and the environment values auto-entry asks for (`kDBCurrentDateOp`, `kDBUserNameOp`, ...) |
| 15 | `GetLayoutObjectAttribute(cursor, name, attribute, ..., DBData&)` | `GetLayoutObjectAttribute` |
| 16 | `GetAVPlayerAttribute` | `GetAVPlayerAttribute` |
| 17 | `GetSensor(cursor, id, ..., DBData&)` | `GetSensor` |
| 23, 24, 25 | `GetLayoutObjectOwnerInfo`, `GetAddonInfo`, `GetLayoutObjectUUID` | design and layout functions |
| 30 | `GetLayoutPortalRelatedSet(cursor, name, shared_ptr<DBRelatedSet>&, bool&)` | portal-filtered related sets |
| 35, 36 | `ExecuteLocalDAPI`, `GenerateCurrentLayoutInfo` | `Execute FileMaker Data API`, AI layout context |
| 40 | `OnDataRequest(cursor, field, rep, DBData&)` | values the host supplies for a field |
| 50 | `GetSystemLocale(Locale&)` | system formats |
| 5, 8, 9 | `GetPrivileges`, `GetUserAccountKey`, `GetAuthenticationInfo` | privilege checks, `Get(AccountName)` |

Fields, records and related sets come through the cursor and catalog
classes rather than through these virtuals: `DBCursor::Choose`,
`DBFieldIterator`, `DBTableMgr::GetProxy`, `DBTableProxy::GetFieldCatalog`,
`DBRelatedSet`. The locale for conversions travels in each `DBData`
(`mLocale`) and comes from `DBFileUser::GetLocale` [O-code].

### 9.3 Locales [O-run, O-code]

Every file stores its formats and language (the Contacts templates carry
en-US, de-DE, fr-FR, sv-SE, ja-JP); the record `[2, 0d]`, its mapping to
`Get ( FileLocaleElements )` and to each behaviour below are in
[file-metadata.md](file-metadata.md) section 2. The server always uses them; FileMaker
Pro can be set to use the system's formats instead, which is why the host
exposes `GetSystemLocale` [D, I]. Everything locale-dependent in this
document (decimal separator, date and time formats and parsing, AM/PM,
collation, day and month names, literal parsing in `Evaluate`) follows the
file's locale in the harness.

### 9.4 Proposed Rust boundary (for `om-calc`)

A trait shaped after the code above, so the evaluator stays independent of
the protocol and GUI crates:

```rust
pub trait CalcHost {
    /// Locale of the file (formats, separators, collation language).
    fn file_locale(&self) -> &Locale;
    /// Locale when the user chose system formats (FileMaker Pro only).
    fn system_locale(&self) -> Option<&Locale>;
    /// Get() selectors and auto-entry values; `selector` as in get-selectors.csv.
    fn get_value(&self, selector: u16, record: Option<&RecordRef>, repetition: i16) -> Result<Value, CalcError>;
    /// $$ and $ variables, keyed by case-folded name and repetition.
    fn global_vars(&mut self) -> &mut VarMap;
    fn local_vars(&mut self) -> &mut VarMap;
    /// Field data through the context's table occurrence.
    fn field_value(&self, ctx: &Context, field: FieldRef, repetition: u32) -> Result<Value, CalcError>;
    fn related_set(&self, ctx: &Context, occurrence: TableOccurrenceId) -> Result<RecordSet, CalcError>;
    /// Layout object, sensor, AV player, add-on, Data API hooks (optional).
    fn layout_object_attribute(&self, ctx: &Context, name: &str, attribute: &str) -> Result<Value, CalcError>;
}
```

`Context` carries the table occurrence, current record, portal row,
repetition and auto-extend flag, `Self`, and the layout object, mirroring
`DBExpressionEnv`.

## 10. Implementation checklist

1. Decimal numbers with display precision p, two guard digits, words of
   10^9, the operation rules of 2.3 (exact add/subtract, short-product
   multiply truncated at p + 2, divide rounded at p + 2), the comparison of
   2.5 and the formatting of 2.7. Transcendental functions as in 2.4, with
   the double path for trigonometry formatted with `%.20g`.
2. The lenient text-to-number parser of 2.8 with locale separators.
3. Dates as day numbers from 1/1/0001, times and timestamps as seconds with
   microseconds truncated, the arithmetic table of 6.1, display styles and
   separators that survive parsing (4.5).
4. Text as UTF-16 with grapheme-based functions, CR as `¶`, style runs, and
   per-grapheme collation-key equality (5.3).
5. The comparison-type table of 6.2 and the empty rules of 6.4.
6. Sticky first error, `?` as text `?`/numeric NaN, error codes of 7.1.
7. Left-to-right evaluation with the short-circuits of 8.1, eager `Let`,
   variables with repetitions in host maps, `While` limits.

## 11. Test vectors

`catalogs/calc-value-vectors.tsv`: columns `expression`, `expected_result`
(catalog escapes), `expected_error` (the evaluation's error code, as
`EvaluationError` would report it; 7 aborts), `evidence`. Expressions use
the harness escapes (`\r` CR, `\n` LF, `\t`, `\\` one backslash, `\uXXXX`).
All 2,196 rows are `established:run`:

- 1,976 evaluated without a table occurrence, with the file locale, the
  result's type, the battery, and flags (`declared-result=<type>` for rows
  converted to a calculation result type, `clock=2026-10-28` for rows that
  depend on today's date, `env=fms-harness-session` for two rows that read
  the harness session);
- 203 evaluated in record context (batteries `x01`..`x04`) with `file=`,
  `context=` (stored table occurrence key), `record=` (record ID) and `rep=`
  (repetition being calculated); the field names are those of the sample
  files described in each battery's header;
- 17 rules for an abstract schema (`schema:` names the fields to create),
  formerly inferred, settled by those runs
  (`tools/fms-calc-harness/schema-vectors.tsv`).

Regenerate with `tools/fms-calc-harness` (README there). Rows were left out
when they depended on a variable set by an earlier line or on the harness's
file name, account or missing table.

For the FileMaker Pro oracle: evaluate each expression in a file whose
formats match the `file=` locale (or use `Evaluate` in a calculation with
"use file's formats"), compare result text and `EvaluationError`, and for
`declared-result` rows put the expression in a calculation field of that
result type.

## 12. Unknowns and experiments

| # | Question | Status |
|---|---|---|
| U1 | Does FileMaker Pro (client app type, "use system formats" off) give the same results as the server engine? | Open. Run the en-US vectors in FileMaker Pro; diff. |
| U2 | Two-digit year window: does it move with the clock? | Open. Oracle (or harness under a faked clock): `GetAsDate("1/2/56")` on dates in 2027 and 2030. |
| U3 | Which collation a calculation field uses when its "default language" (field indexing language, `Unicode`) differs from the file's | Open. Calc fields with result `"ä" = "a"` and `"z" < "ä"` set to English, German, Swedish, Unicode in one file. |
| U4 | Does a stored number calculation store `12abc` or `12`? And date text styles (`1-2-2003`)? | Partly settled (6.5): the stored path applies `ConvertData`, which keeps `12abc` with value 12 [O-code, O-run]; that the record write keeps the text form is [I]. Confirm by reading back a stored calc field in FileMaker Pro. |
| U5 | Repetitions of fields, `Extend`, `Last`, `Get(CalculationRepetitionNumber)` | Settled (8.6, 12.1). |
| U6 | Related field first-record order and aggregates over repetitions | Settled for unsorted relationships (8.7, 12.1); sorted relationships open (no sample file has one with data). |
| U7 | Custom function recursion limits and `SetRecursion`'s effect on them | Settled (8.5, 12.1): 50,000 calls per evaluation, not a depth limit. |
| U8 | Exact short-product rule for longer operands (R ≥ 3) and `Divide` rounding for p > 16 | Settled (14.2): rows over the right operand cut at word R; division by divisors of up to nine significant digits is correctly rounded, wider ones go through Newton division. |
| U9 | Digits of `Exp`, `Ln`, `Sqrt`, `^` at each precision | Settled (14): algorithms specified, model `tools/fixpt_model.py` matches 7,504 of 7,504 engine results at p = 16 to 400. |
| U10 | `Get(LastError)` after a calculation error inside a script step | Open. Script runtime oracle. |
| U11 | `Evaluate` literal decimal separator in FileMaker Pro's formula editor for comma locales | Engine side settled: compiling `1,5` in a de-DE file gives 1.5 and `1.5` is error 106 (calc-encoding.md 10.1); whether FileMaker Pro's dialog shows and accepts the same is for the oracle. |
| U12 | Where the 1,000+ selector offset and `GetValue` opcodes of non-`Get` environment values are defined | Open. Static: `FMFileUser::GetValue` in libFMEngine. |
| U13 | `Self` | Open. It compiles only with `kSupportSelf` (calc-encoding.md 10.1), which `Evaluate` never sets; needs a field, validation or layout object context. |
| U14 | Summary field values and `GetSummary` | Settled: every kind and option with the script engine on a probe file ([file-metadata.md](file-metadata.md) 5.3); in ExecuteSQL ([execute-sql.md](execute-sql.md) 12, S3). |
| U15 | How large a text value a calculation may build, how deep `Evaluate` nests, and the error past either | Open. Claris documents 2 GB for a text field; neither limit was measured in a calculation. Oracle: `Length ( Substitute ( $p ; "a" ; $p ) )` with `$p` of 50,000 `a` (2.5 billion characters), and `Let ( $x = "Evaluate ( $x )" ; Evaluate ( $x ) )`, each wrapped in `EvaluationError`. |

### 12.1 Engine answers (record context)

The harness evaluates in a table occurrence on one record with
`DBRecordCursor(DBFileUser const&, HBSerialKey<5> const& occurrence,
HBSerialKey<9> const& record)` (`0xe7fab0`) and `DBExpressionEnv::Setup`
with that cursor; `DBExpressionEnv::mRepetition` is set directly for the
repetition being calculated (README in `tools/fms-calc-harness`, `@` lines).
It works on a private copy of the file and writes nothing. Creating fields,
records or custom functions through the engine was not attempted (catalog
member creation and record transactions are large undocumented APIs), so
the runs use sample files that already have the needed schema and data:

| battery | file | what it has |
|---|---|---|
| `x01-repetitions` | MBS example AppleTouchBar | text fields with 9 and 12 repetitions holding data, a 9-repetition calculation using `Get(CalculationRepetitionNumber)`, `GetRepetition` and an unindexed repeating field, a non-repeating `List(Extend(…))` of it; 4 records |
| `x02-numbers` | MBS example Phidget Digital Outputs | a number field with 8 repetitions (all 1), empty number fields; 1 record |
| `x03-related` | FileMaker Server sample (en) | Tasks (12 records) with an unsorted self-join to all 12, and relationships to two empty tables |
| `x04-recursion` | MBS example FileMaker_Snippet_Storage | custom functions with non-tail recursion (`__GEN_Range_ID`, depth = range length) and tail recursion (`__GEN_RandomPW`, depth = length) |

Every result is [O-run]; the rows are in calc-value-vectors.tsv (section 11)
and the raw results in `tools/fms-calc-harness/results/`. Summary:

- **Repetitions (U5).** Outside a repetition an unindexed repeating field
  reads repetition 1 (`ATB::flex_Label` = repetition 1, `Connected + 1` =
  2); in repetition 2 it reads repetition 2, a non-repeating field reads
  empty unless in `Extend` (`ATB::_Table` = empty, `Extend ( ATB::_Table )`
  = its value), and `Extend` of a repeating field gives repetition 1.
  `field[n]` reads repetition n everywhere; `[1.9]` is repetition 1; `[0]`,
  `[-1]` and `[10]` of a 9-repetition field are error 14. A repetition
  beyond the field's count read through the calculation repetition is
  empty without error. `Last` = last non-empty repetition. The stored
  repeating calculation evaluates repetition i with
  `Get(CalculationRepetitionNumber)` = i; a bare `Evaluate` gives 0.
- **Aggregates over repetitions.** One argument: every non-empty repetition
  (`Count ( ATB::flex_Label )` = 2, `Sum ( Connected )` = 8, `Count (
  ATB::_default )` = 5 of 12), even when indexed (`List ( ATB::_default[3]
  )` lists all 5). Two or more arguments: the current repetition of each
  (`Sum ( Connected ; 5 )` = 6, `List ( flex_Label ; flex_Script )` =
  repetition 1 of each).
- **Empty number fields.** As 6.4: `n = 0` → 1, `n + 1` → 1, `n + m` →
  empty.
- **Related records (U6).** First related record in creation order for an
  unsorted relationship, both directions; one-argument aggregates walk all
  related records (`Sum` = 78 over 12); two or more arguments use the first
  related record (`Sum ( Rel::x ; 100 )` = 107); `Last`, `GetNthRecord` as
  8.7; empty related set: `Count` 0, other aggregates empty, `GetNthRecord`
  error 14.
- **Found set.** On a record cursor `Get ( FoundCount )` is the table's
  record count, `Get ( RecordNumber )` the record's position in creation
  order (record ID 30 is 4 of 12), and `GetNthRecord ( f ; n )` reads
  record n.
- **Custom functions (U7).** 49,999 calls per evaluation succeed and the
  50,000th is error 7 (aborts), tail or not; recursion 49,999 deep works;
  `SetRecursion ( expr ; n )` allows exactly n calls (n + 1 fail, 0 none);
  `While` iterations and custom function calls are counted separately;
  `Evaluate` text counts on its own.
- **Literals at evaluation** (battery `16-literals`, no context): a CR typed
  inside a literal evaluates as a space (`Length ( "ab<CR>c" )` = 4,
  character code 32), also inside `Evaluate` text; LF stays LF; `"a\¶b"` is
  `a¶b`; `""""` is `"`.

## 13. Oracle answers (implementation)

Questions the `om-calc` value core met that sections 1-12 left open, settled
with `tools/fms-calc-harness` (FileMaker Server 26.0.3.309, 2026-10-28, the
Contacts template of the locale named, en-US unless noted) [O-run]. `→`
gives the result text, then the error code when not 0.

**Locales** (`Get(FileLocaleElements)`; `om_calc::Locale` constructors):

| | en-US | de-DE | fr-FR | sv-SE | ja-JP |
|---|---|---|---|---|---|
| `IDNum` | 21 | 25 | 24 | 41 | 31 |
| decimal / thousands | `.` `,` | `,` `.` | `,` space | `,` space | `.` `,` |
| date order, separator | MDY `/` | DMY `.` | DMY `/` | YMD `-` | YMD `/` |
| pad day and month (`0d`, `0m`) | no | yes | yes | yes | yes |
| 12-hour clock / pad hours (`0h`) | yes / no | no / yes | no / yes | no / yes | no / no |
| `amStr` / `pmStr` | ` AM` / ` PM` | none | none | none | ` 午前` / ` 午後` |
| first day of week, list separator | 1 `,` | 2 `;` | 2 `;` | 2 `;` | 1 `,` |

**Number precision**

- `+`, `-`, `*` run at max(evaluation precision, both operands'); `/` at
  max(evaluation precision, the dividend's): `1/3 + 0.0000000000000000000` →
  `.333333333333333333` (p 19), `1/3 * 1.0000000000000000000` → the same,
  `(1/3) / 1.0000000000000000000` → `.3333333333333333`,
  `1.0000000000000000000 / 3` and `(1.0000000000000000000 + 0) / 3` →
  `.3333333333333333333`; `Abs`, `Round`, `Int`, `Mod`, `Div`, `Floor`,
  unary minus and `GetAsNumber` keep the operand's precision (all p 19 for
  that literal).
- `SetPrecision` sets the evaluation precision of what is evaluated inside
  and rounds the result; it does not raise a value's own precision:
  `SetPrecision(0.5;20)/3` → `.1666666666666667`, `SetPrecision(1/3;20)/1`
  keeps 20 digits, `SetPrecision(0.333333333333333333*0.333333333333333333;30)`
  → `.111111111111111110888888888889` (the product runs at 30).
- `(10^400)*(10^400)*(10^400)` → `1.0E+1200`, times 10 → `?` (`IsValid`
  1). The limit is on the operands of `*` and `/`, not on results (14.2
  and the implementation notes of section 14: `9*10^1200 + 9*10^1200` →
  `1.8E+1201`).
- The literal limit is on the written exponent: `10e799` → `1.0E+800`,
  `0.1e-799` → `1.0E-800`, `1e0799` → `1.0E+799`; `0.5e800` and `100e-801`
  → 1218. Text: `GetAsNumber("0.5e800")` → `?`, `GetAsNumber("10e799")` →
  `1.0E+800`.
- Held digits: `Sqrt(2)*10^18` and `(2^0.5)*10^18` → `1414213562373095049`,
  `Pi*10^18` → `3141592653589793238`, `(1/3)^2*10^18` →
  `111111111111111111`, `1.1^30*10^18` → `17449402268886407319` (p + 2,
  rounded); but `Exp(1)*10^18` → `2718281828459045235.36`, `Exp(-1)*10^18`
  → `367879441171442321.6`, `Ln(2)*10^18` → `693147180559945309.4`,
  `Log(2)*10^18` → `301029995663981195.214` (19 to 21 digits).
- `Int`, `Floor` and `Ceiling` round to the display precision first:
  `Int((1/3)*3)` and `Floor((1/3)*3)` → 1; `Int(0.99999999999999999)` → 0
  and `Ceiling(0.00000000000000000001)` → 1 (the literals' own p).
- `Mod` is the exact floor remainder, and 0 when that rounds to zero at the
  display precision: `Mod(2/3;1/3)`, `Mod(1;1/3)*10^18`, `Mod(5;1/3)*10^18`
  → 0; `Mod(-1;1/3)*10^18` → `333333333333333332`, `Mod(2/3;-1/3)*10^18` →
  `-333333333333333332`, `Mod(7;2/3)*10^18` → `333333333333333330`,
  `Mod(0.666666666666666667;0.333333333333333333)*10^18` → 1 (p 18).
  `Div(1;1/3)` → 3, `Div(0.999999999999999999;1)` → 0.
- `Factorial(212)` works, `Factorial(213)` → 14 (10^405 again);
  `Factorial(2.9)`, `Factorial(-0.5)`, `Factorial(10;20)`,
  `Factorial(10;-1)`, `Factorial(10;2.5)` → 14; `Factorial("")` → 1,
  `Factorial(3;2)` → 6, `Factorial(5;0)` → 1.
- Display: `10^9+0.5` → `1.0000000005E+9`, `1.5*10^9` → `1500000000`,
  `10^9+10^-10` → `1.0000000000000000001E+9`, `0.0000000015` →
  `.0000000015`, `0.00000000099` → `9.9E-10`, `123456789000000000000.5` →
  `1.234567890000000000005E+20`.

**Text to number** (2.8): the first `+` or `-` before the first digit
decides the sign (`"+-1"` → 1, `"-+1"` → -1, `"a+b-1"` → 1), U+2212 is a
minus (`"−5"` → -5), and parentheses negate only when `(` precedes the
first digit and `)` follows it (`"(12"` and `"12)"` → 12, `"(1)2"` → -12,
`"1(2)"` → 12, `"(a)1"` → 1, `"((1))"` and `"(-12)"` → -1, -12). Pure form:
leading whitespace and trailing spaces are skipped, the latter only where
the thousands separator is not a space (`"1e3 "` → 1000 in en-US, 13 in
fr-FR and sv-SE; `"1e3" & Char(9)` → 13, `Char(9) & "1e3"` → 1000); `"5.e1"`
→ 50, `".5e1"` → 5, `"1,234.5e2"` → 123450, `"1 000,5"` → `1000,5` in
fr-FR and sv-SE (also with U+00A0 or U+202F); `"1e3.5"` → 13.5. Only the
exact text `?` is `?`: `" ? "`, `"??"`, `"a?"` → empty, `"1?"` → 1.

**Trigonometry** (2.4):

- The decimal becomes a double word by word: fraction words from the
  bottom (`(f + word) / 1e9`), then integer words from the lowest, each
  times a running scale 1, 1e9, 1e18, … (14.8):
  `Sin(1e-15)*10^15` → `.999`, `Sin(Exp(1))` → `.4107812905029084` (a
  correctly rounded conversion gives `...088`). The `%.20g` result is
  truncated to p + 2 digits: `Sin(1)*10^18` → `841470984807896504`.
- Arguments up to 1000 in magnitude go to `sin` directly (`Sin(1000)` →
  `.8268795405320025`); above 1000, `Sin`, `Cos` and `Tan` first take
  `Mod(x; 2π)` with 2π truncated to 18 digits (result in [0, 2π)):
  `Sin(1001)` and every integer argument up to 2,100 tried, `Sin(10^6)` →
  `-.3499935021711554`, `Sin(10^20)` → `.993720817727796`, `Sin(-10000)` →
  `.3056143888882539` while `Sin(10000)` → `-.3056143888882536`; with
  glibc's function values every reduced argument matches (battery
  `t12-trig-reduce`, 1,250 lines, `Sin(1529)` included, 14.8). (`-10^20`
  is `(-10)^20`.)
- The function values are glibc's: `Sin(21)` → `.8366556385360561`,
  `Sin(200)` → `-.8732972972139946` (macOS libm is one unit off there).
  glibc is not correctly rounded everywhere: that is `Sin(1529)` (14.8).
  `Sin("")` → empty, `Sin("abc")` → 0, `Sin(1e-20)` → 0 because the
  argument is cut to 18 fractional digits first (14.8).

**Comparison with `?`**: `(1/0) < 1`, `(1/0) ≤ 1`, `1 = (1/0)`, `1 < (1/0)`,
`1 ≤ (1/0)` → 0; `(1/0) > 1`, `(1/0) ≥ 1`, `1 > (1/0)`, `1 ≥ (1/0)`,
`(1/0) ≠ 1` → 1; `(1/0) ≠ (1/0)` → 0, `(1/0) ≤ (1/0)` → 1. A time against a
timestamp is 0 for `≥` and `≤` too.

**Dates and times**

- An empty timestamp keeps its type: `Timestamp(Date(1;1;2001);"") + 1` →
  `1/1/0001 12:00:01 AM`, `... = ""` and `... = 0` → 1, the sum of two →
  empty timestamp; `GetAsTime(" ")` and `GetAsTimestamp(" ")` → empty of
  their type.
- Arithmetic at the range ends: a date result of 0 or less becomes a number
  (`Date(1;1;1) - 1` → 0, `- 2` → -1, `- 1.5` → -.5), one in (0, 1) is 500
  (`Date(1;1;1) - 0.5`); the day count is truncated (`Date(1;1;2001) - 0.5`
  → 12/31/2000, `+ 0.9999` → 1/1/2001). A negative timestamp result becomes
  a number (`GetAsTimestamp(0) - 1` → -1, `Date(1;1;1) - Time(0;0;1)` → -1,
  `Time(1;0;0) - Timestamp(Date(1;1;2001);Time(0;0;0))` → -63113900400);
  above 12/31/4000 is 500 (`Date(12;31;4000) + Time(24;0;0)`).
- Text operands convert as numbers: `Time(1;0;0) + "1:30"` → 1:02:10,
  `Date(1;1;2001) + "1/2/2003"` → 1/14/2335, `Date(1;1;2001) + "abc"` →
  1/1/2001; but subtracting text without digits fails:
  `Date(1;1;2001) - "abc"` → 500, `Time(1;0;0) - "abc"` → 501.
- A timestamp is false only at 1/1/0001 00:00 (`If(GetAsTimestamp(0);1;0)`
  → 0); `If(Time(0;0;0.5);1;0)` → 1.
- Dates in text: mixed separators (`1/2-2003`), internal spaces, a sign,
  four fields, a five-digit year and year 0 (`1/2/0000`, `1/2/000`) → 500;
  `001/2/2003` → 1/2/2003, `1/2/00` → 1/2/2000; leading spaces and a
  trailing tab are fine. Two fields joined by the decimal separator read as
  a number and fail (`2.1` → 500 in en-US and ja-JP; `02.01.2026` in de-DE
  and fr-FR, `2026.02.01` in sv-SE); in year-first locales two fields are
  month and day (`2/1` → 2026/02/01 in ja-JP) and three start with the year
  (`1/2/03` → 2001/02/03, `03-01-02` → 2003-01-02). Display pads per locale
  and keeps the typed separator (`GetAsDate("2-1-2003")` → `02-01-2003` in
  de-DE).
- Times in text: a decimal last field of two is minutes and seconds
  (`1:30.5` → 0:01:30.5, `10:30.5` → 0:10:30.5, `1:30.` → 0:01:30); `1.` →
  0:00:01, `.5` → 0:00:00.5; a bare hour with AM/PM keeps an hour-only style
  (`1 PM` → `1 PM`, `12 AM` → `12 AM`, `0 PM` → `12 PM`, `GetAsTime("1 PM")
  + 0` → 13:00:00); with a marker the hour must be at most 12 (`13 PM`,
  `13:30 PM`, `24:00 PM` → 501) and decimal forms fail (`1:30.25 PM`, `1.5
  PM` → 501); markers are matched in any case after optional spaces or a
  tab (`1:30 P`, `1:30 A.M.`, `1:30 AM PM` → 501); `+1:30`, `1 : 30`,
  `1: 30`, `:30`, `1:`, `1::30`, `1.5:30` → 501, `- 1:30` → -1:30:00;
  `1:9999` → 167:39:00; `1/2/2003 13:30` → `1:30 PM`. ja-JP accepts `午前`
  and `午後` after the time (`1:05午後`, `1:05 午後` → 13:05; `午後 1:05`
  → 501); de-DE, fr-FR and sv-SE accept no markers. Fractions use the
  locale's decimal separator (`13:05:07,5` in de-DE, 501 in en-US).
- Timestamps in text: the time needs a colon (`1/2/2003 13`, `1/2/2003 1
  PM` → 500), minutes and seconds stay below 60 (`1:75`, `1:30:60` → 500),
  `T` is rejected, several spaces are fine. `24:00` and `24:00:00` are
  midnight of the next day (`1/2/2003 24:00` → 1/3/2003 12:00 AM; `24:30`,
  `24:00:01`, `25:00` → 500), and with `24:00` the date fields roll over
  instead of being checked: `2/30/2003 24:00` → 3/3/2003 12:00 AM (fr-FR
  `03/06/2005 00:00`), sv-SE `1/2/2003 24:00` → `2006/07/28 00:00`. A
  missing year is the current one (`1/2 1:00` → 1/2/2026 1:00 AM).
- Display: de-DE, fr-FR and sv-SE pad hours (`01:02:03`, `-01:00:00`,
  `00:00:01,5`); times of day omit zero seconds in every locale
  (`02.01.2003 13:00`, ja-JP `2003/01/02 0:00`, `GetAsTime(Timestamp(...;
  Time(1;5;7)))` → `01:05:07` in de-DE); ja-JP uses an unpadded 24-hour
  clock.

**Text**

- Characters (5.1): a base code point absorbs the following code points of
  non-zero canonical combining class, and nothing else:
  `Length("a" & Char(65039))` (VS16), `Length("a" & Char(3633))` (Thai
  U+0E31), `Length("क" & Char(2366))` (spacing mark), `Length("a" &
  Char(8419))` (enclosing keycap), ZWJ and ZWNJ → 2; `Char(1471)`,
  `Char(2509)`, `Char(1619)`, `Char(12441)` after `a` → 1. Conjoining jamo
  runs join (`Char(4352) & Char(4352)`, V+V, T+T → 1) but a precomposed
  syllable takes no jamo (`Char(44032) & Char(4520)` → 2). CR LF → 1, LF CR
  → 2, CR or LF then U+0300 → 2, tab or space then U+0300 → 1, a lone
  U+0300 → 1.
- Ordering is per character too: `"ss" < "ß"` → 1, `"ß" < "st"` → 0,
  `"ae" < "æ"` → 1, `"æ" < "af"` → 0, `"ﬀ" < "fg"` → 0, `"½" = "1/2"` → 0;
  primary weights only (`"á" < "a"` and `"a" < "á"` → 0); an ignorable
  character still counts (`"a" = "a" & Char(9)` → 0, `"a" < "a" & Char(9)`
  → 1; also `Char(127)`, U+00AD, U+200B).
- Tailorings: German keys ä, ö, ü as a e, o e, u e (`"ä" < "ae"` → 0,
  `"ae" < "ä"` → 1, `"ä" < "af"` → 0, `"æ" = "ä"` → 1, `"ø" < "ö"` → 1,
  `"å" = "a"` → 1). Swedish sorts å < ä < ö after z and before þ and ʒ,
  keys ø as ö, ü as y and w as v (`"ü" = "y"`, `"v" = "w"`, `"ø" = "ö"` →
  1; `"æ" = "ä"` → 0, `"æ" < "b"` → 1). French and Japanese compare like
  English in every probe (kana, voicing and width variants equal; Latin
  before Greek, Cyrillic and kana, kana before kanji). `"a" & Char(776) =
  "ä"` → 1 in all five.

Settled since (section 14): the series behind `Exp`, `Ln`, `Sqrt` and
fractional powers and their held digits (14.3-14.5), `Sin(1529)` (glibc's
rounding of a near-tie, 14.8), `Sin(1e-20)` (the argument keeps 18
fractional digits, 14.8) and `Atan` beyond 10^315 (14.8).

### Oracle answers (evaluator implementation)

Questions the evaluator (`om_calc::eval`) met that sections 1-13 left
open, settled with `tools/fms-calc-harness` (FileMaker Server
26.0.3.309, 2026-10-29) in the files of 12.1 and the Contacts template
(en-US), batteries `e01`..`e09` and `c06`, results in
`tools/fms-calc-harness/results/` [O-run]. `L(k ; v)` abbreviates
`Let ( $a = $a & "k" ; v )` after `Let ( $a = "" ; 0 )`; `→` gives the
result, then the error when not 0.

**Locals.** Let and While variables and custom function parameters live in
their compiled slots (calc-encoding.md 5.5) and keep their values after
their scope ends, so a `Let` in a `While` condition that takes a slot of
the loop's logic overwrites what the result reads: `While ( [ i = 0 ] ;
Let ( z = i * 100 ; z < 300 ) ; [ w = i ; i = i + 1 ] ; w )` → 300. The
logic's names read empty until the logic runs, also where an earlier
scope used the slot: `While ( [ i = 0 ] ; 0 ; [ x = 5 ; i = i + 1 ] ; x &
"|" & i )` → `|0`, `Let ( [ a = 1 ; b = 7 ] ; 0 ) & While ( [ i = 0 ] ; 0
; [ x = 5 ] ; x )` → `0`. Not modelled: with the logic `[ i = i + 1 ; w =
i ]` (w in slot 2, z in slot 1) the first loop still gives 300, while
`While ( [ i = 0 ] ; Let ( z = i ; z < 3 ) ; [ i = i + 1 ; w = i * 2 ] ;
w )`, compiled to the same slots, gives 6: the engine keeps locals on its
solver stack and the overlap depends on the expressions. Custom functions
have frames of their own (`Let ( pBeginn = 7 ; __GEN_Range_ID ( 1 ; 2 ; 1
) & pBeginn )` → `1¶27`).

**Locals bound to fields.** A Let or While variable bound to a field (or to
`If`, `Evaluate`, `GetField`, another variable giving one) keeps the
field reference along with the value read when it was bound:
`Let ( f = ${Related Tasks}::Task ; Count ( f ) )` → 12, `List ( f )` all
twelve, `GetFieldName ( Let ( f = Task ; f ) )` → `Tasks::Task`,
`GetNthRecord ( Let ( f = Task ; f ) ; 2 )` → `Checklist`, through `[ f =
… ; g = f ]` and `While` initial values too; `Let ( f = ATB::flex_Label[10]
; 1 )` → 1, error 14; `Let ( f = Assignees::Name ; IsValid ( f ) )` → 1
while `IsValid ( Assignees::Name )` → 0. `$` variables hold values only:
`Let ( $f = ATB::flex_Label ; Count ( $f ) )` and `GetFieldName ( $f )` →
`?`, 102 (a field position given anything but a field is 102).

**Evaluation order.** Operators, fixed-arity functions, `Let`, `Case`,
`Choose` and `While` (initial, condition, logic, condition, ..., result:
`1cLcR`) evaluate left to right; `SetPrecision` and `SetRecursion` their
second argument first (`21`). Every function stored with a count
evaluates its arguments last first: `List ( L(1;1) ; L(2;2) ; L(3;3) )`,
`ExecuteSQL` → `$a` = `321`; `Max` → `21`; `Substitute` with groups →
`54321`; `JSONSetElement` → `4321`; `GetLayoutObjectAttribute`,
`FieldNames` → `21`; custom functions (`__GEN_Range_ID`) → `321`; but
`Left`, `Middle`, `Date`, `GetValue` → `12`, `123`. Errors follow the order:
`EvaluationError ( List ( 1/0 ; Sqrt ( -1 ) ) )` → 14, `EvaluationError (
Left ( 1/0 ; Sqrt ( -1 ) ) )` → 15.

**Limits.** `SetRecursion ( expr ; n )` gives `expr` a call count of its
own whose calls do not count outside: `Length ( __GEN_RandomPW ( 30000 ;
"" ; 1 ) ) & SetRecursion ( Length ( __GEN_RandomPW ( 30000 ; "" ; 1 ) ) ;
40000 )` succeeds in either order, `SetRecursion ( … 20000 … & … 20001 … ;
40000 )` → 7, `SetRecursion ( SetRecursion ( … 10 calls … ; 10 ) & … 5
calls … ; 5 )` succeeds. Its limit covers loops nested inside (`SetRecursion
( While ( 2 iterations ; While ( 3 iterations ) ) ; 2 )` → 7) and ends with
`expr` (`SetRecursion ( SetRecursion ( 1 ; 5 ) & While ( 3 iterations ) ; 2
)` → 7); n is truncated and read from text leniently (`3.9`, `"3abc"` →
3), and `?` allows none. `Evaluate` text runs with the default limits
whatever surrounds it (`SetRecursion ( Evaluate ( "… 10 calls …" ) ; 5 )`
succeeds; 49,999 calls inside `Evaluate` succeed, 50,000 → 7; two
`Evaluate`s of 25,000 calls each succeed). Error 7 inside `Evaluate` ends
the whole calculation and wins over an earlier error (`SetRecursion (
While ( … ) ; 1/0 )` → 7). Two loops of 50,000 iterations in one formula
succeed.

**SetPrecision.** n `?` → 16 (error 15 kept), `""` → 16, `"20abc"` → 20,
`17.9` → 17; text and dates pass unchanged.

**Variables.** `$x[n]` unset is an empty number, `$x` unset empty text. The
repetition: empty → 1, text without digits → 0 (`$v["x"]` is `$v[0]`),
`-0.5` → 0, `2.9` → 2, `-2.9` → -2; an index that is `?` reads empty and
assigns nothing (error 15 from the index). `$` and `$$` names are separate
(`Let ( [ $$g1 = 1 ; $g1 = 2 ] ; $$g1 & $g1 )` → 12); `Evaluate` text
assigns the caller's variables (`Evaluate ( "Let ( $ev = 9 ; 1 )" ) & $ev`
→ 19); a variable holds `?` (`Let ( $e = 1/0 ; $e & "|" & EvaluationError
( $e ) )` → `?|0`, 15); assigning empty removes it.

**Evaluate, IsValidExpression, IsValid.** `Evaluate ( 1/0 )` → `?`, 15;
`Evaluate ( "?" )` → 106; `Evaluate ( "Self" )` → 1225; `Evaluate ( "1+" )
& Evaluate ( "1/0" )` → `??`, 1204. `IsValidExpression` checks syntax and
names only: `""`, `" "`, `1`, `"1/0"`, `"$x"` → 1; `"x"`,
`"Contacts::NoSuchField"`, `"Self"` → 0; an error of its argument counts
(`IsValidExpression ( 1/0 )` → 0, 15). `IsValid` is 0 when evaluating its
argument raised an error (`IsValid ( Let ( x = 1/0 ; 1 ) )`) or read a
related field with no related record (`IsValid ( Assignees::Name & "" )`,
`IsValid ( Extend ( Assignees::Name ) )`), not for `?` without an error
(`IsValid ( "?" )`, `IsValid ( GetAsNumber ( "?" ) )` → 1); the error
does not count outside.

**GetField, GetFieldName, GetBaseTableName.** `GetField` compiles its text
as a field reference in the context occurrence (`Task`, `task`, ` Task`,
`${Task}`, `Task[1]`, `Related Tasks::Task`); empty text is empty without
error; anything else is `?`, 102, also without a table context (`"x"`,
`"$persist"`, `"1+1"`, `"Nope::Task"`). `GetFieldName` and
`GetBaseTableName` need a field operand (a field, `GetField`, `Evaluate`,
a variable bound to a field, an `If`/`Case`/`Choose` branch): `GetFieldName
( "Task" )`, `( 1 )`, `( $v )` → `?`, 102. The name is `Occurrence::Field`
with `[n]` for a reference to repetition n > 1 (`ATB::flex_Label[2]`,
`GetField ( "ATB::flex_Label[3]" )` → `ATB::flex_Label[3]`; `[1]` and
`Extend` add nothing); an index out of range is 14.

**Field operands.** Without a current record, with a table occurrence or
without one, every field reference ends the calculation with 1224, global
fields included: `Let ( f = Task ; 1 )`, `EvaluationError ( Let ( f = Task
; 1 ) )`, `GetFieldName ( Task )`, `GetField ( "Task" )`, `IsValid ( Task
)`, `Lookup ( Contacts::First Name ; "fail" )` → `?`, 1224. `field[n]` and
`GetRepetition` check the index when the reference is made, in any
position: `List ( ATB::flex_Label[10] )`, `Count ( ATB::_Table[2] )`,
`Count ( ${Related Tasks}::Task[2] )` → `?`, 14 (an aggregate whose lone
argument fails is `?`, not a count); the index `""` → 14, `"2x"` → 2,
`1/0` → `?` with 15 only. `Extend` takes repetition 1 of any reference
(`Extend ( ATB::flex_Label[2] )`), `GetRepetition` replaces an index or
`Extend` (`GetRepetition ( Extend ( ATB::flex_Label ) ; 2 )` → repetition
2). A lone non-repeating field argument of an aggregate gives its value in
every calculation repetition (`Count ( ATB::_Table )` in repetition 2 →
1), as one of two arguments it reads empty there (`List ( ATB::flex_Label
; ATB::_Table )` → `"Text"`). `Last` ignores the reference's index and the
repetition being calculated. `GetNthRecord` reads the reference's
repetition on record n (`GetNthRecord ( ATB::flex_Label ; 1 )` in
repetition 2 → repetition 2 of record 1); n is truncated and read from
text leniently, `""` → 14, `1/0` → `?` with 15 only. Field references
reach field positions through `If`, `Case`, `Choose`, `Let`, `Evaluate`
(`Sum ( Evaluate ( "${Related Tasks}::${TASK ID MATCH FIELD}" ) )` → 78)
and `GetField`.

**Lookup.** With no related record: the fail expression, evaluated only
then (`EvaluationError ( Lookup ( Assignees::Name ; 1/0 ) )` → 15), else
`?`, 401; a field of the context's own table → 103 or the fail
expression; `LookupNext` with nothing related and no index → 401. `Lower`
= 0, `Higher` = 1. Unexplained: through the sample's cartesian self-join,
where every record is related, `Lookup ( ${Related Tasks}::Task )`, with
a fail expression too, and `LookupNext ( ${Related Tasks}::Task ; Lower )`
are empty without error.

**Commas.** In a file whose decimal separator is a point, `,` is the
argument separator and is stored as `;` (`03`): `List ( 1,5,2 )` → three
values, `Let ( [ a = 1 , b = 2 ] ; a + b )` → 3, `Substitute ( "abc" , "a"
, "x" )` → `xbc`, `While ( [ i = 0 , j = 1 ] , i < 2 , [ i = i + 1 ] , i &
j )` → 21; elsewhere it fails like `;` (`1 , 2` → 1212, `( 1 , 2 )` →
1207, `Abs ( 1 , 5 )` → 1202). Compile-mode bytes in `c06`
(calc-encoding.md 6.1).

**Other.** `Choose` with a `?` index → `?`; `If` and `Case` take a `?` test
as true; a date index is its day number (`Choose ( Date ( 1 ; 1 ; 1 ) ;
"a" ; "b" )` → `b`). `GetLayoutObjectAttribute` without a layout → empty;
`ExecuteSQL` with an unknown table → `?`, 8309. Without a record
`Get ( RecordNumber )`, `FoundCount`, `RecordID` → 0, 0, empty; on a record
`FoundCount` and `TotalRecordCount` are the table's record count.

### Implementation notes (client calculations)

Gaps the client host (`om_data::host`, `om-provider/src/calc.rs`,
`record_semantics.rs`, `om-gui/src/calc.rs`) settled, with their basis.
[O] here is a black-box reading of sample files with `om-hbam`
(AppleTouchBar and Phidget Digital Outputs MBS examples, the shipped
templates).

- **Repetitions in records [O, I].** Repetition 1 of field F is the record
  cell keyed F; repetition n of 2 to 255 is the cell keyed F followed by
  the byte n (`0f 02` is field 15, repetition 2; `1a 02`..`1a 08` the
  eight repetitions of Phidget's `Connected`, matching battery x02).
  Repetitions above 255 and fields with multi-byte ids follow the same
  rule [I].
- **Global values [O].** A table's global fields are stored once at
  `[T, 08, F]` (repetitions as above [I]). Styled text, there and in
  records, is a chunked value (`0` header, `1..n` pieces) beside a style
  subtree `ff`; the client reads the pieces and drops the styles.
- **Summary definitions [O, I].** Field key `09`: byte 0 the kind (1 Total
  and 3 Count, the only kinds in the corpus), at offset 8 the summarized
  field as a key string `02 01 <id>`; the other bytes are not decoded. The
  client computes Total and Count of the first repetition over the found
  set; other kinds, and `GetSummary` with a break field other than the
  summary itself, are error 1225. (Now specified: the whole 30-byte
  definition, every kind and option, and executed values in
  [file-metadata.md](file-metadata.md) section 5; computed by
  `om_data::summary` for every host, below.)
- **Found set and related sets [I].** Without a find the found set is the
  table's record list `[T, 0d]` in record id order (creation order, as on a
  record cursor, 12.1). Related sets are computed by comparing match field
  values with the calculation comparison (6.2) over every record of each
  table on the path, in creation order, not with index keys (JoinProjection,
  query-program.md 3.1). A relationship with either sort option bit
  (`0xc0` of its last option word, schema-path.md 5.4) orders its records in
  a way the client cannot read: reading one related record of a sorted path
  (`Rel::f`, `GetNthRecord`, `Lookup`) is error 1225; aggregates use
  creation order. (Now specified: the sort specs live in the relationship
  definition, every operator reads "left op right", and the related order
  is the direction's sort spec when its bit is set, else creation order;
  [file-metadata.md](file-metadata.md) section 4.)
- **Errors for missing data.** A record the source does not have is 101, an
  occurrence the context is not related to 103; a client evaluating from a
  cache reads data it has not fetched yet as 1224 and lists it for its
  provider, and treats the result as not yet known (an object with a hide
  condition is withheld, [conditional-visibility.md](../conditional-visibility.md)).
- **Data a provider cannot read [implementation].** A desktop provider
  answers a batch of needs table by table: a table whose download fails,
  or that the calculation schema lacks, leaves its needs out of the answer
  and is not read again in that batch, while the needs of other tables are
  still answered. The answer carries the first failure of each table it
  could not read. The batch fails as a whole only when nothing could be
  read. The client treats a need missing from an answer as failed, so the
  calculations that need it show `?`, and does not ask for it again until
  the data changes. A hide condition that read such a need is unresolved
  rather than hidden: its `?` is not FileMaker's answer. The window tells the user why a table could not be
  read the first time it fails for the open file, not again each time its
  needs are asked for; a batch that fails as a whole is reported each
  time.
- **Calculated source match fields [I, implementation].** Relationship
  scans and indexed equality lookups evaluate an unstored source field
  using the normal field evaluator on the source record, in the formula's
  occurrence. This retains its declared result type, repetitions,
  empty-input rule, custom functions and session global field values.
  Empty repetitions relate no records. Pending reads return 1224 and are
  retried; an invalid result is an error, never a literal `?` lookup key.
  The index remains an index of the target's stored field; no unstored
  target index is synthesized. Circular source keys use the host's bounded
  read depth. The downloading provider supplies connection metadata for
  record-independent `Get` answers, such as AccountName. This is not a
  window or script host: `$`/`$$` variables, window/script `Get` selectors,
  summaries, dynamic `Evaluate`/`GetField`, SQL and plug-in calls in a key
  or its nested field/function definitions are conservatively refused
  with 1225, even in untaken branches. Let/While local slots and custom
  function parameters are supported. Synthetic client and loopback server
  regressions cover these rules; this is not a new native semantics probe.
- **Indexed related sets [implementation].** The desktop provider narrows
  each hop of a relationship path with the destination field's value index
  (a whole-value find, query-program.md 6.4) and compares only the
  candidates, as the scan does. A hop relates the union of what each record
  of the previous hop relates; a hop whose predicates are all `×` is the
  destination's whole record list, read once (whatever the previous hop
  relates) and without its records; an empty hop ends the path. The result
  is in creation order, or in the last relationship's sort
  (file-metadata.md 4.3). A hop the index cannot answer (another operator,
  no value index, a key a find cannot express, a calculated target field)
  leaves the whole path to the scan. A find on a field without the index
  it reads gets no records while FileMaker Server builds that index
  (query-program.md 7.1), and the option bit `0x80000000` does not prove
  the index node exists (schema-path.md 6). So each find's op86 program
  first GetKeys the value index's counts leaf `[T, 01, F, 10]` (7.1, 8.5);
  instructions run in order (query-program.md 0), so the GetKey sees the
  file before the find does. Without the counts the scan answers, and the
  find has had the host build the index for later lookups.
- **Stored date text and styles [O-run, implementation].** Native Pro's
  Data Viewer (macOS, 2026-10-30, read-only) distinguishes a stored date
  from a newly converted date: a field read, `GetAsText` and a nested
  text-result calculation keep its entered text and character color.
  `GetAsDate ( field )` and `field + 0` instead normalize it with the
  evaluation locale and discard that color. `DateText` holds the parsed
  date alongside that stored presentation, as `NumberText` does for
  numbers. Declaring an existing `DateText` as a date keeps it; explicit
  casts and arithmetic produce a plain date. Comparisons, date functions,
  SQL values, lookup keys and typed exports use its parsed day, not the
  entered text. `om_data::host::stored_text` accepts decoded styled text;
  `stored_value` is the plain-text entry point. Stored time/timestamp
  presentation remains unsupported.
- **Stored character-style reads [implementation].** The desktop
  provider decodes record and stored-global character runs with the
  existing layout style-stream reader. Font IDs resolve through the
  owning file's `[25, 5]` catalog, already downloaded with layout metadata.
  Text, numeric text and date text keep masked font, size, color and face
  overrides through calculation text operations. An explicit face reset
  does not reset unrelated faces; malformed runs retain plain text.
  Synthetic TLS client/server tests cover the read and font-catalog path.
  This does not add styled writes or GUI painting of those runs.
- **Related files' formulas [I, implementation].** A formula is solved
  for the `DBFileUser` that owns it (9.1), so a related file's formula,
  whether read through the root file's occurrence of its table or solved
  in one of its own occurrences, uses that file's names and custom
  functions, its formats (9.3, also in `Evaluate`, 8.9), its `$$`
  variables (script-runtime.md 5.1) and its `Get ( FileName )`, `Get (
  FilePath )` and `Get ( FileLocaleElements )`; `$` variables stay the
  running script frame's (script-runtime.md 5.1). A value a file computes,
  such as a summary, carries that file's locale (9.2). `om_data::host`
  keeps one calculation schema for all of them:
  `CalcSchema::import_file_schema` adds a related file's base tables,
  fields, occurrences, custom functions, relationships and value lists
  under client-only ids, counted down from `u64::MAX - 1` past every id in
  use and every id an earlier import handed out, including ids reserved
  for members a formula names but the file does not define or the account
  cannot read. Ids stored in the files never change; the import's map
  gives each imported table's own id for reading its records. The root
  file lists imported members as `File::Name`; the related file's own
  names, locale, fonts and ExecuteSQL catalog (its system tables show its
  own names and ids) are found from the file owning the formula, kept
  apart from its occurrence like `mFileUser` (9.1): the context
  occurrence's file, or for a field's formula the file owning the field's
  table, also when the formula has no occurrence of its own (occurrence 0
  or none, calc-encoding.md 1) and is solved in another file's occurrence
  reading the field. That occurrence then resolves the owning file's
  fields unqualified in `Evaluate` and `GetField` text, but the owning
  file's names do not find it. A calculation's result is converted to its
  declared type (6.5) in its own file's locale. `bind_external_occurrence`
  resolves an occurrence of another file's table to the imported table,
  keeping its data source's name for `RelationInfo`. A related file's
  `$$` variables share the host's map under keys no variable name can
  take, and its `Get ( FilePath )` is the root file's path with its own
  name, where a relative file reference finds it. The Design functions
  answer for the file named, `""` being the formula's own: each lists
  only its own occurrences, base tables, fields and value lists, under
  its own names and ids, sorted in its own language; a related file's
  layouts, scripts and windows are not imported, so `LayoutNames`,
  `ScriptNames`, `FieldStyle` and the like answer empty for it unless the
  session also has it open (`SessionInfo::other_files`, calc-functions.md
  9, "Design functions of other files"), when they answer from that open
  file's schema. `WindowNames` lists the windows the session gives the
  file (none for a file only imported), and `DatabaseNames` the session's
  open files (only the root file without a session). The desktop
  provider opens related hosted files (file-metadata.md 1.1,
  network-protocol.md 5.5 "Related files") and reads each imported table
  on its file's connection under the file's own table ids; a related file
  that cannot be opened leaves its occurrences unresolved, so what reads
  them is `?`. A lookup's text (`CalcSource::lookup`) is in the formats of
  the file holding the table, so a date relates by its day whichever
  file's order it is written in. Script steps forward the owning-file
  context too: related formulas keep their names, formats, per-file Get
  selectors and `$$` variables, while `$` variables remain in the running
  script's frame.
- **Locale and storage text [I].** The file's own formats are not read;
  evaluation uses en-US. (Now specified: the file's locale record `[2, 0d]`
  and how the engine uses it, [file-metadata.md](file-metadata.md)
  section 2.) A stored or auto-entered calculation result is
  stored as the en-US text of the value converted to the field's type (as
  the observed timestamp storage form). Stored text reads back through
  `ConvertData` (6.5); a date or time that does not parse stays text.
- **Record rules [I].** Option `0x20` clear means "do not evaluate if all
  referenced fields are empty": a stored calculation is then empty and an
  auto-enter calculation is not evaluated. Stored and auto-enter
  calculations are re-evaluated when a field they reference directly
  changes (references inside `Evaluate` or `GetField` text do not count, as
  for FileMaker). Validation by calculation (`19`) runs with `Self` = the
  field's value when the field changed (always with `0x400`), not on an
  empty value unless `0x2` is set; its custom message is the text at `18`
  (`0x80`). A calculation that ends with an error refuses the write.
- **Layout calculations [I].** They store occurrence 0 and run in the
  window's occurrence on the row's record (the portal row in a portal).
  `Self` is a field object's value, a text or button object's text. A `?`
  hide condition hides; one OpenMaker cannot evaluate as FileMaker would
  is unresolved ([conditional-visibility.md](../conditional-visibility.md)). Conditional formats whose formulas hold apply in
  stored order, later ones over earlier ones. Merge variables are
  `<<$name>>` and `<<$$name>>` with an optional `[n]`; the symbols
  `{{CurrentDate}}`, `{{CurrentTime}}`, `{{UserName}}`, `{{PageNumber}}`
  and `{{RecordNumber}}` show their `Get` selectors (Claris
  documentation).
  - [O] One hosted solution's layout text is stored as `Today is:
    {{1002}}`, the symbol a number where the name would be.
  - [I] The number is the selector's engine op, 1000 + its index (9.2,
    slot 13; calc-functions.md 2), so `{{1002}}` is CurrentDate (2). The
    numbers of the other symbols follow from the same table: CurrentTime
    1029, UserName 1031, PageNumber 1015, RecordNumber 1020 and PageCount
    1125. None of them has been seen in a file.
  - OpenMaker reads these numbers, written as FileMaker writes numbers (no
    sign, no leading zero), as the symbols' names, and PageCount's where
    page symbols are checked. Any other number stays as typed.
  - Unmeasured: which files store numbers rather than names (perhaps ones
    converted from older formats), and what FileMaker Pro shows for
    them.
- **Unstored calculations with every reference empty [O-run].** "Do not
  evaluate if all referenced fields are empty" (option `0x20` clear, the
  default) applies to unstored calculations too: on FileMaker Server's
  engine `N * 2` is empty, not 0, on a record without N, in finds and in
  ExecuteSQL (script-runtime.md 13.19, execute-sql.md 12 S3). The evaluator
  implements it where it computes an unstored field: when the host says
  the field does not evaluate always (`CalcHost::evaluate_always`, default
  true), it solves the formula, then gives the empty value of its type when
  the formula read a field and every field it read was empty (a container
  counts as a value, as in ExecuteSQL; a read that failed is `?`, not
  empty), unless it ran a field inspection function (next item). Only what
  runs is read: the fields of an unchosen `If`, `Case` or `Choose` branch
  or a skipped `and`/`or` side neither count nor are fetched, and a formula
  that reads no field evaluates. `Evaluate` and `GetField` text, custom
  functions' bodies and nested unstored calculations keep their reads and
  inspection functions to themselves; a nested calculation's value is one
  read of its caller, and a custom function's arguments are read by the
  caller. An aggregate reads its field on every related record, and reads
  it empty without one. ExecuteSQL's own check still scans the formula's
  references (`om_formula::Expr::field_references`). The desktop host
  answers from the field's option word; OpenMaker Server's script host does
  not answer yet (it evaluates always).
- **Field inspection forces the result [O-code].** With the option
  selected, FileMaker Pro 26.0.3 shows `If ( IsEmpty ( Input ) ; "Fallback"
  ; Input )` as `Fallback` on a record whose Input is empty. In its macOS
  arm64 DBEngine (addresses of that build, not the Linux ones above),
  `DBRecordCursor::GetCalculatedData` at `0x173c3dc` reads option `0x20`
  into the solver's always-evaluate flag; `DefaultSolvePolish` at
  `0x1707ee8`/`0x1708510` recognizes `GetAsBoolean` (`0x3e`), `IsEmpty`
  (`0x4e`), `GetFieldName` (`0xc81`) and `GetBaseTableName` (`0xc90`) and
  sets the nonempty state at `0x1708520` whatever the operand's value, as
  the `IsValid` path does at `0x1701e5c`; `SolvePolish` checks that runtime
  state and the always-evaluate flag at `0x170278c` before it clears the
  result. The evaluator keeps the state per formula while it solves
  (`om_calc` `FieldReads`) instead of reading every reference first.
- **The host's own reads of a field.** Record pages, field objects, finds
  and a summary's values read a field through the evaluator's field read
  (`om_data::host::read_field`, a reference `Table::Field` evaluated on the
  record), so an unstored calculation gets its formula's occurrence, its
  repetition rule, the rule above and the caller's variables. A summary's
  inputs and a sort's group keys, which `FileCalc` reads itself, read one
  copy of the variables of the host that asked, made once for that read and
  shared by its records (`FileHost::with_file_variables`, `VariableSnapshot`;
  [#615](https://github.com/OpenMaker-Dev/openmaker/issues/615)), so what one
  input assigns the next sees and it is dropped after [I: FileMaker's for
  assignments in a summary's inputs is unmeasured]. The summary cache keys
  each result by the copy's `Variables::revision` pair (every assignment
  takes a new one), so a total read again after an assignment in the same
  evaluation is read again. A `FileCalc` used without a host reads with no
  variables. Nested reads the host starts itself
  (a summary of an unstored calculation that reads a summary) stop at 32
  levels with `?`, as only a circular, broken file gets there.
- **Summaries [O-run, I].** `om_data::summary` computes them for the
  desktop host (and OpenMaker Server's scripts) over the source's found
  set of the summary's occurrence, in its order, with the rules of
  file-metadata.md 5.3. Inputs: the summarized field's repetitions as
  calculations read them, the weight (additional field) on the record or
  on the first related record when its occurrence shows another table, and
  the groups of a break field when the found set is sorted by it (records
  equal in every sort field up to it). An additional field stored without
  an occurrence is ignored (5.4). A record outside the found set sees the
  summaries that do not depend on the record; a running summary or a
  fraction is empty there [I]. A total of nothing is empty.
- **Session `Get` answers.** `ApplicationVersion` is `Pro 26.0.3`;
  `SystemPlatform` 1 on macOS, -2 on Windows and 8 (FileMaker's Ubuntu host
  value) on Linux; `Device` 1, 2 or 6; `MultiUserState` 2; `FilePath`
  `fmnet:/<host>/<file>.fmp12`. A record write answers the clock selectors
  and `UUID` from one reading for the whole record.

### Oracle answers (value-core follow-ups)

Settled with `tools/fms-calc-harness` (FileMaker Server 26.0.3.309,
2026-10-28/29) in the Contacts templates of eleven languages [O-run]:
batteries `vc-styles`, `vc-numbers`, `vc-errors`, `vc-collation`,
`vc-collation-max`, `vc-collation-pairs`, `vc-sortnames` and
`fn-embedding`, results in `crates/om-calc/tests/data/` (checked by
`tests/value_core.rs` and `tests/fn_embedding.rs`). `→` gives the result,
then the error code when not 0. Runs are written `[text]`, `[text|blue]`.

**Style runs.** The harness's `~` run dump prints nothing; `GetAsSVG`
lists every run. A text's runs cover all of it, unstyled stretches
included, and neighbouring runs may have equal styles.

- `&` keeps the left text's runs and merges each run of the right text
  into the run before it when the styles are equal. With `K` =
  `TextColorRemove ( TextColor ( "a" ; 255 ) & "b" ; 255 )` → `[a][b]`:
  `K & "c"` → `[a][bc]`, `"c" & K` → `[cab]`, `"" & K` → `[ab]`,
  `K & ""` → `[a][b]`, `K & K` → `[a][bab]`.
- The setting functions (`TextColor`, `TextFont`, `TextSize` with a
  non-zero size, `TextStyleAdd`) and the removing ones without a value
  (`TextColorRemove ( t )`, `TextFontRemove ( t )`, `TextSizeRemove ( t )`,
  `TextFormatRemove`) change every run and merge equal neighbours:
  `TextColor ( K ; 255 )` → `[ab|blue]`, `TextSizeRemove ( K )` → `[ab]`,
  `TextStyleAdd ( TextStyleAdd ( "a" ; Bold ) & "b" ; Bold )` → one run.
  The removing functions given a value (`TextColorRemove ( t ; color )`,
  `TextFontRemove ( t ; font )`, `TextSizeRemove ( t ; size )`,
  `TextStyleRemove`) change the matching runs in place and merge nothing,
  even where nothing matched: `TextColorRemove ( K ; 65280 )`,
  `TextStyleRemove ( K ; Bold )` → `[a][b]`; `TextColorRemove ( TextColor (
  "a" ; 255 ) & TextColor ( "b" ; 255 ) ; 255 )` → `[ab]` (the `&` had
  merged them). `TextSize ( K ; 0 )` and `TextStyleRemove ( K ; 0 )` return
  the text as it is.
- Kept: slices (`Left ( K3 ; 3 )` → `[ab][c]`, `Middle ( K3 ; 2 ; 2 )` →
  `[b][c]` with `K3` = `[ab][cd]`), `GetValue`, the words functions,
  `Upper`/`Lower`/`Proper` (`[AB][CD]`), `Trim`, `GetAsText`, `If`, `Let`,
  and the functions that delete characters in place: `Filter` and
  `FilterValues` (`Filter ( K3 ; "ad" )` → `[a][d]`), whose deleted runs
  let equal neighbours merge (`Filter ( TextColor ( "a" ; 255 ) & "b" &
  TextColor ( "c" ; 255 ) ; "ac" )` → `[ac|blue]`). `Quote` puts its marks
  in the first and last characters' runs (`[“ab][cd”]`), an escape in the
  run of the character before it. `LeftValues` and the other value
  windows append their return to the last run. Rebuilt and merged:
  `List`, `SortValues`, `UniqueValues`, `SerialIncrement`, the Japanese
  kana and width conversions (`Hiragana ( [カ][キ] )` → one run).
- `Replace` and `Substitute` edit in place, each match left to right:
  the replacement's base style is the first replaced character's (for an
  insertion the next character's, at the end the last one's) with the
  replacement's own attributes laid over it; the replaced characters go,
  and when a whole run goes the runs that meet merge if equal; the new
  characters join the run they land in when it has their base style, at
  a boundary the run before them or else the run after them, otherwise
  they start a run; replacement pieces whose style differs from the base
  then become runs of their own that merge with the run after them when
  equal, or with the run before when they end the text. So `Substitute (
  K3 ; "c" ; "c" )` → `[abc][d]`, `Substitute ( K3 ; "ab" ; "X" )` →
  `[Xcd]`, `Replace ( K3 ; 3 ; 0 ; "X" )` → `[abX][cd]`, `Replace (
  TextColor ( "a" ; 255 ) & "bc" ; 2 ; 0 ; "X" )` → `[a|blue][Xbc]`,
  `Replace ( TextColor ( "a" ; 255 ) & "b" & TextColor ( "c" ; 255 ) ; 2 ;
  1 ; TextColor ( "X" ; 255 ) )` → `[a|blue][Xc|blue]`, with `"X"` →
  `[a|blue][X][c|blue]`, `Replace ( TextColor ( "ab" ; 255 ) & "c" ; 3 ; 1
  ; TextColor ( "X" ; 255 ) )` → `[abX|blue]`. One case is not modelled:
  replacing the middle run of `[a|blue][b][c|blue]` with `"Y" & TextColor
  ( "X" ; 255 )` leaves `c` unstyled (`[a|blue][Y][X|blue][c]`).
- `TrimAll` edits in place: the space it keeps stays in the run of the
  space run's first space, an inserted one (trimType 1) joins the run of
  the character before it; when the kept space is the first or last
  character of its run, the runs around it merge if equal: `TrimAll ( [a
  ][ b] ; 0 ; 0 )` → `[a b]`, `TrimAll ( [a][  b] ; 0 ; 0 )` → `[a b]`,
  but `TrimAll ( [a  ][  b] ; 0 ; 0 )` → `[a ][b]` and `TrimAll ( [a b][c]
  ; 0 ; 0 )` → `[a b][c]`.
- Empty runs never survive (`TextColor ( "" ; 255 ) & "a"` → `[a]`).
  `GetAsCSS` shows the same boundaries (one span per run).

**Text to number** (2.8). Fullwidth forms read as ASCII (`（１ｅ２）`,
`１，５`, `－1`, `．`), and U+2010, U+FE63 count as minus, U+FE59/FE5A as
parentheses (superscript ones do not). Impure reading: signs and
parentheses count only before the first digit and before the decimal
separator: the first `+`/`-` is the sign, `(` opens and `)` cancels the
opening; between the separator and the first digit `(` or `)` cancels it
and signs are ignored; after the first digit the first parenthesis
decides, `)` closing an open one (negative) and `(` ending the matter.

| Text (en-US) | Number | Text | Number | Text | Number |
|---|---|---|---|---|---|
| `.(22)` | .22 | `(.22)` | -.22 | `(.)22` | .22 |
| `.-22` | .22 | `-.22` | -.22 | `,-22` | -22 |
| `(1(2))` | 12 | `(1)(2)` | -12 | `((1)2)` | -12 |
| `(a)1)` | 1 | `(.(22))` | .22 | `1(.2)` | 1.2 |

The pure form may sit inside parentheses, any number of `(` after the
sign and `)` at the end, padded with spaces unless the thousands
separator is a space; it is negative for a `-` sign or both a `(` and a
`)`: `(1e2)`, `((1e2))`, `( 1e2 )` → -100, `(1e2` → 100, `-(1e3)` → -1000,
`(1e800)` → `?`; `(-1e3)` → -13 and `(1e2)x` → -12 (impure); in fr-FR and
sv-SE `( 1e3 )` → -13. Thousands separators after a digit may precede the
exponent: `1,e2` → 100 in en-US, `1.e2` → 100 in de-DE, `1 e2` (one or
more spaces) → 100 in fr-FR (U+202F does not count there). The same rules hold
with `,` as the separator in de-DE, fr-FR and sv-SE (`,(22)` → ,22).

**Error -1.** `CalcError` codes are signed. -1 comes from `JSONMakeArray`
of empty text (`JSONMakeArray ( "" ; "" ; JSONString )`, also with any
separator or type), `JSONUniqueValues` with three or more arguments,
`JSONRemoveValues` with four, and `GetEmbeddingAsText` of text. It is an
ordinary error: the result is `?`, the first error wins (`EvaluationError
( JSONMakeArray ( "" ; "" ; JSONString ) & 1/0 )` → -1, `1/0 & ...` → 15),
`EvaluationError` returns -1 (`... & "x"` → `-1x`, `< 0` → 1), `IsValid`
→ 0, `Let ( x = ... ; "ok" )` → `ok`, -1. `JSONValueExists`,
`JSONValueCount` and `JSONAnyValuesExist` with extra arguments are 1202.

**Collation by file language** (5.3). The other Contacts templates are
Spanish (Modern) (`IDNum` 39), Italian (30), Dutch (20), Portuguese (34),
Korean (76) and Chinese (Pinyin) (45); their formats are in the
`Locale` constructors (`es_es` ... `zh_cn`; Italian times use `.`, Korean
puts AM/PM before the time, Chinese names October `一月`). In `<`, `=`,
`SortValues` and `UniqueValues`: Spanish sorts `ñ` as a letter after `n`
(`"nz" < "ña"`, `"ñ" = "n"` → 0; no `ch`/`ll` contractions); Italian,
Dutch and Portuguese compare like English; Japanese orders the kanji of
JIS X 0208 by their JIS code (`阿 < 一 < 二 < 八`, `七 < 丁`, `乂 < 丂`),
before the other ideographs, whose order is not settled (`𠀋 < 龥`);
Korean orders Hanja by their Hangul reading among the syllables (`다 <
阿`, `가 < 伽 < 나`) and Chinese orders Hanzi by pinyin (`丫 < 一`, `唖 <
亜`); both need reading tables om-calc does not have.

**`Max` and `Min` over text** compare character by character with the
same keys as `<`, except that a character whose key is an expansion
(several primary weights) sorts after every character with a single
weight, implicit ideograph weights counting as single; expansions compare
by their number of weights, then the weights. English: `… < z < þ < α < а
< あ < 一 < 阿 < 가 < ꜳ < æ < ǆ < ﬀ < ﬁ < ĳ < ǈ < œ < ß = ẞ < ŉ < ½`; so
`Max ( "æ" ; "z" )` → `æ`. In German the tailored `ä ö ü` are expansions
(`a e` ...): `Max ( "ä" ; "b" )`, `Max ( "b" ; "ä" )` → `ä`, `Min ( "ä" ;
"b" )` → `b` while `"ä" < "b"` → 1; `æ` ties with `ä` (the first is
kept). In Swedish `å ä ö` are single weights after `z` and `æ` an
expansion: `Max ( "æ" ; "ä" )` → `æ`. Korean files make Hangul syllables
expansions too (after `½`; not modelled).

**`SortValues` and `UniqueValues` by locale name.** Sort keys keep only
the low byte of each secondary weight, so `ð` (DUCET 0x159) sorts before
`đ` (0x7D) and `æ` after `ae`. Tailorings (`x < y`: a letter of its own
after the other; `x << y`: an accent variant of it; contractions in
quotes):

| Locale names | Tailoring |
|---|---|
| Spanish | `n < ñ` |
| Spanish_Traditional | `c < "ch"`, `l < "ll"`, `n < ñ` |
| Turkish | `c < ç`, `g < ğ`, `h < ı`, `o < ö`, `s < ş`, `u < ü`; `I` is the capital of `ı`, `İ` of `i` |
| Danish, Norwegian | `y << ü << ű`; after `z`: `æ << ä < ø << ö << ő < å = "aa"` |
| Swedish, Finnish | `v << w`, `y << ü << ű`; after `z`: `å < ä < ö << ő << ø` |
| Swedish_Custom, Finnish_Custom | as Swedish without `v << w` |
| German | `ä`, `ö`, `ü` as `ae`, `oe`, `ue`, before them on a tie |
| Czech | `c < č`, `h < "ch"`, `r < ř`, `s < š`, `z < ž` |
| Slovak | `a < ä`, `c < č`, `h < "ch"`, `o < ô`, `r < ř`, `s < š`, `z < ž` |
| Polish | `a < ą`, `c < ć`, `e < ę`, `n < ń`, `o < ó`, `s < ś`, `z < ź < ż` |
| Croatian | `c < č < ć`, `s < š`, `z < ž` |
| Slovenian | `c < č`, `s < š`, `z < ž` |
| Hungarian | `c < "cs"`, `d < "dz" < "dzs"`, `g < "gy"`, `l < "ly"`, `n < "ny"`, `o < ö << ő`, `s < "sz"`, `t < "ty"`, `u < ü << ű`, `z < "zs"` |
| Estonian | `s < š < z < ž`, `v << w`, after `v`: `õ < ä < ö < ü` |
| Latvian | `c < č`, `ı < y`, `l < ļ`, `n < ņ`, `s < š`, `z < ž` |
| Lithuanian | `c < č`, `ı < y`, `s < š`, `z < ž` |
| Icelandic | `a < á`, `e < é`, `i < í`, `o < ó`, `u < ú`, `y < ý`, after `þ`: `æ << œ < ö << ø` |
| Romanian | `i < î`, `s < ş`, `t < ţ` |
| Catalan | `"ll" << "l·l"` in sort keys only |
| Catalog | `ı` as `i` |
| Ukrainian | `я < ь` |
| Japanese | kanji in JIS X 0208 order |

A tailoring applies to the letter as written: Latvian and Lithuanian move
`y`, not `ý` (which stays with `y`'s accents); Estonian moves `z`, not
`ź`. `German_Dictionary`, Arabic, Bengali, Bulgarian, Chinese_Stroke,
Dutch, English, French, Greek, Gujarati, Hebrew, Hindi, Italian, Kannada,
Malayalam, Marathi, Panjabi, Persian, Portuguese, Russian, Serbian,
Tamil, Telugu and Unicode_Standard sort the test list as English. Not
modelled: Chinese (pinyin), Korean (kana before jamo, syllables after
jamo, Hanja by reading), Thai (Latin `x` sorts first), Vietnamese (tone
order, `ă â đ ê ô ơ ư` as letters) and Greek_Mixed (Latin interleaved
with Greek); Hungarian `UniqueValues` reads `dž` as the `dz` contraction
although its sort keys do not.

**Embedding functions** (`functions/embedding.rs`, calc-functions.md 5.8).
A vector is text holding a JSON array, read strictly like the JSON
functions' documents (numbers, `true`/`false` as 1/0, `null` as 0; `+1`
reads, `.5`, `1e400`, strings, nested arrays, objects, `[]`, trailing
commas and extra text do not), numbers and dates are a vector with no
values, or an embedding file: container data named `*.fve` (any case,
`.fve` alone too) holding `FVE.2100` and the values as little-endian
doubles. Results are text when the first argument is text, `[` the values
`, ` `]` with each value rounded to 20 significant digits and shown as
FileMaker shows numbers in the file's locale but with a leading 0
(`[0.5999999999999999778, 0.80000000000000004441]`,
`[0.000000099999999999999995475, 123456789]`, `[1.0E+21]`, `[0,5, -0,25]`
in de-DE); NaN and infinities show as 0. Otherwise they are a file named
`embf_` plus the first five hex digits (upper case) of the MD5 of its
bytes plus `.fve` (`emb_` for `GetEmbeddingAsFile`: `[1, 2, 3]` →
`emb_03D8B.fve`, `[1]` = `[true]` → `emb_92B81.fve`).

- `GetEmbeddingAsFile ( text {; name} )`: empty text → empty; not a vector
  → `?`, 5; the name cleaned up as the decoders' (`a:b.fve` → `b.fve`,
  `x?.fve` → `x_.fve`, first line only) but empty stays empty (`dir/` → a
  file with no name).
- `GetEmbeddingAsText ( data )`: empty → empty; text → `?`, -1; a header
  other than `FVE.2100` → `?`, 1708; a name not ending in `.fve`, no
  values or a partial double → `?`, 408.
- `NormalizeEmbedding ( data {; dimension} )`: the first `dimension` values
  (the number as a 32-bit integer; 0, negative or more than the size =
  all) divided by their length. A vector whose squared length as a
  32-bit float is in [1, 1.00001) comes back unchanged, whole (text as
  the JSON it was read as, `JSONParsedState` 4): `[1, 1e-5]`, `[1.0000049]`,
  `[0.99999999]` stay, `[1.00000499]`, `[0.99999995]` normalize;
  `NormalizeEmbedding ( "[1, 2]" ; 1 )` → `[1, 2]`. A zero length (also
  after underflow) → `?`; an overflowing one gives zeros.
- `AddEmbeddings`, `SubtractEmbeddings`: an empty first vector → empty, an
  empty second one → the first argument unchanged (`AddEmbeddings ( "abc"
  ; "" )` → `abc`); otherwise both must be vectors of one size; the sum or
  difference is normalized (zero → `?`), in the first argument's form.
- `CosineSimilarity ( v1 ; v2 )`: the plain dot product (`[0.1, 0.2]` and
  `[0.3, 0.4]` → .11000000000000001443), as a number of 20 significant
  digits; `?` for different sizes, empty or invalid vectors and when the
  product rounded to six decimals exceeds 1 in magnitude (`[1.00000045]`
  · `[1]` → 1.0000004499999999297, `[1.0000005]` · `[1]` → `?`); an
  infinite product → 0, NaN → `?`. None of these set an error.
- `GetTokenCount` needs the `cl100k_base` tokenizer, which the server
  engine in the harness does not find: the first call of a run → `?`,
  21012, every later one → 0. It is left to the host, like `GetEmbedding`.

## 14. Transcendental algorithms

This section replaces unknown U9. It specifies how FileMaker computes
`Exp`, `Ln`, `Log`, `Lg`, `Sqrt`, `^`, `FV`, `PMT`, `PV`, `NPV` and the
binary family, and the `FixPt` operations they are built from, closely
enough to reproduce every digit. Two kinds of evidence:

- [O-code] reading `libSupport.so` (`FixPt::Exp` `0x1723c0` and
  `ExpComputer::ComputeExp` `0x171480`, `ExpComputer::ExpComputer`
  `0x1710b0`, `FixPt::Ln` `0x172500`, `Log` `0x173490`, `Lg` `0x173780`,
  `Power` `0x173a70`, `Fv` `0x174da0`, `Pmt` `0x1751f0`, `Pv` `0x175400`,
  `FmAnnuity` `0x174aa0`, `FallFact` `0x1744b0`, `Multiply` `0x169dc0`,
  `Divide` `0x16a7b0`, `LongDivide` `0x16ee70`, `DivideTo` `0x16e630`,
  `operator+=` `0x168240`, `Subtract` `0x16ffb0`, `Round` `0x16c590`,
  `Trim` `0x16cd70`, `AsFloat` `0x16fda0`, `operator=(double)` `0x1670b0`,
  `Mod2Pi` `0x175590`, `ConvertToScientific` `0x16d460`) and the callers in
  `libDBEngine.so` (`DBDataArg::Exp` `0xdf39f0`, `Ln` `0xdf3cd0`, `Log`
  `0xdf3e40`, `Lg` `0xdf3b60`, `Sqrt` `0xdf35a0`, `Power` `0xdf2170`, `Sin`
  `0xdf2820`, `ATan` `0xdf2b30`, `Fv` `0xdfd750`, `Npv` `0xe01a30`,
  `NpvAggregator::DoRepetition` `0xe0b370`);
- [O-run] an executable model written from this section,
  `tools/fixpt_model.py` (Python, integer arithmetic), checked against
  FileMaker Server's engine with `tools/fixpt_battery.py` on 7,504
  expressions (batteries `t01`..`t13` in `tools/fms-calc-harness`): every
  result text and error code matches (14.9), as do the 40 known deviations
  `om-calc`'s tests list.

### 14.1 Numbers as the algorithms see them [O-code]

A number has a sign, integer words, F fraction words (base 10^9), a display
precision p (`mPrecision - mExtraPrecision`) and g guard digits
(`mExtraPrecision`); it holds h = p + g fractional digits. Notation used
below:

- **F follows the precision**: F = ceil(h / 9). Changing p or g only
  changes F: fewer words truncate (the dropped words are lost), more words
  are zero. Digits after position h inside the last word are not cleared by
  a precision change; only *trim* and *round* clear them.
- **set p := q**: p = clamp(q, 16, 800), F follows (`SetPrecision`,
  `BoostPrecision`, copies). **widen(lo, hi, a)**: p = clamp(p, lo, hi) + a
  without the [16, 800] clamp; the routines widen their result by
  (12, 796, 4) (`Exp`, `Ln`, `Log`, `Lg`, `FV`, `PMT`, `PV`, division by an
  integer), (6, 790, 10) (`^`) or (10, 794, 6) (Newton division).
- **trim**: in the word holding digit h, clear the digits after h
  (truncation toward zero). **round(n)**: add 5 at fractional digit n + 1 of
  the magnitude (half away from zero), clear every digit after n, then trim;
  when the words hold no digit after n nothing changes.
- **Like(v, y)**: the integer v at p = clamp(p_y, 16, 800) and g = g_y (2 if
  y has none). A fresh number is p 16, g 2.
- **Zero test** (`IsZero`): zero in every word above the last fraction word
  and in the first (h mod 9, or 9) digits of the last word.

Guard digits depend on what produced the number [O-code, O-run]: literals,
text and ordinary results have g = 2; `Exp` and `Lg` results 4, `Ln` 3,
`Log` 5; `^`, `Sqrt`, `FV`, `PMT`, `PV` take the first argument's (base,
payment). Arithmetic keeps the largest (`Exp(1)*10^18` →
`2718281828459045235.36`, `(Exp(1)-1)*10^18` →
`1718281828459045235.36`, `Log(2)*Log(2)*10^18` → `90619058289456537.191`),
`/` the dividend's (`Exp(1)/3*10^18` → `906093942819681745.12`), and so do
`Abs`, `Round`, unary minus, `GetAsNumber`, `Let`, `If`, `Max`, `Sum`,
`Mod`; `SetPrecision` drops them (`SetPrecision(Exp(1);16)*10^18` →
`2718281828459045200`). Display and comparison ignore them.

Constants [O-code; digits checked against the mathematical values]:

| Constant | Table | Fractional digits | Last digit |
|---|---|---|---|
| ln 10 | `Ln10` | 801 (89 words) | truncated |
| 1 / ln 10 | `Ln10Inv` | 801 | truncated |
| ln 2 | `Ln2` | 1,953 (217 words) | truncated |
| 1 / ln 2 | `Ln2Inv` | 801 | rounded |
| π | `PiArray` | 720 (80 words) | rounded |

A **table constant at the precision of y** is Like(0, y) with every one of
its F fraction words filled from the table: the constant truncated to 9 F
fractional digits (more than h, not trimmed). `Exp` instead uses two fixed
copies built once (`ExpComputer`): ln 10 and 1/ln 10 truncated to 45
fractional digits (5 words, p 45, g 2).

### 14.2 Primitive operations [O-code, O-run]

**Comparison** (`<`, `=`, used inside the algorithms and by the operators):
operands of different signs compare by sign (zero counts as positive);
otherwise both are rounded at D = clamp(max(p_a, p_b), 16, 800) and the
results compared.

**Addition** a += b (also `+` and `-`; the three-operand forms copy a into
the result first):

- same signs: the exact sum; p = clamp(max(p_a, p_b), 16, 800),
  g = max(g_a, g_b); not trimmed;
- opposite signs: |a| and |b| are compared as above at D. Equal there →
  the result is exactly 0. Otherwise the exact difference of the larger
  magnitude minus the smaller, with the larger's sign, at the larger
  precision, trimmed. So `(1/3 - .3333333333333333)*10^20`,
  `((1/3)*3 - 1)*10^20`, `(1/3 - .33333333333333333)*10^20` and
  `(Exp(1) - 2.718281828459045235)*10^22` are all 0 [O-run, battery
  `t13-arith`], although the held digits differ.
- **subtract(a, b, out)** first gives out the precision max(p_a, p_b,
  previous p_out) and g max(g_a, g_b), then computes -b + a as above.

**Multiplication** a × b into out: g = max(g_a, g_b, g_out); p =
clamp(max(p_a, p_b, p_out), 16, 800); R = ceil((p + g) / 9). For each word
b_j of the **right** operand b, the row b_j × a is formed exactly (every
word of a, carries along the row) at b_j's position, and the row's words
after fraction word R are dropped; the rows are summed exactly and the sum
trimmed. An operand of 90 or more integer words (10^801) gives `?`. The
product therefore depends on the order of the operands in its last held
digit, and carries from dropped words reach word R [O-run, `t13-arith`]:
`(1/3)*(2/3)*10^18` → `222222222222222221`, `(2/3)*(1/3)*10^18` →
`222222222222222222`; `(2/3)*(1/7)*10^18` → `95238095238095237` (a short
product keeping only the high word of the products just past word R gives
`…236`). This settles U8. At the operator `a * b` the result starts as a
fresh number at the evaluation precision (p_eval, g 2) [I; consistent with
every vector].

**Division by a machine integer** n (`Divide(Digit)`, `DivideTo`): the
result keeps its own p and g; the quotient is formed truncated in
widen(12, 796, 4) words and rounded at h, which equals the exact quotient
rounded half away from zero at h. n = 0 → `?`, error 15.

**Division** a / b into out (`Divide`): b = 0 → `?` 15; a = 0 → 0; an
operand of 90 or more integer words → `?` 14. Otherwise by b's non-zero
words:

1. They span one word: the same long division by that word (shifted by its
   word position), rounded at out's h: the correctly rounded quotient.
2. They span two words that fit in one nine-digit divisor (the top word
   below 10^(9-k) and the low word divisible by 10^(9-k), smallest such
   k = 1..8): the same with the merged divisor, out k digits wider, the
   result shifted back. Also correctly rounded.
3. Otherwise **Newton division** (`LongDivide`), with P = p_out and
   G = g_out:
   1. out widened (10, 794, 6), so it holds H = clamp(P, 10, 794) + 6 + G.
   2. d = double(b) (14.8); when b has more than 200 integer digits D_b,
      d = double(b / 10^D_b) on a copy widened by D_b digits.
   3. D_a = integer digits of a (0 when |a| < 1); Wd = clamp(D_a + H,
      16, 800). Every temporary is p Wd, g 2.
   4. y = 1 / d (a C double division), assigned through `%.20g` (14.8); then
      divided by 10^D_b inside its words when b was scaled.
   5. ε = 10^-(D_a + H - 4), held at p Wd (0 when that is below its words).
   6. Up to D_a + H + 200 steps: t = b × y, u = subtract(1, t), c = y × u,
      y' = y + c, δ = |subtract(y', y)|, y = y'; stop after a step whose δ < ε
      (comparison of 14.2); a δ of 400 or more integer words → `?` 14.
   7. out = a × y (multiplication rule; out's widened precision is the
      floor), rounded at P + g_out (g after the product), p := P.

   The quotient is a × (1/b) good to about six digits beyond h, so it
   agrees with the correctly rounded quotient except within about 10^-6
   units of a rounding tie; the model reproduces the steps.

At the operator `a / b` the result is a fresh number at max(p_eval, p_a)
with the dividend's guard digits (13; `Exp(1)/3` above).

**Shifts**: × 10^k is exact; ÷ 10^k moves digits inside the existing
words (digits past the last word are lost). **Floor division**
(`FixPt::Div`): the quotient by the rule above at out's precision, then
floored. `Mod(x; y)` = x - y × Div(x; y) with that rule.

### 14.3 Exp [O-code, O-run]

`Exp(x)` evaluates into a result of p = p_x and g = 4 (`DBDataArg::Exp`).
Inside `^`, `FV` and the others the same routine runs with the caller's p
and g. With P, G the result's precision:

1. x `?` → `?` (no error). x ≤ -5000 → 0. x > 5000 → `?`, error 14.
   (Compared with 5000 and -5000 at p 16 by 14.2.)
2. result := 1, widened (12, 796, 4): Q = clamp(P, 12, 796) + 4.
   a := |x| (x's precision).
3. If x is not zero:
   1. k := the product c45 × a (c45 = 1/ln 10 to 45 digits; the
      multiplication rule into a fresh number, so at p max(45, p_x)),
      rounded half away to an integer.
   2. W := clamp(Q + k, 16, 800); the result's p := W; a's p := W, then
      a's g := G.
   3. r := subtract(a, k × l45), with l45 = ln 10 to 45 digits. The product
      k × l45 goes into a number of p W, g G, so it is exact and sits at
      p_s = max(45, p_x, W), g_s = max(G, g_x, 2); r is exact at that
      precision. The truncation of l45 (6.3 × 10^-46 per unit of k) is the
      main error of `Exp`: `Exp(100)` (k = 43) holds
      `…873611119.50053455094514358664` where the exact value is
      `…873611118.77374192241519160862`, 0.7268 too high.
   4. Taylor series: term := r; for n = 2, 3, …: result += term;
      q := r / n (division by a machine integer into the number that held
      k × l45, so rounded at p_s + g_s); term := term × q (multiplication
      rule, rows over q, at p_s); stop as soon as term is zero (no cap).
      The result takes p_s by the addition rule.
   5. result := result × 10^k.
4. If x < 0: t := a number of p max(p_s, p_result) and g max(g_s,
   g_result); t := 1 / result (division rule; e^|x| spans many words, so
   Newton division). When e^|x| has 90 or more integer words (x below
   -801 ln 10 ≈ -1844.3707) the division fails: error 14, result `?`;
   x from -5000 to there is error 14, at -5000 and below it is 0.
   Otherwise result := t.
5. round at P + g_result; p := P.

Accuracy follows from this [O-run]. With k = 0 (|x| below ln 10 / 2 ≈
1.1513) every held digit is right at any precision (`Exp(1)` holds
`2.71828182845904523536`, `Exp(-1)` `.3678794411714423216`,
`SetPrecision(Exp(1);400)` is right to all 400 digits). With k ≥ 1 the
result is right to about 45 significant digits whatever p is, the error of
l45 times k: at p 16 that covers the 20 held digits up to x ≈ 53, but at p
400 every `Exp` with |x| ≥ 1.1513 is right to only 44 to 46 significant
digits (battery `t01`, p 400: `Exp(10)` 45, `Exp(-120)` 44). W stops at
800, so `Exp(5000)` has 804 significant digits followed by zeros (its
display, 14.8).

### 14.4 Ln, Log, Lg [O-code, O-run]

`Ln(x)` evaluates into a result of p = p_x, g = 3; `Log` g = 5; `Lg` g = 4
(`DBDataArg::Ln`, `Log`, `Lg`). `Ln` with the result's P and G:

1. x `?` → `?` (no error); x zero or negative → `?`, error 14.
2. result := 0, widened (12, 796, 4): Q = clamp(P, 12, 796) + 4.
   u := x with p := clamp(Q, 16, 800), then g := G (Q + G digits).
3. Decimal scaling s10: if u ≥ 2, s10 := its number of integer digits and
   u := u ÷ 10^s10 (inside u's words); if u < 1 and its first non-zero
   fractional digit is at position f > 1, u := u × 10^(f-1) and
   s10 := -(f-1). Otherwise s10 = 0. Now 0.1 ≤ u < 2.
4. Binary scaling s2 into [0.6, 1.4]. The bounds come from doubles through
   `%.20g` (14.8): hi = `1.3999999999999999112`, lo =
   `.5999999999999999778` (both p 19). Comparisons by 14.2 at u's p:
   - if u ≤ hi: while u < lo: u := u + u, s2 := s2 - 1;
   - otherwise repeat u := u / 2 (division by a machine integer into a
     number of p Q, g G), s2 := s2 + 1, until u ≤ hi.
5. z := u - 1 (addition rule: exactly 0 when u equals 1 at p Q).
6. Series ln(1 + z) = z - z²/2 + z³/3 - …: num := z; for n = 1, 2, …:
   t := num / n (division by a machine integer into a number of p Q, g G);
   result += t for odd n, result += -t for even n; num := num × z
   (multiplication rule); stop after adding a t that is zero, or after
   2Q + 200 terms. (The cap cuts the series only at p near 400 with |z|
   near 0.4, about a tenth of a unit of the last held digit; not visible
   in the batteries [O-code].)
7. If s10 ≠ 0: result += ln 10 × s10, the table constant at the result's
   precision (p Q, g G) times the integer s10 by the multiplication rule.
   If s2 ≠ 0: result += ln 2 × s2 likewise.
8. round at P + G; p := P.

`Log` and `Lg` with P, G (5 or 4): result widened (12, 796, 4) to Q;
λ := `Ln` of x into Like(0, result), i.e. `Ln` with p Q and g G (it works
at Q + 4 and rounds at Q + G); on an error the result is `?` with it;
otherwise result := C × λ with C = 1/ln 10 or 1/ln 2 as a table constant
at the result's precision (multiplication rule, rows over λ); round at
P + G; p := P. Held digits at p 16: `Ln(2)` `.6931471805599453094`,
`Log(2)` `.301029995663981195214`, `Lg(3)` `1.58496250072115618145`.

### 14.5 `^` and Sqrt [O-code, O-run]

`x ^ y` evaluates into a result of the base's p and g (`DBDataArg::Power`);
`Sqrt(x)` is `x ^ 0.5` with 0.5 assigned from a double (p 16). With P, G:

1. x or y `?` → `?` (no error). |x| or |y| of 46 or more integer words
   (10^405) → `?`, error 14.
2. result := 1, widened (6, 790, 10): R = clamp(P, 6, 790) + 10.
3. x < 0 and y with any non-zero fractional digit → `?`, error 14
   (`(-8)^(1/3)`).
4. Split y into i (integer part, toward zero) and f = y - i (y's sign),
   both at y's precision.
5. If i ≠ 0, binary exponentiation from the low bit: s := x with p := R
   (clamped to [16, 800]) then g := G (digits of x past those words are
   dropped); acc := result; n := |i|. Loop:
   - n = 0: if y < 0, result := 1 / result (division rule, both operands
     Like(1 or 0, result)); leave the loop.
   - n odd: result := acc × s (multiplication rule, rows over s);
     acc := result; 46 or more integer words → overflow.
   - n := floor(n / 2); s := s × s (multiplication rule at p R); when n is
     not 0 and s has 46 or more integer words → overflow.

   Overflow: y > 0 → `?`, error 14 (`10^405`, `2^1350`); y < 0 → 0
   without error and step 6 is skipped (`2^-1350`, `1.5^-1000`).
6. If f ≠ 0: c := a copy of result (p R, g G); x = 0 gives c := 0
   (`0^.5` and `0^-.5` are 0); otherwise λ := `Ln` of x into Like(0, c),
   m := f × λ (multiplication rule into Like(0, c)), c := `Exp` of m with
   c's p and g (14.3, so G guard digits, not 4); result := result × c
   (multiplication rule, rows over c).
7. round at P + G; p := P.

Consequences: every product is truncated at R + G digits, so an integer
power is exact only while the exact power fits (`3^40`, `7^25`, `10^404`
are exact; `1.5^100` holds `406561177535215237.397279707567041643`, exact
`…397279707567041671…`; `1.5^360` about 37 correct digits); a negative
integer exponent
divides 1 by that power at R + G digits (`0.1^-404` → 15 because `0.1^404`
is 0 there; `(-2)^-3` → `-.125`); a fractional exponent multiplies the
integer power by e^(f ln x) at p R (`10^15.5` = 10^15 × e^(0.5 ln 10),
29 correct digits; `Sqrt(10^30)` = `999999999999999.9999999999999345`,
from `Ln(10^30)` held at R + G = 28 fractional digits). Through `Exp` a
fractional power also inherits its 45-digit limit once f · ln x reaches
1.1513: `SetPrecision(Sqrt(8164587);400)`, `SetPrecision(Sqrt(10^30);400)`
and most `Sqrt` results at p 400 are right to 43 to 46 significant digits,
while `SetPrecision(Sqrt(2);400)` and `SetPrecision(Sqrt(10);400)` (where
k rounds to 0) are right throughout (`t05`, `t07`). `Ln`, `Log`
and `Lg` have no such limit (right to all digits at p 400). `0^0` = 1,
`0^-1` → 15.

### 14.6 FV, PMT, PV, NPV [O-code, O-run]

`FV(pmt; r; n)`, `PMT` and `PV` evaluate into a result of the payment's p
and g (`DBDataArg::Fv`, `Pmt`, `Pv`); with P, G, the result is widened
(12, 796, 4) to Q:

- **FV**: r zero (or empty) → result := pmt × n. Otherwise b := r + 1;
  f := `^` of b and n into Like(0, result) (so `^` with P = Q: it works at
  Q + 10); t := subtract(f, 1) into Like(0, result); t := pmt × t into
  Like(0, result); result := t / r (division rule at p Q, g G).
- **Annuity** A(r, n) into Like(0, result): r zero → A := n (a copy of n).
  Otherwise b := r + 1; f := `^` of b and -n into Like(0, A);
  t := subtract(1, f) into a fresh number; A := t / r.
  **PMT**: result := pmt / A. **PV**: result := pmt × A.
- round at P + G; p := P. Errors come from the steps: `PMT(100;.1;0)` and
  `PMT(100;-1;10)` → 15, `PV(100;-.5;500)` → 15 (`0.5^500` is 0),
  `FV(100;-1.5;2.5)` → 14, `FV(100;1;2000)` → 14 while `PV(100;1;2000)` →
  100.
- **NPV(field; r)** aggregates the field's values (8.6, 8.7) in order:
  base := r + 1 (r's precision), k := 0, sum := 0 (p 16, g 2). For each
  value v (empty and digitless values are skipped; `?` makes the result
  `?`): k := k + 1; w := `^` of base and k into a number of the payment
  argument's p and g; term := v / w (division rule, same precision);
  sum += term. The result is the sum. The terms are rounded at p + 2
  fractional digits after the power was, so payments with many integer
  digits keep about p + 2 + their integer digits significant digits
  (`NPV(Rel::DueDate;1/3)` in calc-functions.md).

The `w = p + 6` description in calc-functions.md's financial answers is
this with G = 2: Q + G = p + 6.

### 14.7 Factorial [O-code]

`FixPt::FallFact(n, c)` multiplies n (n - 1) … (n - c + 1) exactly; n and c
must be non-negative integers (any fractional digit → 14) with c ≤ n, and a
product of 46 or more integer words is error 14. Nothing in it is
inexact; `Factorial` and `Combination` are described in calc-functions.md.

### 14.8 The binary family and number display [O-code, O-run]

`Sin`, `Cos`, `Tan`, `Atan`, `Asin`, `Acos`, `Atan2` (`DBDataArg::Sin`
and siblings):

1. The argument's p is saved and the argument set to p 16 (`SetPrecision`:
   whole words beyond ceil((16 + g) / 9) are dropped, so with g = 2 it
   keeps 18 fractional digits, truncated). `Sin(.0000000000000000015)`
   (p 19) is `Sin(1.0E-18)`, and `Sin(1E-19)`, `Sin(1E-20)` are 0 because
   the argument became 0. `Asin` and `Acos` first check the domain on the
   double of the full argument: outside [-1, 1] → error 1218.
2. `Sin`, `Cos`, `Tan` reduce arguments outside [-1000, 1000]
   (`Mod2Pi`): τ := 2 × π, π the table constant at the argument's
   precision rounded at its held digits (18 digits at p 16); the argument
   becomes `Mod(x; τ)` (14.2: the quotient at p 16, g 2 rounded at 18
   digits, floored). `Atan`, `Asin`, `Acos` and `Atan2` do not reduce.
3. **Decimal to double** (`AsFloat`): d := 0; for the fraction words from
   the last to the first, d := (d + word) / 1e9; then for the integer words
   from the lowest, d := d + word × s with s = 1, 1e9, 1e18, … (s
   multiplied by 1e9 after each word, +∞ from word 35 on); negate.
   Every operation is an IEEE double operation, so `Sin(Exp(1))` sees a
   double one unit off the correctly rounded one. Integer word 35 or above
   being zero gives 0 × ∞ = NaN: `Atan(10^324)`, `Atan(10^324 + 1)`,
   `Atan(10^330)`, `Atan(10^400)` → 0, while `Atan(10^323)`,
   `Atan(10^324 + 10^315)` and `Atan(10^333 + 10^324 + 10^315)` (no zero
   word from 35 up) → `1.5707963267948966` [O-run, `t11-trig`].
4. The C library function. FileMaker Server for Ubuntu 24.04 calls glibc
   2.39 (`sin`, `cos`, `tan`, `atan`, `asin`, `acos`, `atan2`), which is
   not correctly rounded in every case: `Sin(1529)` reduces to the double
   `0x1.17cde069263eap+1`, whose sine is 1.8 × 10^-5 units below a rounding
   tie; glibc returns the upper neighbour `0.81667302726891533027`
   (correctly rounded: `…521925`), hence `.8166730272689153`. Matching
   every digit needs glibc's algorithms or results [O-run:
   `tools/fms-calc-harness/libm.c` run in the harness image]. FileMaker Pro
   on macOS uses Apple's libm, whose results differ in such cases [I].
5. **Double to decimal** (`operator=(double)`): a NaN result becomes 0
   (the `%.20g` text `nan` has no digits) [O-run]; an infinite one would
   too [I; no argument reaches it]. Otherwise the `%.20g` text is parsed
   into the number: p rises to the number of fractional digits written
   (never falls), g 2, then p is set back to the saved argument p (words
   follow, so ceil((p + 2) / 9) words remain): at p 16 the result keeps 18
   fractional digits, truncated (`Sin(1)*10^18` → `841470984807896504`),
   at p 18 the 20 digits of `.84147098480789650488`.

**Scientific display** (2.7) holds the mantissa in clamp(e + p, 16, 800)
digits after removing the guard digits (`GetText` rounds at p and drops
them, `ConvertToScientific` shifts by the exponent e inside whole words),
so when e + p exceeds 800 the mantissa keeps 801 fractional digits,
truncated: `Exp(1900)` (e = 825) shows 802 significant digits of its 805.
Plain display (`Exp(1850)`, low integer digits not zero) shows them all.

### 14.9 Validation [O-run]

`python3 -I tools/fixpt_battery.py gen <dir>` writes the batteries
(deterministic, seed 20261028); each line is `SetPrecision ( f ; p )`
(the displayed result) and `SetPrecision ( f * 10^6 ; p )` (all held
digits), p = 16, 20, 30, 50, 100, 400, arguments drawn over each function's
range (tiny to 10^700, integer and fractional powers, negative bases and
exponents, overflow, the 1.4/0.6 and 5000/-1844 limits, guard digits of
nested calls). `check` evaluates every expression with the model; `t11`
and `t12` use glibc's results (`libm-calls`, then
`tools/fms-calc-harness/libm.c` in the harness image, output
`results/t11-t12-glibc.txt`). Results, 2026-10-29 (`results/t-summary.txt`):

| Function | p=16 | p=20 | p=30 | p=50 | p=100 | p=400 | all |
|---|---|---|---|---|---|---|---|
| Exp | 180/180 | 139/139 | 180/180 | 181/181 | 139/139 | 121/121 | 940/940 |
| Ln | 166/166 | 126/126 | 166/166 | 166/166 | 126/126 | 106/106 | 856/856 |
| Log | 128/128 | 126/126 | 128/128 | 128/128 | 126/126 | 68/68 | 704/704 |
| Lg | 128/128 | 126/126 | 128/128 | 128/128 | 126/126 | 68/68 | 704/704 |
| Sqrt | 120/120 | 118/118 | 120/120 | 120/120 | 118/118 | 60/60 | 656/656 |
| `^` integer exponent | 120/120 | 124/124 | 116/116 | 116/116 | 118/118 | 52/52 | 646/646 |
| `^` fractional exponent | 166/166 | 96/96 | 170/170 | 170/170 | 102/102 | 114/114 | 818/818 |
| FV | 42/42 | 34/34 | 54/54 | 42/42 | 34/34 | 20/20 | 226/226 |
| PMT | 44/44 | 26/26 | 38/38 | 40/40 | 24/24 | 12/12 | 184/184 |
| PV | 44/44 | 30/30 | 38/38 | 48/48 | 32/32 | 8/8 | 200/200 |
| NPV (12 related values) | 40/40 | 40/40 | 40/40 | 40/40 | 40/40 | 40/40 | 240/240 |
| Sin, Cos, Tan, Atan | 1,276/1,276 | | 26/26 | | | | 1,302/1,302 |
| arithmetic probes | 28/28 | | | | | | 28/28 |
| total | | | | | | | 7,504/7,504 |

(The `^ fractional` row also counts the edge battery's other `^` lines.)
The deviations `om-calc`'s tests list all reproduce (`results/t-known.tsv`,
40 of 40): the 36 of `vectors.rs`, `fn_number.rs` and `fn_financial.rs`
(`Exp(1)*10^18`, `Exp(1400)`..`Exp(2900)`, `Sqrt(10^30)`,
`Sqrt(2*10^400)`, `10^15.5`, `2^200.5`, `FV(1;1/3;360)*10^18`,
`PMT(12345.67;.11/12;7.5)*10^18`, …) and the four of `fn_trig.rs`
(`Sin(1E-20)`, `Sin(1529)`, `Atan(10^400)`, `Atan(-10^400)`). Of the en-US rows of `calc-value-vectors.tsv` the
model's evaluator can parse (arithmetic and these functions), 409 of 413
match; the four others are evaluator gaps outside this section (the `5.`
literal error, `SetPrecision` of an integral result).

### 14.10 For implementers

- A number needs its guard-digit count g (2, 3, 4 or 5) along with p, kept
  as 14.1 says; display and comparison use p only.
- Multiplication follows 14.2 (rows over the right operand), not the short
  product of 2.3; opposite-sign addition of values equal at the larger
  display precision is 0.
- Division by divisors of more than nine significant digits is Newton
  division; implementing it as specified reproduces FileMaker near ties.
- The decimal functions need the constants of 14.1 (ln 10 to 801 digits,
  ln 2 to 1,953, the 45-digit copies for `Exp`); `tools/fixpt_model.py`
  computes them from their definitions.
- The binary family needs glibc's results for exact agreement with the
  server; `tools/fixpt_battery.py libm-calls` lists the doubles the
  batteries pass to the C library.

### Implementation notes (section 14)

`om-calc`'s number core (`crates/om-calc/src/number/`: `fixpt.rs` holds the
word form and the primitive operations of 14.1 and 14.2, `transcend.rs` and
`finance.rs` the algorithms of 14.3 to 14.8, `consts.rs` the tables)
follows this section; `tests/fn_transcend.rs` runs every line of `t01` to
`t13` (7,504) and requires all but the three `Sin(1529)` lines. What it
settled beyond the text above, with battery `s14-probes` (FileMaker Server
26.0.3.309, 2026-10-29, results in
`tools/fms-calc-harness/results/s14-probes.contacts-en.tsv`):

- Guard digits by operation [O-run]: `Int`, `Truncate`, `Round`, `Sign`,
  `Abs`, unary minus, `GetAsNumber`, `Mod`, `Max`, `Sum`, `Let`, `If` and
  `Case` keep a value's guard digits; `Floor`, `Ceiling`, `Div` and
  `SetPrecision` give 2. `Int(Exp(1))/3*10^18` → `666666666666666666.67`,
  `Truncate(Exp(1);16)/3*10^18` → `906093942819681733.33`,
  `Sign(Exp(1))/3*10^18` → `333333333333333333.33`, but
  `Floor(Exp(1))/3*10^18` and `Div(Exp(1);1)/3*10^18` →
  `666666666666666667` and `SetPrecision(Exp(1);16)/3*10^18` →
  `906093942819681733`.
- The binary family's result keeps the argument's guard digits, not 2
  (14.8 step 5): `Sin(Exp(1))*10^18` → `410781290502908402.74`.
- Magnitude limits of the operators [O-run]: `*` limits only its operands
  (90 or more integer words give `?` without an error), not the product:
  `(10^400*10^400)*(10^400*10^400)` → `1.0E+1600`,
  `(10^400*10^400*10^400)*1` → `?`. `/` with such an operand is error 14
  (`(10^400*10^400*10)/7`, `1/(10^400*10^400*10^400)`), with 89 words it
  works (`(10^400*10^400)/7`). `+` and `-` have no limit
  (`9*10^400*10^400*10^400+9*10^400*10^400*10^400` → `1.8E+1201`).
- `+` compares opposite-sign operands at the evaluation precision as well
  [O-run]: `Let(x=1/3;SetPrecision(x + -.3333333333333333;20))*10^25` →
  `330000000`, not 0 (x and the literal have p 16). The implementation
  gives every operand and function argument the evaluation precision when
  its own is lower before applying the rules above, as the model's
  literals carry it; a value's own precision still never falls
  (`SetPrecision(1;20)/3` keeps 16, 2.6).
- `Div` floors the quotient after rounding it to the display precision,
  `Mod` floors its held digits [O-run]: `Div(-1;1/3)` → -3 and
  `Div(2;2/3)` → 3, while `Mod(-1;1/3)*10^18` → `333333333333333332`
  (quotient -3.000000000000000003 floored to -4). The product y × q is
  exact and the difference follows `-`, so `Mod(2/3;1/3)` and
  `Mod(1;1/3)` are 0.
- `^` with a negative exponent whose integer power is 0 at the working
  digits [O-run, `t07`]: the reciprocal's error 15 stands only when no
  fractional part follows; when one does, the outcome of its `Ln` and
  `Exp` replaces the error and the result is the zero power times
  e^(f ln x): `.037828396891^-32.5555388` → 0 without an error, while
  `0.1^-404` → 15 and `0^-1.5` → 15 (`Ln` is not run for x = 0).
- `Asin` and `Acos` treat a NaN double (an argument beyond 10^324, 14.8
  step 3) as outside [-1, 1]: `Asin(10^400)` → 1218.
- `NPV` gives each term the payment's own precision and guard digits (the
  evaluation precision when that is larger), so the model's `p_eval, 2`
  for integer payments.
- The tables of 14.1 are computed on first use from the series in
  `consts.rs` (40 digits beyond the longest table, cut only where the 30
  digits after the cut are neither all 0 nor all 9 nor at a rounding tie);
  every digit agrees with Python's `Decimal` at 2,100 digits. Generating
  them takes about 50 ms in a debug build.
- The trigonometric value is the correctly rounded one; of the 1,302 lines
  of `t11` and `t12` only `Sin(1529)` (three result lines) differs from
  glibc 2.39's (14.8 step 4).
- Timings at p 400 (debug / release, Apple silicon): `Exp(1)` 57 / 12 ms
  including the tables, `Exp(5000)` 21 / 2.7 ms, `Exp(-1800)` 51 / 6.8
  ms, `Ln(7)` 4 / 0.6 ms, `Sqrt(8164587)` 8 / 1 ms, `FV(1;1/3;360.5)`
  22 / 2.5 ms; `tests/fn_transcend.rs` runs the 7,504 lines in 7 s debug,
  1 s release.

### Oracle answers (collation gaps)

Settled with `tools/fms-calc-harness` (FileMaker Server 26.0.3.309,
2026-10-29) [O-run]: the Contacts templates en, ja, ko and zh, and the
locale names of `SortValues` and `UniqueValues` in contacts-en. Batteries
`vc-cjk-order` and `vc-cjk-stroke` (every BMP character and one character
per high surrogate of U+20000..U+2B7FF, sorted in ascending and in
descending input order, so that ties show), `vc-cjk`, `vc-sortnames-chars`
and `vc-surrogates`; fixtures in `crates/om-calc/tests/data/`, checked by
`tests/value_core.rs`; implementation in `crates/om-calc/src/text/`
(`cjk.rs`, `sorting.rs`, `collate.rs`) and `om-collate`. With these,
`SortValues` over every BMP character matches FileMaker exactly in English,
Chinese and Korean files and under every locale name but Japanese (kana),
Catalog (8 characters) and Vietnamese (3 marks). Note for batteries:
`UniqueValues ( list ; "Name" )` reads the name as the data type and uses
the file's language; the locale is the third argument.

**Computed weights (every language).** FileMaker orders the ideographs
and Hangul it computes weights for after all other letters, in blocks:
U+4E00..U+9FCB < Extension A U+3400..U+4DB5 < U+20000..U+2A6DF <
U+2A700..U+2B7FF (unassigned code points of these blocks included) <
Hangul syllables < what DUCET lists with ideograph weights (Kangxi and
CJK radicals, enclosed and parenthesized ideographs, the twelve unified
compatibility ideographs `﨎`...) < unassigned code points (U+4DB6...,
U+9FCC..., U+FA2E..., then U+2B800..., U+30000...). Compatibility
ideographs of Unicode 5.2 sort as their canonical decomposition (`"豈" =
"豈"`); U+FA2E, U+FA2F (6.1) are unassigned code points. `Max`/`Min`
count every key of several weights as an expansion: a supplementary
ideograph has two (`Max ( "𠀋" ; "ß" )` → `𠀋`, `Max ( "𠀋" ; "½" )` → `½`),
DUCET's ideograph pairs and unassigned code points too (`Max ( "㊂" ;
"𨈌" )` → `㊂`); U+4E00.., Extension A and Hangul syllables have one
(`Max ( "一" ; "ß" )` → `ß`).

**Chinese (Pinyin)** files and the name `Chinese` order 27,508
ideographs (main block and Extension A) by pinyin with tones, then stroke
count, then an order of FileMaker's own (`丫 < 一 < 衣`, `唖 < 亜`): CLDR's
pinyin collations (1.3 to 24) and Unihan `kMandarin`/`kTotalStrokes`
(5.2.0, 18.0.0) do not reproduce it, so om-calc carries the observed order
(`data/zh-han.bin`, 56 KB). The Unicode 5.1/5.2 additions U+9FBC..U+9FCB and
U+FA2E, U+FA2F, U+FA6E, U+FA6F come first (`龼 < 吖`); U+9FA6..U+9FBB, the
unlisted Extension A ideographs and the supplementary ones follow the
table by computed weight. Listed ideographs never tie (compatibility
ideographs tie with theirs); `UniqueValues` keeps them apart. The name
`Chinese_Stroke` (in any file) orders 27,967 ideographs by stroke count,
then stroke types (`一 丨 乙 二 十 丁 人 ... 龜 齉`), again matched by neither
CLDR's stroke collations nor `kTotalStrokes` (`data/zh-stroke.bin`).

**Korean** files and the name `Korean` place, after every other letter,
a table of FileMaker's (`data/ko-hangul.bin`, 59 KB): compatibility forms
first (full width punctuation and small letters, squared units, circled
numbers, half width and circled katakana, CJK strokes U+31C0..U+31CF), then
per initial consonant its jamo (`ᄀ` = `ㄱ` = `ﾡ`), the 588 syllables that
start with it, its final jamo (`ᆨ`), its enclosed forms (`㈀ ㉠ ㈎ ㉮`) and
the Hanja read with it (CLDR's KS X 1001 Hanja of each reading first, in
CLDR's order, then about 20,000 more by code point, many without a reading
in Unihan), then archaic jamo, the vowels and the filler. So `가 < 각 <
깋 < ᆨ < 伽 < 까`, `다 < 阿`. A syllable has its jamo's weights and, without a
final, the lowest Korean weight: `"각" = Char ( 4352 ) & Char ( 4449 ) &
Char ( 4520 )` → 1, `"가" = Char ( 4352 ) & Char ( 4449 )` → 0 and the jamo
sort first, `UniqueValues` merges both (index keys leave the filler out);
syllables are expansions for `Max` (`Max ( "가" ; "½" )` → `가`). Full width
small letters are letters of their own (`"ａ" = "a"` → 0) while `"Ａ" = "A"`
→ 1. Extension A and the supplementary ideographs take table weights by
code point (per high surrogate), so some equal a Hanja.

**Japanese** files and the name `Japanese` rank U+4E00..U+9FA5: `仝`, then
the JIS X 0208 kanji and the IBM extension kanji of code page 932 by code
(as EUC-JP encodes them), then the rest by code point. Extension A (rank =
code point - 0x2C33), U+9FA6..U+9FCB (- 0x9BFE), the Hangul syllables
(- 0x8251) and the supplementary ideographs (first weight = high surrogate
- 0xB2BE, then the code point) take ranks of that sequence, so `"填" =
"㐀"`, `"暐" = "가"`, `"甑" = Char ( 40870 )` → 1, `UniqueValues` merges
them, and `𠀋 < 龥`. Circled ideographs and ideographic annotation marks
sort as their kanji (`"㊀" = "一"`; their third-level weight counts in
`SortValues`), radicals do not. Third level: circled katakana after
katakana (`か < ｶ < カ < ㋕`); the kana order is applied to kana only (small
punctuation keeps DUCET's). Index keys tell small kana and katakana apart
for kana only (half width Hangul letters share those tertiary weights).
Not modelled: small kana sort after longer sequences that start with the
same kana (`㌃` before `ァ`), `ん`, `ン` and `ヿ` sit after bopomofo right
before the kanji (`〆` too), and the voiced marks have an order of their
own.

**Named locales.** Over the 16,478 BMP characters outside the ideograph
and Hangul blocks, and the `vc-sortnames` list:

- Sort keys keep one byte per second-level weight, the weight less 0x1F:
  `ð` (0x159) sorts before `đ` (0x7D) as before, and the marks weighing
  0x100..0x11E (Balinese, Javanese, Sundanese, Lao signs...) after all
  others (U+0EC9, 0x11F, before U+0EC8, 0x11E).
- A tailored letter comes after its base letter and everything that starts
  with the base's weight (Czech `č` after `㏇`, `ž` after `ƍ`); Latvian,
  Lithuanian and Estonian put theirs after the whole DUCET family of the
  base letter (Latvian `č` after `ꜿ`, `ģ` after `ƣ`, `y` after `ᵼ`, `ķ` after
  `ʞ`, `ļ` after `ʎ`, `ņ` after `ŋ`, `ŗ` after `ꝝ`, `š` after `ʆ`, `ž` after
  `ꝣ`; Lithuanian `č` only in small letters; Estonian `š z ž` after `ʆ`, `õ`
  with the weight of `ʍ`, then `ä ö ü`, and `w` after every accented `v`).
  Latvian and Lithuanian index keys give the letters placed after a
  Unicode 5.1 letter that letter's key (Latvian `č` = `ꜿ`, `y` = `ᵼ`, `ŗ` =
  `ꝝ`, `ž` = `ꝣ`).
- Letters om-calc lacked: Romanian `ă` (after `a`) and `ţ` (after `ꜩ`),
  Latvian `ģ ķ ŗ`, Croatian `ǉ` (after `ʫ`) and `ǌ`, Danish `ǽ ǿ ǻ` and
  Cyrillic `ӕ` (with `æ`, also Icelandic), Danish `aa` after `ǻ`, Swedish
  full width `ｗ`. Hungarian sorts `ccs ddz ddzs ggy lly nny ssz tty zzs`
  as the letter twice (`cs < ccs`), and its index keys read accented
  letters in contractions as their base (`dž` = `dz`, `čs` = `cs`; Czech
  `čh` ≠ `ch` since `č` is a letter there). Turkish behaves as modelled
  (`I` = `ı`, `İ` = `i`, `Ç` = `ç` ≠ `c`).
- Thai makes space, `-`, `.` and `ฯ ๆ ๏ ๚ ๛` differ at the third level only
  (right after U+A881's second-level weight; index keys ignore them) and
  sorts `x`/`X` as a symbol after `♯` (before digits).
- Vietnamese letters `a ă â d đ e ê o ô ơ u ư` are their base letter's
  weight and a second, tiny one: `á < a < aa < az < ă < ăa < â < ㏂ < ꜳ <
  æ < áz < ᴀ`; accented letters keep DUCET's weights (`ắ` sorts and indexes
  as `á`, `UniqueValues` merges every accented `a`). The bare tone marks sit
  between acute and grave (U+0341 < U+0300 < U+0309 < U+0303 < U+0301 <
  U+0323 < U+0340); precomposed letters keep DUCET's accent order.
- Greek_Mixed sorts each basic Latin letter (and its full width forms,
  capitals first) after its Greek counterpart at the second level: `α A a
  β B b γ C c δ D d ε E e ζ Z z F f G g η H h ι I i J j κ ... ρ Q q R r σ ...
  υ Y y U u φ V v W w χ X x`; `œ` sorts as `o`, `ß` after the `ω` forms;
  accented Latin letters and the Kelvin sign keep their DUCET places; index
  keys are primary only (`a` = `α`, `f` = `g` = `z` = `ζ`).
- Persian reorders the harakat (`fatha << kasra << damma << fathatan <<
  kasratan << dammatan`, kasratan with the isolated kasra's weight) and the
  letters `ا آ ٱ أ ٲ إ ٳ ؤ ئ ك ڭ ڮ ە ہ ة ۃ ۀ ھ ى ے ي ې ۑ` (`ا` with the weight of
  U+081B, `ۏ` with that of `ݩ`); not modelled: its index keys, which merge
  those letters with the letter they are placed after. Arabic sorts `إ` as
  `أ` and `ة` as `ت`.
- Catalog compares a third level, DUCET's tertiary weights, except that
  the basic Latin letters and their full width forms ignore case and width
  (`a` = `A` = `ａ`, the Kelvin sign is `K`; `ⓐ < Ⓐ`, `ȧ < Ȧ`, `^ < ＾`); 8
  letters with two accents (`ặ`, `ậ`, `ệ`...) and combining Latin letters
  differ (not modelled).
- Lithuanian puts the bare dot above after the ogonek.
- Every locale: the curly quotation marks sort as one single and one double
  mark (`‘ = ’ = ‚ = ‛` right after `'`) and index keys read them as `'` and
  `"` (`UniqueValues ( "it's¶it’s" )` → `it's`); U+FFFE and U+FFFF are
  ignorable; an empty value sorts before values whose characters are all
  ignorable.
- File index keys (`UniqueValues` without a name) tailor capitals as well
  (German file: `Ä` = `ä` = `ae`; Swedish file: `W` = `V`), like named ones.

**Lone surrogates.** FileMaker's text is UTF-16 and keeps a lone surrogate
as a code unit: `Code ( Char ( 55357 ) )` → 55357, `Length` → 1, `Left`,
`Middle`, `Right` slice it out, `Exact ( Char ( 55357 ) ; Char ( 65533 ) )`
→ 0, and a high one followed by a low one is their character wherever they
meet (`Char ( 55357 ) & Char ( 56832 )` → 😀 of length 1, also across style
runs; low before high stays two). `=` and `<` give lone surrogates no
weight (`Char ( 55357 ) = Char ( 55358 )` → 1, `< "a"` → 1, `= ""` → 0);
`SortValues` gives the high ones and U+DC00 a second-level weight that
sorts as byte 0 (FileMaker does not order them consistently against Lao
marks of such weights) and ignores the other low ones; `UniqueValues`
tells high ones apart. `GetAsCSS` writes `&#55357;`, `Quote` keeps it, words
separate at it. om-calc holds each as a stand-in, U+10F800 + (unit -
0xD800) in Supplementary Private Use Area-B, joined into the character
whenever a text is built (`text::lone_surrogate`, `utf16_units`); hosts
that store text should write `utf16_units` (a genuine U+10F800..U+10FFFF
character would read as a lone surrogate). Not modelled: FileMaker drops
lone surrogates when it converts text to bytes (`Base64Encode ( "a" & Char
( 55357 ) & "b" )` → `YWI=`, `TextEncode`, JSON strings; a lone high one
alone is error 17) and searches UTF-16 units, so a lone surrogate matches
half of a pair (`PatternCount ( "a😀b" ; Char ( 55357 ) )` → 1, `Position (
"a😀b" ; Char ( 56832 ) ; 1 ; 1 )` → 3, `Substitute` splits the pair).
`GetAsURLEncoded ( Char ( 55357 ) )` crashes the engine itself.

### Portal rows on an intermediate relationship hop

[O-GUI, 2026-10-30] For a layout based on A, with a B portal and a B→C
relationship, a native Data Viewer calculation in active portal rows 1 and 2
returned `Count(B::Key) = 2` in both rows and `Count(C::Key) = 1` in each.
The two B rows had different match keys, each identifying one C row. B and C
were occurrences of the same base table. Expressions were evaluated
read-only and cancelled without saving a watch. This distinguishes traversal
through the displayed B record from the union of all B records related to A.

`CalcHost::relationship_origin` lets a graph-aware host pin an intermediate
portal occurrence to its row. `CalcSchema` pins only when the layout-to-target
path passes through that occurrence and the target is beyond it. The portal's
own aggregate/GetNthRecord set stays related to the layout parent; parent
fields and other relationship branches keep the layout context. Ordinary
reads, aggregates, lookups and nested summary context use that same origin.
Hosts without graph metadata retain their existing behavior through the default.

This preserves the full related-set semantics of an aggregate over B; portal
filtering does not restrict that set ([Claris aggregate functions](https://help.claris.com/en/pro-help/content/aggregate-functions.html)).
