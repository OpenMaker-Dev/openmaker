# Stored calculation formulas

How FileMaker stores calculation formulas in a file, how it compiles source
text into them, and how it shows them again. Written so that a clean-room
implementation can build, from this document alone:

1. a lossless decoder of every stored formula into an expression tree,
2. an encoder that turns source text into the exact bytes FileMaker stores,
3. a printer that shows a stored formula as FileMaker displays it.

Status (2026-10-28): verified on every formula of the 66 shipped templates
and the test project (5,304 formulas: 100% decoded, 100% re-encoded byte for
byte in both streams, 100% compiled from the source-order token stream to the
exact stored code), and on 56,696 formulas of 680 further files (section 11).
FileMaker Server's own engine, run as a compiler (`tools/fms-calc-harness`
compile mode), turns the display text of every template formula back into
its stored bytes, and settled the open points of section 10 (section 10.1).

Evidence keys: **[O]** observation (code, data or a run), **[I]** inference.
**[O-run]** marks observations from runs of FileMaker Server's engine.
Addresses are FileMaker Server 26.0.3.309 for Ubuntu, `lib/libDBEngine.so`
unless marked `Support` (`lib/libSupport.so`). Opcode names are the DWARF
constants `Draco::kDB*Op` (`DBOpTbl.h`, `DBCalcEngine.h`). The complete opcode
table is [catalogs/calc-tokens.csv](catalogs/calc-tokens.csv) (section 9).

## 0. Summary for implementers

- A formula is a key node with up to three children: `4` context table
  occurrence (`KAlias`), `5` the **formatted stream** (`KFormula`: the source
  as a token list in source order, with whitespace and comments) and `6` the
  **Polish stream** (`KPolish`: compiled prefix code). FileMaker evaluates
  only `6`; `5` is for display and editing and may be absent [O].
- Both streams are made of the same tokens: a one-byte opcode (two bytes for
  the newer function ranges, prefix `f8`..`fe`; `9b` + selector for `Get`)
  followed by an operand whose layout depends on the opcode (section 2).
- The Polish stream is prefix code. Every node is its opcode then its
  arguments; fixed-arity functions carry no count, variable-arity forms carry
  a big-endian u16 count. Variable-argument functions, lists, custom
  function and plug-in calls store their arguments **in reverse order**
  (section 3).
- The formatted stream holds the source tokens in order, with `(`, `)`,
  `;`, `[`, `]`, whitespace and comment tokens, and no counts. Names are
  stored resolved (field and table occurrence ids, function opcodes), so
  display text is rebuilt from the current catalog names (section 4).
- Compiling source to Polish code needs the grammar of section 6 and a few
  rules (tail calls, Case defaults, field-reference arguments, local variable
  slots, U+E004) in section 5.
- All text uses FileMaker's SCSU compressor with every byte XOR `0x5A`; its
  exact algorithm is in section 2.4.

## 1. Where a formula lives and its context

`DBCalcFormula::Set` (`0xdb1770`) writes, under the owner's key, the children
`KAlias` = 4, `KFormula` = 5 and `KPolish` = 6; `DBCalcFormula::Get`
(`0xdb0370`) reads `KAlias` and `KFormula`, `LoadCalc` (`0xdaf060`) reads
`KPolish`, and `DeleteTextualPortion` (`0xdb22f0`) deletes `KFormula` [O,
GOT references; key values from the `_GLOBAL__sub_I_DBGlobals.cpp`
initializer, which stores each `Draco::K*` as `02 01 <id>`].

| child | content |
|---|---|
| 4 | context: a key string `[len][segment]` of the table occurrence the formula is evaluated from, e.g. `03 d0 00 02`; `01 00` (occurrence 0) means none: script steps, layout objects, custom menus, and some field calculations in converted files [O] |
| 5 | formatted stream (section 4); absent when the formula text was stripped (`DBFunctionCatalog::StripAllCalculationText`; 936 of 1,427 formulas in the FileMaker Server sample files have no `5`) [O] |
| 6 | Polish stream (section 3) |

Custom function bodies have no child 4 [O]. Values longer than one chunk are
stored chunked like any other value (file-format.md 4.3). Where formulas are
found in a file is section 8.

## 2. Tokens and operands

### 2.1 Opcodes

`DBCalcSolver::ReadOpcode` (`0xe3a9e0`) and `DBCalc::WriteOpcode`
(`0xdbdc20`) [O]:

| stored bytes | opcode | range |
|---|---|---|
| `00`..`f7` | the byte | base table (string resource 130 index = opcode) |
| `f8 n` | 2000 + n | `kDBBeginSpecialOp`: 2000 = While (resource 139) |
| `f9 n` | 2300 + n | `kDBBeginVarArgsOp`: resource 140 index n |
| `fa n` | 2900 + n | `kDBBeginZeroArgOp`: resource 141 |
| `fb n` | 3200 + n | `kDBBeginOneArgOp`: resource 142 |
| `fc n` | 3500 + n | `kDBBeginTwoArgOp`: resource 143 |
| `fd n` | 3800 + n | `kDBBeginThreeArgOp`: resource 144 |
| `fe n` | 4100 + n | `kDBBeginFourArgOp`: resource 145 |
| `9b n` | 1000 + n | `Get` with selector n (opcode 155 node holding solve opcode 1000 + n; resource 131 index n) |
| `ff n` | invalid | the reader consumes two bytes and yields 255, which no form accepts |

`WriteOpcode` writes one byte for opcodes below 1000, the prefix pairs above,
and `9b`, `(op + 0x18) & 0xff` for 1000..1255. This resolves the open
question of architecture.md section 10: **the stored function token is the
resource index**, offset by range as above [O; every name and range checked
by emulating `DBCalc::GetEnglishNameOfDBOpcode` (`0xdb6ea0`) for all opcodes].

### 2.2 Numbers: `Draco::FixPt` in Polish form

`FixPt::WriteAsPolish` (Support `0x16bfc0`) and `ReadAsPolish` (Support
`0x16c180`) [O]:

```
tag u8: 0 = empty, 1 = not a number (kNaNResult), 2 = value; 0 and 1 end it
tag 2:  i16 BE  whole limb count, negated for a negative number
        u16 BE  precision (mPrecision - mExtraPrecision); 16 for literals
        u32 BE  limbs, most significant first: |whole| whole limbs, then
                the fraction limbs
```

The reader takes the absolute value of the precision, raises it to at least
16 and caps it at 800, and reads `(precision + mExtraPrecision + 8) / 9`
fraction limbs (`SetFractUsed`); the writer writes the number's `mFractUsed`
limbs [O]. All 33,710 number literals in the corpus have precision 16, 2
fraction limbs and a positive sign (a written `-1` is unary minus applied to
`1`) [O]. A literal needing more than 16 fraction digits gets precision =
its number of fraction digits and `(precision + 10) / 9` fraction limbs
(mExtraPrecision 2): `0.12345678901234567890123` is precision 23 with 3
fraction limbs, `1E-400` precision 400 with 45; whole limbs grow as needed
(`1E400` has 45 whole limbs and precision 16) [O-run]. A negative zero
reads as zero.

Limbs are base 10^9 (`FixPt::sPower10`, nine digits per `Digit`) [O for the
layout, I for the base, confirmed by every literal in the corpus: `1` =
`02 0001 0010 00000001 00000000 00000000`; `584` = whole limb `0x248`]. The
fraction limbs hold digits 1-9, 10-18, ... after the point. A number token is
opcode `10` + this; opcode `11` (`kDBBoolData`) is read the same way but never
written (`DBDataArg::WriteAsPolish`, `0xdcb910`, converts booleans to numbers)
[O].

### 2.3 Text

A text operand is (`SwapIStream::InternalSCSUExpand`, Support `0x1f31c0`;
`InternalSCSUCompress`, Support `0x1f3e60`) [O]:

```
count   number of UTF-16 code units, in the smallest form:
          n <= 0x7f      1 byte  n
          n <= 0x3fff    2 bytes (0x8000 | n) BE        (first byte 80..bf)
          n <= 0xffff    c2 + u16 BE
          otherwise      c4 + u32 BE
text    SCSU (section 2.4), every byte XOR 0x5A; the reader expands until
        `count` code units are produced, so the byte length is implicit
style   (text literals and symbols only) u8 flags; 0 = none. Nonzero: u32 BE
        length then that many bytes of style buffers (bit 1 character
        styles, bit 2 paragraph styles; SwapIStream::Get<Text>, Support
        0x1f35f0)
```

The writer always emits style flag 0 (`SwapOStream::Put<TextBuffer>`, Support
`0x1f3f20`); no stored formula in the corpus has styles [O]. `$`-variable
names (`1a`) have no style byte [O].

### 2.4 SCSU compressor

Decoding is standard SCSU (UTS #6). Encoding must follow FileMaker's
compressor (`Draco::SCSUCompress`, Support: `BaseState` `0x1ed420`,
`UnicodeState` `0x1ed970`, `LocateWindow` `0x1ed210`, `PositionWindow`
`0x1ed2b0`, `ReStart` `0x1ee210`) to get identical bytes. Behaviour [O,
reproduced byte for byte for all 14,337 distinct strings of the formula
corpus, including 676 with characters above U+00FF]:

State: eight dynamic windows, initially `80 C0 400 600 900 3040 30A0 FF00`
(`SCSU::sInitialDynamicOffset`); active window 0; next window to redefine
3, advancing 3, 4, ..., 7, 0, 1, ... (mod 8) after each definition;
single-byte mode. "Compressible" means U+0000..U+33FF or U+E000..U+FFFF.

Single-byte mode, for the character c at position i (a valid surrogate pair
is one character of two units):

1. c in 1..31 except tab, LF, CR: `SQ0` (01), c.
2. c < 0x80: the byte c.
3. c in the active window: `80 + c - offset`.
4. c in another dynamic window w (lowest w first): if the next unit exists
   and lies in the *active* window, quote: `SQ(w)` (01 + w), `80 + c -
   offset(w)`; otherwise switch: `SC(w)` (10 + w), same byte, and w becomes
   active.
5. c < 0x100: `SQ1` (02), c - 0x80. c < 0x180: `SQ2` (03), c - 0x100.
   c in 0300-037F / 2000-207F / 2080-20FF / 2100-217F / 3000-307F: `SQ3`
   .. `SQ7` (04..08), c - base.
6. Otherwise define a window for c (rule below): `SD(n)` (18 + n) with the
   offset index, or for c > U+FFFF `SDX` (0B) with two bytes `(n << 13) |
   ((c - 0x10000) >> 7)`; n becomes active and c is processed again.
7. If c has no window (U+3400..U+DFFF): when c is the last character, or the
   next two units are both compressible, quote `SQU` (0E), c as u16 BE;
   otherwise `SCU` (0F) and continue in Unicode mode with c.

Window definition for c: 00C0-013F: offset `c & FF80`, index `c >> 7`;
0250-02CF, 0370-03EF, 0530-05AF, 3040-30BF, 30A0-311F, FF60-FFDF: the fixed
windows FA..FF; other c <= 33FF: offset `c & FF80`, index `c >> 7`; E000-FFFF:
offset `c & FF80`, index `(c - AC00) >> 7`; above FFFF: extended.

Unicode mode, for the unit u at i:

1. If u and the next unit are both compressible, leave Unicode mode: target =
   u if u > 0x7F, else the first unit above 0x7F in the run of compressible
   units starting at u (or the last unit of that run). Target <= 0x7F: `UC`
   (E0 + active). Target in a dynamic window w: `UC(w)`, w active. Else define
   a window as above with `UD(n)` (E8 + n) or `UDX` (F1). Then u is processed
   in single-byte mode.
2. Otherwise u in E000..F2FF (and the next unit not compressible, or u last):
   `UQU` (F0), u. Else u as two bytes.

`om-scsu::encode_scsu` implements this algorithm (it reproduces every
formula string and catalog name of the corpus byte for byte; `scsu_encode`
in `tools/calc_polish.py` is the research reference).

### 2.5 Field references

Opcodes `16` (`kDBFieldOp`) and `17` (`kDBFieldRefOp`), identical layout
(`DBFieldRefArg::WriteAsPolish` `0xdbe130`, `ReadFieldAsPolish` `0xe3cb70`)
[O]:

```
16|17  flags u8  table occurrence key  field key  repetition u16 BE
```

- keys are HBKey strings: `[total length][segment length][segment]`, at most
  6 bytes (`04 03 d0 00 02` = occurrence `d0 00 02`; `02 01 0a` = field 10).
- flags (DWARF unnamed enum in `DBCalcEngine.h`): 1 `kFieldNameBracketted`,
  2 `kTableNameBracketted`, 4 `kTableNamePresent`; the reader maps values 6
  and above to 7. In files: 0 (written without `Table::`, the occurrence is
  the context) and 4 (`Table::Field`) [O]. The engine writes 1 for
  `${Field}`, 5 for `Table::${Field}`, 6 for `${Table}::Field` and 7 for
  `${Table}::${Field}`, in both streams; nothing else in the Polish code
  changes [O-run], so the flags only steer display.
- repetition: 1 in every field reference of the corpus; `field[n]` compiles
  to `GetRepetition` instead (section 5.4) [O].
- `17` marks a field used where a field reference is required (section 5.3).

### 2.6 Other operands

| opcode | operand | written by |
|---|---|---|
| `0b` symbol | text (2.3, with style byte) then u8 flag: 0, or 1 when the name was written `${...}` (always 0 in files [O]; 1 for `Let ( ${a b} = 1 ; ${a b} )` [O-run]; `DBSymbolArg` member at `+0x172`) | `DBSymbolArg::WriteAsPolish` `0xe07a00` |
| `14` key | key string `[len][bytes]` (custom function id: `02 01 04` = function 4) | `DBKeyArg::WriteAsPolish` `0xdcab50` |
| `15` plug-in id | 4-byte plug-in id (QuadChar, e.g. `MBSP`, `FMDR`) then u16 BE function id | `DBPluginOpcodeArg::WriteAsPolish` `0xdc9d20` |
| `18` local | u16 BE slot (section 5.5) | `DBVar::WriteAsPolish` `0xdc7d40` |
| `1a` `$`/`$$` variable | name: count + SCSU, no style byte; the name includes `$`/`$$` | `DBScopedVar::WriteAsPolish` `0xdc8160` |
| `9b` Get | u8 selector (Polish) | `DBGetFunction::WriteAsPolish` `0xdc6310` |
| `9c` | u8 selector (formatted only, the name inside `Get ( )`) | `DBCalcScan::Tokenize` `0xe2e930` |

## 3. The Polish stream

### 3.1 Node forms

Read by `DBCalcSolver::ReadAsPolish` (`0xe3ad70`), which keeps an operator
stack: `GetNextOp` (`0xe3c180`) reads an opcode and, for counted forms, its
count; `DefaultReadAsPolish` (`0xe406c0`) schedules `Arity(op)` arguments for
ordinary functions [O]. The forms, by opcode:

| opcode(s) | form |
|---|---|
| `00` | empty argument (an omitted If branch, an empty varargs slot); reads as an empty text constant |
| `10`, `13`, `16`, `17`, `0b`, `14`, `15`, `18`, `1a` | operands of section 2 |
| `9b` sel | Get ( selector ) |
| fixed arity (NArgs Unary..Quad) | opcode, then `Arity(op)` arguments in source order (`DBOperation::WriteAsPolish` `0xdc1420`) |
| NoArgs (`22` True, `23` False, `24` ¶, `31` Random, `97` Pi, `fa xx` constants, text-style constants `aa`..`c8`) | opcode alone |
| VarArgs (NArgs 7) and every `f9 xx` | opcode, u16 n, then the n arguments **last first**; a null argument is `00` (`DBVarArgsOper::WriteAsPolish` `0xdc3690`) |
| `19` list `[a ; b]` | `19`, u16 n, items **last first** (`DBListArg` `0xdcae90`) |
| `4b` If | test, then, else; an omitted branch is `00` (`DBIf` `0xdc4280`) |
| `4c` Case | u16 n, then n pairs (test, result); a default result is a final pair whose test is the number 1 (`DBCase` `0xdc4e20`) |
| `4d` Choose | u16 n results, the test, then the n results (`DBChoose` `0xdc5910`) |
| `df` Let | u16 n, n binding values, the body (`DBLet` `0xdc6710`) |
| `f8 00` While | u16 a, a initial values, the condition, u16 b, b logic values, the result (`DBLoop` `0xdc7380`; `mLogicOffset` = a) |
| `dc` Evaluate | u16 n, u16 `mNeedFieldRef`, the n arguments in order: the text, then one argument per trigger field; `Evaluate ( t ; [ f1 ; f2 ] )` is n = 3 with the fields as plain `16` operands, no list node [O-run] (`DBEval` `0xdc85a0`) |
| `dd` GetField | u16 `mNeedFieldRef`, one argument |
| `3d` SetPrecision, `40` SetRecursion | the second source argument first: `3d <precision> <expression>` |
| `4a` EvaluationError, `4f` IsValid, `de` IsValidExpression | one argument |
| `e3` SetVariable (internal) | value, repetition, target; target is `18` (local) or `1a` ($var) (section 5.5) |
| `e4` GetVarRep (internal) | repetition, variable: `$v[r]` is `e4 <r> <$v>` |
| `1b` call / `1c` tail call | u16 n, the function key (`14 ...`), then the n arguments **last first** (`DBFunctionCall` `0xdc95e0`) |
| `1d` plug-in call | u16 n, the plug-in id (`15 ...`), then the n arguments **last first**, `00` for a null one (`DBPluginFunction` `0xdc9ff0`) |

Counts above 1000 (`0x3e8`) are rejected by `GetNextOp` [O]. Opcodes 1-10
and 12-15 (scanner tokens), 500-537 (solver-internal), 248-254 and 255 are
not valid in Polish code; `0b` (an unresolved name) is accepted by the reader
(`ReadSymbolAsPolish` `0xe3d020`) but never occurs in a stored formula.
Ordinary binary operators are fixed-arity functions: `25 a b` is `a + b`.

Example (Contacts::Initial, [O]): `Case ( not IsEmpty ( Last Name ) ; Left (
Last Name ; 1 ) ; not IsEmpty ( First Name ) ; Left ( First Name ; 1 ) )`,
context `03 d0 00 02`, fields 2 = Last Name and 1 = First Name, is

```
4c 0002
   1e 4e 16 00 04 03d00002 02 0102 0001        not IsEmpty ( Last Name )
   57 16 00 04 03d00002 02 0102 0001           Left ( Last Name ;
         10 02 0001 0010 00000001 00000000 00000000              1 )
   1e 4e 16 00 04 03d00002 02 0101 0001        not IsEmpty ( First Name )
   57 16 ... 0101 0001 10 ...                  Left ( First Name ; 1 )
```

### 3.2 Operators and keywords

| opcode | source | opcode | source |
|---|---|---|---|
| `1e` 30 | `not` (prefix) | `41` 65 | `<` |
| `1f` 31 | `and` | `42` 66 | `<=` |
| `20` 32 | `or` | `43` 67 | `≤` |
| `21` 33 | `xor` | `44` 68 | `=` |
| `25` 37 | `+` | `45` 69 | `<>` |
| `26` 38 | `-` | `46` 70 | `≠` |
| `27` 39 | `*` | `47` 71 | `>=` |
| `28` 40 | `/` | `48` 72 | `≥` |
| `29` 41 | `^` | `49` 73 | `>` |
| `2c` 44 | unary `-` (`kDBNegateOp`) | `50` 80 | `&` |
| `68` 104 | unary `+` compiles to GetAsNumber | `24` 36 | `¶` |

The ASCII and single-character spellings of `<=`, `<>`, `>=` are distinct
opcodes and are preserved [O]. (`DBCalcScan::PunctLookup` `0xe28540`.)

## 4. The formatted stream

### 4.1 Tokens

The formatted stream is the list of tokens `DBCalcScan::Tokenize`
(`0xe2e930`) produced while scanning the source; there is no tree, no count
and no argument reordering [O]:

| token | bytes |
|---|---|
| punctuation | `03` `;`, `04` `(`, `05` `)`, `06` `[`, `07` `]` |
| operators, keywords, function names | the opcode alone (2.1), including `f8`..`fe` pairs and `9b` for the word `Get` |
| whitespace | `0c` then a text token `13 ...` holding the exact characters (spaces, tabs, CR, U+00A0) |
| comment | `0d` (`/* */`) or `0e` (`//`) then a text token with the comment body; a `//` body includes its terminating CR when present |
| number | `10` FixPt (2.2) then u16 BE format: `0x0020` (`FixPt::kForceFloat`) normally, `0x0040` (`kForceScientific`) when written with an exponent (`DBCalcScan::GetNumber` `0xe29fd0`) |
| text literal | `13` text (2.3): the literal's value (escapes already applied, 6.1) |
| field | `16` as 2.5 (always `16` in this stream) |
| unresolved or local name | `0b` symbol: Let/While variable names, custom function parameters, and names typed before they resolve |
| `$` variable | `1a` name |
| Get selector | `9c` u8 selector |
| custom function | `1b` then `14` key (no count) |
| plug-in function | `1d 00 00` then `15` plug-in id (count written as 0) |

Get is stored as `9b ( 9c sel )` with whitespace tokens between [O]. The
Contacts primary key auto-enter formula `Get ( UUID )` is the formatted stream
`9b 0c13017a00 04 0c13017a00 9c65 0c13017a00 05` (20 bytes) and the Polish
`9b 65` [O].

### 4.2 Display

`DBCalcScan::UnTokenizeToFunctor` (`0xe2f120`) rebuilds the text token by
token [O]:

- punctuation and operators: `PunctLookup`; `<=`, `<>`, `>=` (66, 69, 71) as
  two ASCII characters; `≤ ≠ ≥` (67, 70, 72) as the single characters.
- function names and keywords: the resource string of the opcode (localized
  per the resource tables; English in the catalog); Get selectors from
  resource 131.
- fields: `DBCalcScan::GetQualifiedFieldName` with the current names of the
  occurrence and field, `Table::Field` when flag 4 is set, else `Field`;
  names that need it are quoted as `${name}` (`QuoteName` `0xe32970`) [I for
  when].
- numbers: `FixPt::GetFormattedText(format, 0, locale)` with the stored
  format word.
- text literals: `Text::MakeLiteral`: wrapped in `"`, with `"` and `\`
  escaped by `\` [O for the escape characters, I for the full rule, 6.1].
- whitespace and comments: verbatim; a separator `03` prints as `;` (it has
  its own case in `UnTokenizeToFunctor`; a locale-dependent form is not
  ruled out) [I].

Because names are stored as ids, renaming a field or table occurrence changes
how existing formulas display without rewriting them [I]. Symbols (`0b`)
display their stored text.

### 4.3 How the streams relate

Both streams are written together from one parse (`DBCalcFormula::Set`). The
Polish stream is exactly what compiling the formatted tokens with the rules of
sections 5 and 6 produces: 61,063 of the 61,064 formulas that have both
streams compile to their stored bytes (section 11).
One third-party formula has a formatted stream lacking two separators that its
Polish code implies (`Choose ( f ; "Today" "Upcoming" "Past" )`, Event
Management with Audit, field 30); FileMaker would display it as stored.

## 5. Compiling source to Polish code

Rules beyond "prefix order of the parse tree", each verified on the corpus
[O unless noted]:

### 5.1 Argument order and counts

- VarArgs functions, `f9 xx` functions, lists, custom function and plug-in
  calls store arguments last first; everything else in source order.
- SetPrecision and SetRecursion store the second argument first.
- A trailing separator before `)` is accepted and dropped: `Case ( a ; 1 ; b ;
  2 ; )`, `List ( "x" ; "y" ; )`, `MBS ( ... ; $x ; )`, custom function
  calls (`f ( 1 ; 2 ; )`). It is an error after the last argument of `If`
  (`If ( 1 ; 2 ; )` is 1204 at the `)`), and an empty argument in the middle
  is an error everywhere (`List ( 1 ; ; 3 )` is 1204) [O-run].
- An omitted If else-branch is `00`. A Case with an odd number of arguments
  has a default: stored as the pair (number 1, default).
- A function that takes no arguments is written without parentheses (`Pi`,
  `Random`, `Self`, `JSONString`, zero-parameter custom and plug-in
  functions); empty parentheses `( )` give zero arguments.

### 5.2 Tail calls

A custom function call is stored as `1c` (tail call) when its value is the
value of the whole formula: at the root, in the then/else branches of If, the
results (and default) of Case and Choose, and the body of Let, recursively;
elsewhere `1b`. This applies to every formula, not only custom function
bodies.

### 5.3 Field-reference positions

Some arguments must be field references: the arguments of the aggregate
functions Average, Count, List, Max, Min, StDev, StDevP, Sum, Variance,
VarianceP, the field parameter of UnaryField/BinaryField functions
(GetSummary, Extend, GetRepetition, Last, GetNthRecord, NPV; NArgs in
calc-tokens.csv), the first argument of `Lookup` and `LookupNext` (both
VarArgs; never in a file, `e0 0001 17 ...` [O-run]) and `field[n]`. In such
a position:

- a field `16` becomes `17`;
- the request passes into the then/else of If, the results of Case and
  Choose, the body of Let and the result of While;
- an Evaluate or GetField reached this way gets `mNeedFieldRef = 1` (DWARF
  `DBEval::mNeedFieldRef`, the u16 after the count); otherwise 0.
- inside the argument list of a plug-in call no conversion happens: `MBS (
  ... ; List ( T::f ) )` keeps `16` (6 formulas) [O]. The engine does the
  same for the built-in design functions: `FieldNames ( "a" ; List (
  Table::Input ) )` compiles to `1d 0002 15 FMDR 0004 87 0001 16 ...`
  [O-run]. The rule is therefore "any `1d` argument list"; the reason in the
  parser is not located.

`DBCalcParse::IsFieldRefType` (`0xe123c0`) accepts as field-reference
arguments the node kinds 22, 24, 26, 27, 28, 75, 76, 77, 136-138, 220, 221,
223, 227, 228 and While [O].

### 5.4 Repetitions

- `field[n]` compiles to `GetRepetition ( field ; n )` (`89 17 ... <n>`),
  even for a constant n; the field token keeps repetition 1 [O].
- `$v[n]` compiles to `e4 <n> <$v>`.
- `$v[n] = value` in Let compiles to `e3 <value> <n> <$v>`; without `[n]` the
  repetition is the number 1.

### 5.5 Local variable slots

Custom function parameters, Let and While variables live in numbered slots,
referenced by `18 <u16 slot>`:

- custom function parameters are slots 0..p-1, in declaration order;
- each Let binding takes the next slot (base = slots in scope at the `Let`,
  plus its position in the binding list), whatever it binds;
- the first binding of a name in a list declares it in its slot; a binding
  that repeats a name of the same list, and every `$`/`$$` binding, compiles
  to `e3 SetVariable ( value ; repetition ; target )` and leaves its own slot
  unused. A first binding may shadow a parameter or an outer variable;
- While: the initial list as Let; the logic list continues the numbering and
  treats every name of the initial list as a repeat (so `i = i + 1` is `e3
  ... 18 <slot of i>`);
- slots are released at the end of the Let/While (sibling scopes reuse them);
- names compare case-insensitively (`JavascriptSource` matches parameter
  `JavaScriptSource`).

Example (custom function `CURLExecute ( URL ; Post Text ; HTTP Headers )`):
`Let ( [ curl = ... ; r = ... ; r = ... ; ... ; result = ... ; $$debug = ... ;
r = ... ] ; result )`: `curl` = slot 3, `r` = 4, the repeated `r`s and
`$$debug` are `e3` with targets `18 0004` and `1a "$$debug"` in slots 5, 6, 7,
9, 10, and `result` = slot 8, so the body is `18 0008`.

### 5.6 Literals and names

- A text literal's Polish value is its formatted value with U+E004 replaced
  by a space (`DBDataArg::WriteAsPolish` calls `TextBuffer::ReplaceAll(0xE004,
  0x20)`), then re-compressed (2.4). 119 literals contain U+E004 [O]. The
  scanner itself puts it there: a CR (a typed Return) inside the quotes is
  stored in the formatted stream as U+E004 and therefore evaluates as a space
  (`"ab<CR>c"` is formatted `ab` U+E004 `c`, Polish `ab c`, length 4); `¶` is
  the way to put a CR in a literal (6.1) [O-run].
- A symbol that is not a local variable or parameter resolves to a field of
  the context table occurrence by name (`On` in fmSyntaxColorizer field 4
  compiles to `16 00 04 03d00019 02 0106 0001`).
- Number literals compile to `10` FixPt with precision 16.

## 6. Source grammar

From `DBCalcScan` (lexer) and `DBCalcParse` (parser). The formatted stream is
the token list of this grammar, so the compile checks of section 11 also test
it.

### 6.1 Lexical structure

- Characters are classified after `Unicode::ConvertToASCII` (full-width forms
  count as their ASCII equivalents) [O].
- Whitespace: runs of space, tab, CR and U+00A0 (the characters observed)
  are kept as `0c` tokens.
- Comments: `// ...` to the end of the line (CR), `/* ... */` (`GetComment`
  `0xe29160` compares `/`, `*` and CR) [O]; block comments nest, and an
  unterminated block is error 1205 [O-run, Oracle answers (implementation)].
- Punctuation (`PunctToOpTbl` at `0x2e58ad0`): `( ) [ ] + - * / ^ = ≠ ≤ ≥ <
  > & ; ¶`, plus the two-character `<=`, `>=`, `<>` (`GetPunct` `0xe296d0`).
  `;` is the argument separator. In a file whose decimal separator is a
  point, `,` is one too and is stored as `;` (`03`): `List ( 1,5 ; 2 )`
  has three arguments in en-US [O-run, battery `c06`; calc-values.md 13,
  evaluator answers]; with a decimal comma `,` belongs to numbers (10.1).
- Text literals `"..."` (`GetLiteralString` `0xe29920`, which compares the
  characters `"`, `\`, `¶`, `!` and CR) [O]. Stored values contain real CR
  characters, unescaped quotes and backslashes and, in 22 literals, `¶`
  itself [O]. The rule [O-run, compile mode and evaluation]:
  - `\"` is `"`, `\\` is `\`, `\¶` is the character `¶`; a `\` before any
    other character stays, with that character (`"a\nb"` is the four
    characters `a \ n b`, `"a\x"` is `a\x`).
  - `""` inside a literal is one `"` (`""""` is `"`, `"a""b"` is `a"b`);
    two literals side by side are an error (`"a" "b"` is 1212).
  - `¶` is CR (U+000D).
  - A CR typed inside the quotes becomes U+E004 in the formatted stream and a
    space in the Polish stream (5.6), even after a `\`; LF, tab and U+2028
    stay as they are.
  - The full-width quotation mark U+FF02 delimits literals like `"`
    (`ConvertToASCII`).
  - A missing closing quote is 1206 (`"a\"` is unterminated).
  - Display (`Text::MakeLiteral`) writes `"` and `\` with a `\` [O], so a
    literal shown and compiled again keeps its value (section 11).
- Numbers (`GetNumber`, states `kBeforeDigits`, `kBeforeDec`, `kBeforeExp`,
  `kJustAfterExp`, `kAfterExp`): digits, an optional decimal point, an
  optional `E` followed by an optional sign and digits; parsed with
  `FixPt::Set(text, locale)`. An exponent sets format word `0x0040`
  (`1E3`, `1e3`, `1E+3`, `1.5E-2`, `1.5e2`); every other literal has
  `0x0020` (`.5`, `0001`, `1.50`, 20-digit integers) [O-run]. A literal
  ending in the decimal point or exponent is 1218 (`5.`, `1.`, `.`, `1e`,
  `1e+`); a second point ends the literal (`1.2.3` is 1212 at `.3`). In
  en-US `1,5` is 1212; decimal-comma locales are in 10.1.
- Names: identifier characters per `DBCalcScan::IsIdentifierChar`; a name may
  be quoted `${...}` (`GetBrackettedName` `0xe2d680`; unterminated: error
  1207). `Table::Field` qualifies a field. `$name` and `$$name` are variables
  (`IsScopedVar`).
- Function names, keywords and `Get` selectors match case-insensitively
  (`DBStrMap::FindPrefix` uses `Unicode::FoldCase` and `StringCaseCompare`)
  against the English names, the localized names (`gLocalizedFunctionNames`),
  plug-in functions and custom functions, after `DBFunctionNameRemap`
  (renamed legacy functions) [O for the maps; which localized names exist is
  open].

### 6.2 Precedence and associativity

From the call structure of `ParseExpr` (`0xe0edb0`) and the `*LeftRecur`
functions [O]; tightest first:

| level | operators | parser |
|---|---|---|
| 1 | primary: literals, names, calls, `( expr )`, `[ list ]` | `Parse_TrivialExpr` `0xe10500` |
| 2 | prefix `not`, `-`, `+` (right-recursive) | `Parse_UnaryExpr` `0xe102a0` |
| 3 | `^` (left) | `Parse_PowLeftRecur` |
| 4 | `*` `/` (left) | `Parse_MultLeftRecur` |
| 5 | `+` `-` (left) | `Parse_AddLeftRecur` |
| 6 | `&` (left) | `Parse_ConcatLeftRecur` |
| 7 | `= <> ≠ < <= ≤ > >= ≥` (left) | `Parse_RelLeftRecur` |
| 8 | `and` (left) | `Parse_AndLeftRecur` |
| 9 | `or` `xor` (left) | `Parse_OrLeftRecur` |

So `not` binds tighter than comparison: `not a = b` is `(not a) = b`, and
`-2^2` is `(-2)^2` [O from the parser structure; O-run: `not 1 = 2` compiles
to `44 1e <1> <2>`, `-2^2` to `29 2c <2> <2>`]. Also confirmed by compiling:
`2^3^2` is `(2^3)^2`, `1 - 2 - 3` and `8 / 4 / 2` group left, `1 & 2 + 3`
is `1 & (2 + 3)`, `1 = 1 and 0 or 1` is `((1 = 1) and 0) or 1`, `1 or 0 and
0` is `1 or (0 and 0)`, `1 < 2 = 1` is `(1 < 2) = 1`, `- -1` and `not not 1`
nest, `+"3"` is `GetAsNumber ( "3" )` [O-run]. Parentheses leave no trace in
the Polish code, and the engine folds no constants (`1+2` stays `25 <1>
<2>`).

### 6.3 Primary forms

```
primary  = number | text | field | "$"var ["[" expr "]"] | name | "¶"
         | constant | function | "(" expr ")" | "[" expr {";" expr} "]"
field    = [table "::"] name ["[" expr "]"]
function = name                         (no parameters)
         | name "(" [expr {";" expr} [";"]] ")"
         | "Get" "(" selector ")"
         | "Let" "(" bindings ";" expr ")"
         | "While" "(" bindings ";" expr ";" bindings ";" expr ")"
bindings = binding | "[" binding {";" binding} "]"
binding  = (name | "$"var | "$$"var) ["[" expr "]"] "=" expr
```

Argument counts are checked against `DBCalcParse::GetMinMaxArgs`
(`0xe17920`, columns `min_args`/`max_args` of calc-tokens.csv, 0 =
unlimited); errors 1201 too few, 1202 too many.

### 6.4 Errors

Parse errors are FileMaker error numbers 1200-1225 (catalogs/error-codes.csv:
1201 too few parameters, 1202 too many, 1203 unexpected end, 1204 operand
expected, 1205 comment not terminated, 1206 text not terminated, 1207
unbalanced parenthesis, 1208 operator missing or function not found, 1209 name
missing, 1211 list not allowed, 1212 operator expected, 1213 variable already
defined in Let, 1214 field required, 1215 invalid Get parameter, 1216-1217
GetSummary arguments, 1219 self reference, 1224 function does not exist, 1225
not supported in this context). Seen in code [O]: 1203 and 1204 in the
`*LeftRecur` parsers, 1207 for an unterminated `${` (`GetBrackettedName`),
1218 for an error token when displaying (`UnTokenizeToFunctor`), 1224 from
`DBOperation::WriteAsPolish` for an opcode it cannot write. A repeated Let
name is accepted (5.5; the engine compiles `Let ( [ x = 1 ; x = 2 ] ; x )`
without error), so 1213 was never produced.

OpenMaker bounds the parser's recursion and the tree it builds separately. At
most 151 expressions are read at once: the formula and each parenthesized
expression, argument, list item, binding value and repetition index inside
another. Every recursive path of the parser passes through one of these;
binary and prefix operators are parsed without recursion. The tree may have at
most 151 nodes on one path, the decoder's 150 levels below the root: each
node's depth is carried up as it is built, and a node one deeper is refused.
Parentheses can reach the recursion bound without adding tree nodes; an
operator chain or a run of prefixes can reach the tree bound without
recursion. Either excess is error 1200 with a source span, before any pass
over the tree. This is an OpenMaker limit, not a measured FileMaker parser
limit. The decoder appends each operand as it reads it, so a header's count
reserves no memory: 151 nested headers declaring 1000 operands each allocate
no operand storage before the depth bound refuses them.

The boundary and hostile-input tests run on a 1 MiB thread. They compile every
recursive reader at the deepest recursion the parser allows, alone and behind
each binary precedence level and prefixes, from text and from formatted
tokens, and decode streams nested past the decoder's bound through every node
form. Measured with rustc 1.99.1 on macOS arm64, they need about 531 KiB of
stack in a debug build and 228 KiB in a release build; frame sizes vary with
compiler and target.

What the engine's parser returns [O-run, compile mode in a file whose context
table occurrence `Table` has fields `Input` and `Output`; battery
`c01-needed`]. `begin:end` is the span FileMaker selects: UTF-16 offsets into
the source, end exclusive (`DBScanError::mBeginPos`/`mEndPos`, read with
`DBScanError::Retrieve`).

| source | error | span |
|---|---|---|
| `1 +` | 1204 | 3:3 (end of text) |
| `1 + * 2` | 1204 | 4:5 `*` |
| `) 1` | 1204 | 0:1 |
| `Abs ( )` | 1204 | 6:7 `)` |
| `If ( 1 ; 2 ; )`, `List ( 1 ; ; 3 )` | 1204 | the token after the `;` |
| `Case ( )`, `Evaluate ( )`, `Get ( )` | 1204 | the `)` |
| `Let ( 1 = 1 ; 1 )`, `[ 1 ; 2 ]`, `List ( [ 1 ; 2 ] )` | 1204 | the `1` / the `[` |
| `While ( [ i = 0 ] ; i < 3 ; i + 1 )` | 1204 | 30:31 `+` |
| `( 1`, `Abs ( 1` | 1207 | end of text |
| `${abc` | 1207 | 5:5 |
| `"abc`, `"` | 1206 | from the quote to the end |
| `/* open`, `/*` | 1205 | from `/*` to the end |
| `Abs ( 1 ; 2 )`, `Abs ( 1 ;` | 1202 | 8:9, the `;` before the extra argument |
| `Left ( "a" ; 1 ; 2 )` | 1202 | 15:16, the second `;` |
| `Left ( "a" )`, `Case ( 1 )`, `If ( 1 )`, `Choose ( 1 )` | 1201 | the `)` |
| `Substitute ( "a" ; [ "a" ] ; "b" )` | 1201 | 25:26 |
| `NoSuchFunction ( 1 )`, `Länge ( "abc" )` (en-US file) | 1208 | the name |
| `Get ( NoSuchSelector )`, `Get ( 1 )` | 1215 | the selector |
| `GetRepetition ( 1 ; 1 )`, `Extend ( 1 )`, `Last ( "a" )` | 1214 | the argument |
| `Sum ( 1 + 1 )` | 1214 | 6:11, the whole argument |
| `Average ( Table::Input + 1 )` | 1214 | 17:26, from the field name to the end of the argument |
| `GetSummary ( Table::Input ; 1 )` (Input is not a summary field) | 1216 | 20:25 `Input` |
| `1,5` (en-US), `"abc" "def"`, `1 2` | 1212 | the token where an operator was expected |
| `Let ( x = 1 ; x[2] )` | 1212 | 15:16 `[` |
| `nonexistent_name + 1`, `x[2]`, `Let ( x = 1 ; y )` | 102 | the name |
| `Abs 1` | 102 | 0:5: unquoted names may contain spaces, so `Abs 1` is one field name |
| `Table::NoSuchField` | 102 | 7:18 |
| `NoSuchTable::Input` | 106 | 0:11 |
| `Self` (without `kSupportSelf`, 10.1) | 1225 | 0:4 |
| `5.`, `1e`, `.` | 1218 | the literal |

## 7. Special and newer functions

- **Design functions** (resource 134) and the mobile group (resource 156) are
  built-in plug-in functions: plug-in id `FMDR` (`DBPlugIn::gBuiltinFMPlugID`)
  with function id = resource 134 index, and `FMMB`
  (`gBuiltinMobilePlugID`) [O: initialized in `_GLOBAL__sub_I_DBPlugIn.cpp`
  `0xb105e0`; all 15 `FMDR` ids in the corpus match the argument counts of
  their resource 134 signatures, e.g. `1d 0001 15 464d4452 000a 9b 05` =
  `LayoutNames ( Get ( FileName ) )`]. `FMMB` ids are assumed to be resource
  156 indexes [I]. The design functions are not known to libDBEngine alone:
  the host registers them at start-up with libFMEngine's
  `Draco::InstallDesignFunction()` (`0x9faa30`), which calls
  `DBCalcScan::RegisterExternalFunction` (DBEngine `0xe33070`) with
  `gBuiltinFMPlugID` and the resource 134 names [O]. Without it they are
  1208 (or 102 for the zero-argument `DatabaseNames`); with it all 26
  compile to `1d n 15 FMDR <id>` with the ids above, `WindowNames` and
  `DatabaseNames` without parentheses as `1d 0000` [O-run]. Nothing in the
  server libraries registers the `FMMB` group (`Location ( 1 )` is 1208 even
  then), so an implementation registers it only where FileMaker would (Go).
- **Newer functions** (resources 140-145) use the extended opcodes of 2.1;
  their arity class follows the range: 2300+ VarArgs (count, reversed), 2900+
  zero-argument constants and functions, 3200+ one, 3500+ two, 3800+ three,
  4100+ four arguments [O, `GetNumberOfArgs` `0xe3aae0`].
- **While** (resource 139) is `f8 00` (2000).
- **Get** selectors: resource 131 index = stored byte; `kEndGetFuncOp` = 1142,
  so selectors run to 141 [O]. FileMaker 26.1.1 adds selector 142,
  `SortOrder` (`9b 8e`), so its `kEndGetFuncOp` is presumably 1143 [I: no
  26.1.1 DWARF].
- **Plug-in functions** (third party): `1d`, count, `15` id, arguments. The
  display name comes from the installed plug-in (`DBCalcScan::
  GetExternalFunctionName`); without it FileMaker cannot name the function
  [I].
- **Custom functions**: `1b`/`1c`, count, `14 <key>` where key is the custom
  function id in `16/5/<id>` (section 8).

## 8. Where formulas are stored

All locations share this codec. Owner keys are `Draco::K*` constants
(`_GLOBAL__sub_I_DBGlobals.cpp`) [O for values, I for names where marked].

| owner | path | key constant | corpus count |
|---|---|---|---|
| calculation field / auto-enter calculation | `<T>/3/5/<F>/5` | `KFormula` 5 | 1,142 |
| validation calculation | `<T>/3/5/<F>/25` | `KValidateFormula` 0x19 | 2 |
| validation message calculation | `<T>/3/5/<F>/32` | `KValidateMessageFormula` 0x20 | 0 |
| container external storage path | `<T>/3/5/<F>/39` | `KStorageFormula` 0x27 | 40 |
| field display names | `<T>/3/5/<F>/49` | `KFieldDisplayNames` 0x31 | 0 (file-metadata.md 1.2) |
| script step parameter | `17/5/<script>/5/<step>/<slot>/5`, deeper for nested options (`.../<slot>/<n>/0/5`) | `KScriptDefs` 17 | 48,538 |
| layout object text / label | `4/5/<L>/5/<O>/2` | `KLabelCalc` 2 | 664 |
| tooltip | `4/5/<L>/5/<O>/3` | [I] | 2,252 |
| (button text, older layouts) | `4/5/<L>/5/<O>/4` | unknown | 154 |
| placeholder text | `4/5/<L>/5/<O>/6` | `KPlaceholderText` 6 | 1,081 |
| item labels (segments, panels, chart items) | `4/5/<L>/5/<O>/17/<n>/5` | unknown | 1,444 |
| portal filter | `4/5/<L>/5/<O>/37` | `KFilter` 0x25 [I] | 14 |
| conditional formatting condition n | `4/5/<L>/5/<O>/138/<n>/5` (sibling `13` = `CFCondition` protobuf, `1` = predefined kind) | `KCFConditions` 0x8a | 1,104 |
| hide object when | `4/5/<L>/5/<O>/141` | `KHideCondition` 0x8d | 275 |
| active button bar segment | `4/5/<L>/5/<O>/142` | `KActiveObjectCalc` 0x8e | 1 |
| calculated button icon (26.1.1, SVG text) | `4/5/<L>/5/<O>/145` | `KButtonIconSvgCalc` 0x91 | 0 (layout-model.md 5.3.4) |
| button action parameter | `4/5/<L>/5/<O>/0/<n>/5` | | 82 |
| object script trigger parameter | `4/5/<L>/5/<O>/48/...` | `KScriptTriggers` 0x30, `KParameter` 0x31 | 3 |
| layout script trigger parameter | `4/5/<L>/48/5/<trigger>/49` | same | 7 |
| custom function | `16/5/<id>/35`, no context; `16` name, `32` = `02` + u16 BE parameter count (checked on 218 functions with 0-21 parameters), `33` parameter names joined by `;`, `36` = `01`; 26.1.1's descriptions in `3` (calc-functions.md 6.0) | `KGlobalFunctions` 0x10, `KFunction` 0x23, `KArity` 0x20, `KParamList` 0x21 | 224 |
| custom menu install-when; menu items | `65/5/<M>/37`; menu-item formula data below the member at a separate location (see below) | `KCustomMenus` 0x41, `KFilter` 0x25 | 4,970 |
| privilege set record access calculation, per table; only k = 9 seen, with formulas guarding deletion ("need to be able to delete own record") [I: 9 = delete] | `23/2/5/<P>/5/<T>/<k>` | | 3 |
| other `K*Calc` constants (not seen in the corpus) | `KTempTextLayoutCalc` 0x8f, `KLayoutCalcs` 0x90, `KImportODBCAuthCalc` 0x91, `KFieldEntryCalc` 0x92, `KUserNameCalc` 0x73, `KPasswordCalc` 0x74, `KItemsCalc` 5 (value lists) | | 0 |

Some FileMaker Server sample files keep layouts under `4/5/<L>/13/5/<O>/...`
with the same attribute keys [O]. Counts are over all 747 files of section 11.

The custom menu definition at `[41, 5, M, 02]` (hex) is `KType`,
two big-endian u32s for the base menu ID and flags. Its items have a
separate `KList` stream at `[41, 5, M, 07]`; the stream starts with a
big-endian i16 count followed by item records, each with an inline
script-step descriptor in the custom menu serialization. [O from bin:
FileMaker Pro 26.1.1 Mac arm64 FMEngine, `FMCustomMenu::LoadMenuItems`
at `0x544284`, `SaveMenuItems` at `0x544554`,
`CustomMenuItemList::Save` at `0x5876a8` and
`CustomMenuItem::Write` at `0x585730`]. The step's keyed parameter data
is separate from the item stream [O from bin: DBEngine
`DBCatalogItemList::GetDataPos` at `0x1a50220`]. The 66 starter files contain no menu
item list. A populated native save is still needed to check the item
references and locate their formula and step data in a file
([file-format.md](file-format.md) 3.7).

## 9. calc-tokens.csv

[catalogs/calc-tokens.csv](catalogs/calc-tokens.csv), one row per opcode
(523 rows) [O except `polish_operands` text, which summarizes section 3]:

| column | meaning |
|---|---|
| `opcode` | internal `DBOpcode` value (blank for built-in plug-in rows) |
| `stored` | bytes in both streams (`9b xx` for Get selectors; plug-in rows show the call pattern) |
| `constant` | DWARF `kDB*Op` constant name(s) |
| `english_name` | `DBCalc::GetEnglishNameOfDBOpcode` (emulated) |
| `source_name` | the name or symbol typed in source |
| `kind` | `function`, `operator`, `constant`, `data`, `punctuation`, `layout` (formatted only), `get-selector`, `builtin-plugin`, `internal`, `unused` |
| `nargs` | `DBCalcSolver::GetNumberOfArgs` class (`DBCalcNArgs`) |
| `arity` | `DBCalcSolver::Arity`, arguments read without a count |
| `min_args`, `max_args` | `DBCalcParse::GetMinMaxArgs`, 0 = unlimited; for special forms from section 3 |
| `polish_operands` | layout after the opcode in Polish code |
| `resource` | string resource / index of the name (calc-functions.csv) |
| `signature` | from the resource |
| `corpus_templates`, `corpus_all` | node counts in the 67 required files and in all 747 files |

## 10. Unknowns

Status after the engine runs of 10.1 (settled items keep their number):

1. Settled: the text-literal rule is in 6.1; a CR typed inside quotes is
   what becomes U+E004 (5.6).
2. Settled: the symbol flag is 1 for a `${...}` name and field flags 1, 5,
   6, 7 record `${...}` quoting (2.5, 2.6); neither changes the Polish code
   otherwise, so they affect display only [I for display: `QuoteName` is
   the consumer].
3. Partly settled: the engine itself keeps `16` inside every `1d` argument
   list, built-in design functions included (5.3); the line of parser code
   that does it is not located.
4. Settled: only `0x0020` and `0x0040` occur (6.1); the decimal separator of
   number literals is the file locale's (10.1).
5. Open: `FMMB` ids (resource 156 assumed); the server engine never
   registers that group (7).
6. Open: layout attribute keys 3, 4 and 17 have no named constant; meanings
   are from layout-model.md and the data.
7. Open: how FileMaker displays a formula whose formatted stream was
   stripped (`KUnknown`/`KNull` in `DBCalcFormula::Get` suggest a
   placeholder); the harness cannot reach `DBCalcFormula::Get` without an
   `HBPosition`. A reference to a deleted field displays as
   `Table::<Field Missing>` (`UnTokenize` on the Japanese server sample)
   [O-run].
8. Settled: every function, operator, constant and Get selector absent from
   the corpus compiles to the bytes its arity class predicts (10.1), with
   two rule corrections (`Lookup`/`LookupNext` field references, 5.3;
   `Evaluate` trigger lists, 3.1). Still never produced: `0b` in Polish
   code and the data opcodes `11` (boolean) and `12` (date/time).

### 10.1 Engine answers (compile mode)

`tools/fms-calc-harness` compile mode (`calceval -c`, README there) runs
the steps of `DBCalcFormula::Set` (`0xdb0fe0`) without the database writes:
a `DBCalcParse` on the file's locale and table occurrence catalog
(`DBFileUser::mTableAliasCatalog`, `+0xa00`), `DoParse` (`0xe0e990`) in a
given context, the root's `Optimize(1)` and `WriteAsPolish` (DBCalc vtable
slots 15 and 16) into memory, then `DBCalcScan::Tokenize` (`0xe2e930`) of
the same text. It prints the error, the error span, the context bytes and
both streams. Round-trip mode (`-r`) displays a stored formatted stream with
`DBCalcScan::UnTokenize` and compiles the text again. Every answer below is
[O-run]; the batteries and results are in `tools/fms-calc-harness/batteries`
(`c01`..`c05`) and `results/`.

- **Validation.** The engine's display text of every formula of the 66
  templates and the test project compiles back to its stored formatted and
  Polish bytes in its stored context: 5,304 of 5,304 (section 11). The 44
  layout conditional-formatting formulas using `Self` need option
  `kSupportSelf` (`DBCalcFormula::CalcOptions` 16; without it `Self` is
  1225, with it `Self` is `fa 00`), so FileMaker compiles those
  calculations with it [I]. Options 1, 2, 4 (auto-quoting) wrap unparsable
  text in quotes (`ShouldAutoQuote`, `Text::MakeLiteral`) and were not
  needed by any stored formula.
- **No constant folding.** `Optimize(1)` leaves `1+2` as `25 <1> <2>`.
- **Precedence** (6.2), **literals** (6.1, 2.2, 5.6) and **errors with
  spans** (6.4) as documented there.
- **Decimal comma.** In a file whose locale uses a decimal comma (Contacts
  de-DE), `1,5` compiles to 1.5, `1,5E2` to 150 and `List ( 1,5 ; 2 )` to
  two arguments; `1.5` is 106 at `.5`, and `1 , 5` is 1218 at the comma.
  `;` stays the only separator. In en-US `1,5` is 1212.
- **Names.** Function names, `Get` selectors and custom function names match
  case-insensitively (`abs`, `ABS`, `get(CURRENTDATE)`, `__gen_rabatt`);
  localized function names are not accepted in an en-US file (`Länge` is
  1208). `Let` names match case-insensitively (`Let ( A = 1 ; a )`) and
  `${x}` binds the same slot as `x`. An unresolved name is 102 (106 with no
  table context, as `Evaluate` reports).
- **Field references.** `Input` (context field) is flags 0, `Table::Input`
  4, `${Input}` 1, `Table::${Input}` 5, `${Table}::Input` 6,
  `${Table}::${Input}` 7. `Input[2]`, `Table::Input[0]`, `[-1]` and `[1+1]`
  all compile to `GetRepetition ( fieldref ; n )`; range errors come at
  evaluation (calc-values.md 12.1).
- **Field-reference positions.** As 5.3, now including `Lookup`,
  `LookupNext` and `GetSummary` (`86 17 <summary> 17 <break>`, compiled in
  the MBS Swiss QR example, which has a summary field); `Evaluate` and
  `GetField` get `mNeedFieldRef` = 1 under `List`, `Sum` and
  `GetRepetition`, also through `If` and `Let` results, and 0 elsewhere
  (alone, or as a `While` result outside a field position). `Evaluate (
  "1" ; [ Table::Input ; Table::Output ] )` is `dc 0003 0000 <"1"> 16 ...
  16 ...` (3.1).
- **Control forms.** `If ( 1 ; 2 )` stores `00` as the else branch; `Case (
  1 ; "a" ; "b" )` stores the default as the pair (1, "b"); `Case ( 1 ; "a"
  ; )` drops the trailing separator; `If ( 1 ; 2 ; )` is an error (5.1).
- **Let, While, variables** compile exactly as 5.5 (slots, `e3`, `e4`).
- **Custom functions** (the MBS FileMaker_Snippet_Storage example:
  `__GEN_Rabatt ( pBetrag ; pRabatt )` id 64, `__GUID` id 20 without
  parameters, `__GEN_Range_ID` id 17, standing in for needed-expressions
  section 15's f, g, r): `f ( 1 ; 2 )` is `1c 0002 14 02 01 40 <2> <1>`
  (tail call, arguments last first), `1 + f ( 1 ; 2 )` is `1b`, inside `If`
  branches, `Case`/`Choose` results and as the whole formula `1c`, as a
  `Let` binding value or a `List` argument `1b`. `g` and `g ( )` both compile
  to `1c 0000 14 02 01 14`. Too few or too many arguments are 1201/1202 at
  the `)` (built-in functions report 1202 at the extra `;`); a trailing `;`
  is accepted.
- **Every unused function** (battery `c02`, generated from calc-tokens.csv
  rows with `corpus_all` = 0): all 106 functions, 30 constants, `xor`, 68
  Get selectors and the 26 `FMDR` design functions compile, and
  calc_polish.py's compiler reproduces each Polish stream from the engine's
  formatted stream (239 of 239 compiled lines, 183 of 183 in `c01`).
  Constants compile standalone (`Plain`, `ValueNumber`, `ROWID`). The
  exceptions are expected: `GetSummary` needs a summary field (1216, then
  compiled in `c04`), and the `FMMB` functions are not registered (7).

## 11. Verification and tools

Tools (read-only):

- `cargo run --release -p om-hbam --example dump_calcs -- <file.fmp12>... >
  calcs.tsv` walks the key tree and writes one row per stored formula
  (`calc  file  owner  path  context  formatted  polish`, hex) plus the names
  a printer needs (`to`, `field`, `cf` rows). Nodes whose children are only
  4..6 with a `6` are formulas when their path matches a section 8 owner;
  catalog indexes (`<cat>/1/...`) and record data are excluded.
- `python3 -I tools/calc_polish.py check calcs.tsv` decodes both streams,
  re-encodes them, compiles the formatted tokens into Polish code (sections
  5-6, including `scsu_encode`) and compares with the stored bytes;
  `print calcs.tsv [path]` shows each formula from both streams (the Polish
  printer regenerates source from code alone, naming locals `_vN`); `diff`
  compares trees; `hex <polish> [<formatted>]` decodes one formula.

Results (2026-10-28):

| corpus | files | formulas | Polish decoded and re-encoded | formatted decoded and re-encoded | compiled to stored Polish |
|---|---:|---:|---:|---:|---:|
| 66 templates + test project (required) | 67 | 5,304 | 5,304 | 5,304 | 5,304 |
| MonkeyBread MBS plug-in examples (`/Volumes/MBSFMPlugin163/Examples`) | 673 | 55,269 | 55,269 | 55,269 | 55,268 (1 anomaly, 4.3) |
| FileMaker Server samples (6 languages) + oracle smoke file | 7 | 1,427 | 1,427 | 491 (936 stripped) | 491 |

Opcodes in the corpus: 229 Polish node kinds (counting each of the 75 Get
selectors seen) and 152 formatted token kinds; the formatted-only tokens are 3-7, 11-14 and
156. Opcodes seen in code but absent from every file: see calc-tokens.csv
rows with `corpus_all` = 0 (137 functions, operators and constants).

Reproduction of the catalog: opcode names and arity come from DWARF
constants (`llvm-dwarfdump --name=kDB...`) and from emulating the leaf
functions `GetNumberOfArgs`, `Arity`, `ArgsNeeded`, `GetMinMaxArgs`,
`GetEnglishNameOfDBOpcode`, `PunctLookup`, `IsFieldRefType` of
`libDBEngine.so` under unicorn (scratch harness in
`re/work/calc-encoding/emu.py`, not part of the repository).

Expressions FileMaker Pro should compile to confirm the open points are in
`re/work/calc-encoding/needed-expressions.txt` (171 expressions); sections
1-14 were compiled by FileMaker Server's engine instead (battery `c01`, 10.1)
and section 15 with stand-in custom functions (`c03`).

Engine round trip (`tools/fms-calc-harness/roundtrip.py`, 2026-10-28): each
stored formatted stream displayed with `DBCalcScan::UnTokenize` and compiled
again in its stored context (10.1) [O-run]:

| corpus | formulas with a formatted stream | compiled | formatted identical | Polish identical |
|---|---:|---:|---:|---:|
| 66 templates + test project | 5,304 | 5,304 | 5,304 | 5,304 |
| FileMaker Server samples + oracle smoke file (custom function bodies excluded) | 491 | 481 | 481 | 481 |

44 template formulas (conditional formatting `Self = 0`) compile only with
`kSupportSelf`. The 10 server-sample failures reference a deleted field: the
Japanese sample displays them as `担当者::<Field Missing>`, which is not
compilable text (error 102). Custom function bodies are left out because
their parameter names need a binding environment the harness does not build.
Per-owner counts are in `tools/fms-calc-harness/results/roundtrip.txt`.

## Oracle answers (implementation)

Settled for `om-formula` (2026-10-28) with FileMaker Server 26.0.3's engine
(`tools/fms-calc-harness`): evaluation runs in `inventory-en.fmp12` without
a table context (an unknown or unqualified field name is 106 there, a
qualified one that resolves 103), and compile-mode runs in its occurrence
`Products` (`03d00002`). Answers already in 6.1, 6.4 and 10.1 are not
repeated. Spans are UTF-16 `begin:end` as in 6.4.

Names and characters:

| expression | result |
|---|---|
| `Let ( a#b = 7 ; a#b )`, likewise with `!`, `$`, `%`, `'`, `.`, `:`, `?`, `@`, `_`, backtick, `{`, `\|`, `~`, `«`, `»`, `‘`, `’`, `÷`, `×`, `–`, `—`, `…`, `·`, U+00A0, U+2003, U+3000, U+200B, U+2028, full-width `｛`, `＄`, `：` | 7 (name characters) |
| the same with `"`, `&`, `(`, `)`, `*`, `+`, `,`, `-`, `/`, `;`, `<`, `>`, `[`, `\`, `]`, `^`, `}`, `≠`, `≤`, `≥`, `¶`, `“`, `”`, U+2212, full-width `＋`, `（`, `；`, `，`, `＂`, `｝`, `＆`, `＊`, `／` | 1204 (delimiters; `=` and `＝` give 106) |
| `Let ( x y = 1 ; x y )`; `Let ( x Abs = 1 ; x Abs )`; `Let ( Model Year = 1 ; 1 )` | 1; 1; 1 (a name runs over the whitespace between words) |
| `Let ( x y = 1 ; x  y )`; `Let ( x y = 1 ; x<TAB>y )` | 106; 106 (whitespace inside a name is compared exactly) |
| `Let ( x = -2 ; Abs x )`; `Pi 1`; `Get ( UUID 1 )` | 106; 106; 1215 (`Abs x`, `Pi 1`, `UUID 1` are single names) |
| `Let ( x and y = 1 ; 1 )`; `Let ( x = 1 ; x and 0 )`; `Let ( notx = 1 ; notx )` | 1204; 0; 1 (`and`, `or`, `xor`, `not` end a name) |
| `Inventory Transactions::Units In \| Out`; `Products:: Model Year` | 103; 102 (everything before the first `::` is the occurrence, everything after it the field) |
| `Products :: Model Year`, `Products ::Model Year` (compile mode) | 102 at 0:8 (a word starting with `::` does not continue a name) |
| `Table::${Input}` | flags 5 (a name word stops before `${`; a `$` variable's does not: `Let ( $${x} = 1 ; 1 )` is 1204 at 10:10) |
| `${Model  Year}`; `Let ( ${} = 1 ; 1 )`; `Let ( ${$x} = 1 ; $x )` | 102 at 2:14; 1209 at 9:9; 1204 at 8:11 (a quoted name is selected from after `${` to after `}`; it may not start with `$`) |
| `1<LF>+1`, and with U+00A0, U+2003, U+3000, VT or FF in place of LF; `1<U+2028>+1` | 2; 106 |
| `1 // x<LF>+ 1`; `1 // x<CR>+ 1` | 1; 2 (only CR ends a line comment) |
| `1 /* a /* b */ c */ + 1`; `1 /* a /* b */ + 1`; `1 /**/ + 1` | 2; 1205; 2 (block comments nest) |
| `Length ( “ab” )`; `”ab”`; `Length ( „ab„ )`; `Length ( ‟ab‟ )`; `Length ( "a“b" )`; `Code ( "\“" )` | 2; ab; 2; 2; 106; 8220 (`“ ” „ ‟` open and close literals like `"` and `＂`, and `\` escapes them) |
| `Length ( "a″b" )`; `Length ( "a〝b" )`; `Length ( ‹ab› )` | 3; 3; 1204 (`″`, `〝` are text) |
| `1 ‹ 2`; `2 › 1`; `1 ‹= 2` (compile mode) | `41 <1> <2>`; `49 <2> <1>`; 1204 at 3:4 (`‹ ›` are `< >`, never part of `<=` or `<>`) |
| `Length ( "a\;b" )`; `Code ( "\\\s" )` | 4; 1150009200092 (`\` before other characters stays) |
| `1 \ 2`; `\`; `}`; `Let ( a}b = 7 ; a}b )` | 1204 at 2:2; 0:0; 0:0; 7:7 (no token starts there; the span is empty) |

Name resolution:

| expression | result |
|---|---|
| `Let ( Abs = 1 ; Abs )`; `Let ( Pi = 1 ; Pi )`; `Let ( Self = 1 ; Self )`; `Let ( and = 1 ; and )` | 1 (a Let name wins over functions, constants and keywords; the formatted stream holds symbols `0b`) |
| `Let ( Abs = 1 ; Abs ( -1 ) )`; `Let ( a = 1 ; a ( 2 ) )` | 1212 (even before `(`) |
| `Abs`; `Get`; `Let` | 106 (a function with parameters only before `(`) |
| `Pi`; `Lower`; `Extend`; `Lower ( "A" )` | 3.1415926535897932; 0; 16384; a (without `(`, the constant) |
| `1 foo`; `Let ( x = 1 x )`; `Let ( x = 1 ; x 2 )` | 106 (an unknown name fails when read, before any operator check) |

Grammar:

| expression | result |
|---|---|
| `Max ( 1 ; 2 ; )`; `Choose ( 1 ; "a" ; "b" ; )`; `Case ( 0 ; 2 ; 1 ; 4 ; )` | accepted |
| `Case ( 1 ; 2 ; 3 ; )`; `Choose ( 1 ; )`; `Left ( "a" ; )` | 1204 (a trailing `;` only after a complete Case pair and once the minimum count is met) |
| `Count ( 1 )`; `Sum ( 1 ; )`; `Max ( 1 )`; `Sum ( 1 ; 2 )`; `Extend ( 1 ; 2 )` | 1214; 1214; 1214; 3; 1214 (only a lone aggregate argument must be a field; a field parameter is checked before the count) |
| `Substitute ( "a" ; "a" ; "b" ; "c" )`; `Substitute ( "a" ; [ "a" ; "b" ; "c" ] )`; `Substitute ( "a" ; [ "a" ] )` | 1202; 1202; 1201 (parameters, or groups of exactly two) |
| `Substitute ( "a" ; [ "a" ; "b" ] ; "a" ; "c" )`; `Substitute ( "a" ; "a" ; [ "a" ; "b" ] )` | 1211 (not both) |
| `Substitute ( "a" ; [ "a" ; "b" ] ; )`; `Substitute ( "a" ; [ "a" ; "b" ] [ "c" ; "d" ] )` | 1204 at the `)`; 1207 |
| `JSONSetElement ( "{}" ; "a" ; 1 )`; with `; JSONNumber`; with `; JSONNumber ; "b"`; `JSONSetElement ( "{}" ; [ "a" ; 1 ] )` | 1201; {"a":1}; 1202; 1201 (as Substitute, groups of three) |
| `Evaluate ( "1" ; [ ] )`; `Evaluate ( "1+1" ; 1 )`; `Evaluate ( "1+1" ; [ 1 ] )` | 1; 1204 at 19:20; 1204 at 21:22 (a trigger that is not a field) |
| `Let ( [] ; 1 )`; `Let ( [ "x" = 1 ] ; 1 )`; `Let ( [ x = 1 ; ] ; x )` | 1; 1211; 1211 (inside `[ ]` a missing binding name is 1211) |
| `Let ( x = 1`; `While ( i = 0 ; i < 3 ; i = i + 1 )`; `Let ( x = 1 ; x ; 2 )`; `While ( ... ; i ; 4 )` | 1201; 1201; 1202; 1202 |
| `Let ( [ x = 1 2 ] ; x )`; `Let ( $x[1 = 1 ; 1 )` | 1204 |
| `$x[`; `$x[1`; `( 1 2 )`; `Let ( [ x = 1` | 1203; 1207; 1207; 1204 |
| `1 2`; `1,5`; `Abs ( -1 ) Abs ( 1 )`; `Pi ( )` (compile mode) | 1212 at 2:3; 1:3; 11:20; 3:6 (after a complete formula FileMaker selects the rest of it) |

Display (`Text::MakeLiteral`, from the engine's round trip of stored
formulas): every quote character is written with a `\`, and a `\` is
doubled only where it would otherwise escape what follows (a `\`, a quote,
`¶`, a return or the closing quote): the value `\\server\share` shows as
`"\\\server\share"`, `\;` as `"\;"`. A decimal-comma file shows `,5`.

Verification of `om-formula` against the engine (`cargo run -p om-formula
--example check_corpus -- --print | --compile`): its printer's text equals
`DBCalcScan::UnTokenize` (round-trip mode) for all 5,304 template formulas
and for 4,086 of 4,087 formulas of five MBS files with the design functions
registered (the other is in a decimal-comma file, which the comparison
printed with a point); and 602 lines (every expression above, the evaluation
batteries and the 6.4 table) compile to the engine's error, span, formatted
and Polish bytes.

Still open for an implementation: the display of a formula whose formatted
stream was stripped (10, item 7); and one MBS formula (fmSyntaxColorizer,
field 4) whose formatted stream holds the context field `On` as a symbol
(5.6): `om-formula` compiles its text to the stored Polish code but writes a
field token there (the engine could not compile the text without the
plug-in it calls).
