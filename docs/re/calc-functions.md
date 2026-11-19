# Calculation functions and Get selectors (FileMaker Pro 26)

Per-function behavior for a 1:1 clone of FileMaker's calculation language:
every function in the 19 categories of the Claris functions reference, every
`Get ( selector )`, the functions the engine has but the docs omit, and the
behavior the docs leave open, settled from the engine where possible.

The machine-readable companion is
[catalogs/calc-function-spec.json](catalogs/calc-function-spec.json) (one
entry per function, selector and keyword constant) and the test vectors are in
[catalogs/calc-function-vectors.tsv](catalogs/calc-function-vectors.tsv).
Neither keeps the Claris pages' prose: the JSON holds each page's link,
format, parameter names, return type, version and example expressions with
their results, and the one-line descriptions OpenMaker shows are FileMaker's
own UI strings in `catalogs/calc-functions.csv` and `get-selectors.csv`.
This document covers what those files can't hold: cross-cutting rules, edge
cases, the evidence behind them, and the unknowns with the exact expression
that would settle each one in an oracle run.

Out of scope here (other workstreams): the stored token encoding of formulas
(calc-encoding.md) and the shared value model: decimal arithmetic, text to
number/date/time conversion, comparisons, date ranges (calc-values.md). This
document notes only where a function deviates from those general rules.

Evidence: FileMaker Server 26.0.3.309 for Ubuntu, `libDBEngine.so` (build id
`db88cdc1...`, full DWARF) and `libSupport.so`, `libFMEngine.so` from the same
package; Claris FileMaker Pro help as published 2026-08-27 (MadCap build time
in `Data/HelpSystem.xml`). "Observation" means seen in code, data or the docs;
"inference" means concluded from them and not yet checked against a running
FileMaker. Addresses are virtual addresses in the Linux libraries.

## 1. Sources and how to regenerate

```
# 1. Claris pages (TOC-driven, sequential, 0.6 s apart; cached, re-runs fetch only missing pages)
python3 -I tools/claris_calc_docs_fetch.py re/work/calc-functions/claris-html
# 2. Structured extraction of every page
python3 -I tools/claris_calc_docs_parse.py re/work/calc-functions/claris-html re/work/calc-functions/docs-extract.json
# 3. Engine opcode constants from DWARF
python3 -I tools/dbopcode_table.py re/extracted/fms/deb/opt/FileMaker/lib/libDBEngine.so re/work/calc-functions/dbopcodes.tsv
# 4. Join everything and write the catalogs
python3 -I tools/calc_function_spec.py --docs re/work/calc-functions/docs-extract.json \
    --ops re/work/calc-functions/dbopcodes.tsv \
    --syms re/work/symbols/linux/libDBEngine.txt --fmsyms re/work/symbols/linux/libFMEngine.txt \
    --catalogs docs/re/catalogs --out docs/re/catalogs
```

The research judgments (tiers, host capabilities, compatibility overrides,
server notes, engine notes, extra vectors) live in
`tools/calc_function_annotations.py`. `tools/elf_fn_summary.py` prints a
function's callees, constants, referenced strings and source lines without its
body; every engine observation below was gathered with it, plus
`llvm-dwarfdump` and targeted `llvm-objdump`.

The help site is MadCap Flare output. `Data/Tocs/main_toc.js` plus three chunk
files give the full table of contents; the subtree under "Functions
reference" has 19 category pages, 230 function pages and 138 Get pages (388
entries). The fetch also caches the JSON overview (`json-functions.html`),
custom functions, variables, special characters, server-side scripts, and the
named constants pages. The cache lives in
`re/work/calc-functions/claris-html/` with `manifest.tsv` (URL, status,
size, SHA-256, time) and `toc-functions.tsv`.

### 1.1 Compatibility data

Unlike script steps, function pages have **no compatibility table**
(observation: no page under the functions reference has one, and the
WebDirect, Data API, Server, Cloud and OData guides have no per-function
list either; the WebDirect guide only points back to the Pro help). The
`compatibility` object in the JSON is therefore derived:

- default `yes` for all seven products, basis "every product links the same
  calculation engine" (inference: FileMaker Server, WebDirect, the Data API,
  OData and CWP all evaluate with the same DBEngine; see architecture.md 7);
- overridden by the product-specific sentences of each page (for example
  "In FileMaker WebDirect, this function is not supported and returns 0"),
  which are kept verbatim in `compatibility_notes`;
- UI, device and printing Get selectors are `partial` on host engines
  (inference: no window, device or printer there; expect empty or 0);
- hand overrides for CoreML, Vision, mobile and layout functions.

Values: `yes`, `partial` (supported with documented or inferred differences),
`no` (not supported; the returned value is in the notes), `unknown`.

## 2. Reconciliation: docs, string resources, engine

Join keys (observation): each Claris page's purpose sentence is a MadCap
variable named after the engine opcode, for example
`ui-strings-functions.kDBLeftOp`; DWARF declares
`const Draco::DBOpcode kDBLeftOp = 87` in `DBOpTbl.h`, and 87 is also the
index in string resource 130 (`catalogs/calc-functions.csv`). Get selectors
are `kDB...Op = 1000 + index` in resource 131 (`kDBBaseGetParameterOp` = 1000
in `DBCalcEngine.h`); newer functions use per-arity bases (var-args 2300,
zero 2900, one 3200, two 3500, three 3800, four 4100). How opcodes are stored
in formulas is calc-encoding.md's subject.

| Set | Count | Notes |
|---|---:|---|
| Documented function pages | 230 | every one maps to an engine opcode or an FMEngine external function |
| Documented Get pages | 138 | every one maps to a resource-131 index |
| Catalog functions (`kind=function`) | 236 | 230 documented + `Get` + 5 undocumented JSON functions |
| Catalog Get selectors (named) | 140 | 138 documented + 2 hidden |
| Keyword constants | 52 | `and or not xor True False`, styles, code sets, JSON types, path formats, `ValueNumber`, `ROWID`... |

Functions the engine has and the docs don't (observation: string resources
140/144 and `DBDataArg` methods; none appears in the Claris reference):

| Function | Opcode | Catalog signature and description |
|---|---:|---|
| `JSONUniqueValues` | 2313 | `JSONUniqueValues ( json ; keyOrIndexOrPath )` - JSON array of the unique values of the array at the path |
| `JSONRemoveValues` | 2314 | `JSONRemoveValues ( json ; keyOrIndexOrPath ; jsonValuesArrayToRemove )` - the array without any of the given values |
| `JSONValueExists` | 3805 | `JSONValueExists ( json ; keyOrIndexOrPath ; value )` - whether value occurs in the array at the path |
| `JSONValueCount` | 3806 | `JSONValueCount ( json ; keyOrIndexOrPath ; value )` - occurrences of value in the array |
| `JSONAnyValuesExist` | 3807 | `JSONAnyValuesExist ( json ; keyOrIndexOrPath ; jsonValuesArrayToFind )` - whether any of the values occurs |
| `Get ( DBEngineVersion )` | 1087 | name only, no signature or description (hidden) |
| `Get ( StudioDatasource )` | 1138 | name only, no DWARF constant (hidden; Claris Studio data sources) |
| `IndexItems` | 2300 | engine-only (`DBDataArg::IndexItems`); resource 140 index 0 is `#UnUsed#`, so it has no name |

The five JSON functions are reachable by name in a formula if the parser
accepts every catalog name (inference; the encoding workstream should check
whether any shipped formula uses them). All five resolve the path with
`DBData::GetResolvedJsonPath`, read array elements with
`Json::Value::asString`, and compare with
`TextBuffer::IsMatchWithLocale(..., Locale, bool)` (observation,
`DBDataArg::JSONValueExists` @0xddee20, `JSONValueCount` @0xddf840,
`JSONAnyValuesExist` @0xddf260, `JSONUniqueValues` @0xddfca0,
`JSONRemoveValues` @0xde05e0), so element equality is a locale text match on
each element's string form, not JSON structural equality (inference).

Documented functions with no `kDB` constant in the server's DWARF (they exist,
but outside DBEngine's opcode table):

- the 26 Design functions: string resource 134, registered at run time by
  FMEngine `DesignFunctions.cpp` (`InstallDesignFunction`,
  `gDesignFunctions`) through the plug-in function interface
  (`short f(short, DBExpressionEnv const&, DBDataVect const&, DBData&)`); the
  handlers are `DatabaseNames`, `FieldLayoutInfo` (FieldBounds,
  FieldRepetitions, FieldStyle), `FieldIDnNames`, `FieldInfo` (FieldType,
  FieldComment, FieldAnnotation, FieldDisplayNames, GetNextSerialValue),
  `LayoutIDnNames`, `RelationIDnNames` (TableIDs/TableNames), `RelationInfo`,
  `ScriptIDnNames`, `ValueListIDnNames`, `ValueListItems`, `WindowNames`,
  `LayoutObjectNames`, `BaseIDnNames`, `BaseTableInfo` (FMEngine 0x9f57a0 to
  0x9fa770; the grouping is inferred from the names);
- `Location`, `LocationValues`, `RangeBeacons`: resource 156, FMEngine
  `Draco::Location` etc. (0xa5b860...), same registration;
- eight Get selectors whose constants are absent from this server build:
  `HostApplicationVersion` (1090), `TriggerExternalEvent` (1116),
  `TouchKeyboardState` (1117), `RegionMonitorEvents` (1119), `CacheFileName`
  (1130), `CacheFilePath` (1131), `SystemStorageAvailable` (1137),
  `GuidedAccessState` (1141). Their numbers are 1000 + resource-131 index
  (inference, consistent with all 130 others).

Docs variable names that differ from the DWARF names (the JSON maps them):
`kDBGetHeightOp`/`kDBGetHeight`, `kDBGetWidthOp`/`kDBGetWidth`,
`kDBVerifyContainerOp`/`kDBVerifyContainer`,
`kDBJsonGetTypeOp`/`kDBJsonGetElemTypeOp`,
`kDBGeTokenCountOp`/`kDBGetTokensCountOp`,
`kDBUTCmicroSecsOp`/`kDBUTCMicroSecsOp`,
`KDBTriggerKeystrokeOp`/`kDBTriggerKeystrokeOp`,
`kDBLastErrorDetailOp`/`kDBLastExternalErrorDetailOp`,
`kDBLLMLastStepTokensUsedOp`/`kDBLastStepTokensUsedOp`,
`kDBTransactionOpenStateOp`/`kDBTransactionStateOp`.

Nothing documented is missing from the engine. Obsolete names (resources
137, 138: `Leftb`, `Lengthb`, `Middleb`, `Positionb`, `Replaceb`, `Rightb`,
`RelationNames`, `RelationIDs`, `Status(CurrentGroups)`,
`Status(CurrentRecordID)`) map to current functions and matter only for
converting old files.

Where each function is implemented: `engine.implementation` in the JSON. Most
are `Draco::DBDataArg::<Name>`; control flow and context functions are
`DBCalcSolver` methods (`SolveIf`, `SolveCase`, `SolveChoose`, `SolveLet`,
`SolveLoop`/`SolveLoopIf`/`SolveLoopRewind` for While, `SolveEvaluate`,
`SolveGetField`, `SolveValidExpr`, `SolveLookups`, `SolveSetPrecision`,
`SolveSetRecursion`, `SolveAggregates`); every Get selector goes through
`DBCalcSolver::SolveGetFunction` (@0xe451d0), which asks the host environment
through a virtual call, so selector values come from the embedding
application, not from DBEngine.

## 3. Cross-cutting rules

These apply to many functions; per-category sections only repeat exceptions.

### 3.1 Integer arguments

Counts, positions, precisions and indexes (`Left`, `Right`, `Middle`,
`Replace`, `*Words`, `*Values`, `GetValue`, `Round`, `Truncate`,
`WeekOfYearFiscal`, `GetRepetition`...) are converted with
`FixPt::AsLongLong` (libSupport @0x170fa0), which uses only the integer limbs
of the decimal and applies the sign afterwards: **truncation toward zero**
(observation). `Left ( "abcdef" ; 2.9 )` is `ab`; `Middle ( "abcdef" ; 2.9 ;
2.9 )` is `bc`.

An empty argument (the number's empty flag at `FixPt+0xc`) becomes 0 before
the call (observation in `Left` @0xdce880, `Middle` @0xdceb00).

The text functions then pass the count to `TextBuffer::NextGraphemes(start,
count)` as an **unsigned 32-bit** value (observation). Consequences
(inference, to confirm with the oracle):

- a negative count becomes about 4 billion, so it means "to the end":
  `Left ( "abcdef" ; -1 )` is expected to return `abcdef`, and `Middle (
  "abcdef" ; 3 ; -1 )` returns `cdef`;
- counts are taken modulo 2^32: `Left ( "abc" ; 4294967296 )` is expected to
  return empty.

`Middle` treats an empty or <= 0 start as 1 (observation, explicit test at
@0xdceb65; matches the docs' "If start <= 1").

### 3.2 Characters are grapheme clusters

`Length`, `Left`, `Right`, `Middle`, `Replace`, `Position` (result),
`Filter` count and cut **extended grapheme clusters**, not UTF-16 units or
code points (observation: `TextBuffer::NextGraphemes`, `PreviousGraphemes`,
`CountGraphemes`, `GetGraphemeCountFromPosition`, `GetGrapheme`,
`FindGrapheme`). `Length ( "e" & Char ( 769 ) )` is 1 and a surrogate pair
such as `Char ( 66560 )` (U+10400) is one character. `TextBuffer` keeps a
"simple text" flag (`+0x38`); when set it skips grapheme segmentation (a
fast path, same result). The segmentation is legacy, not extended: a
base character absorbs following combining marks and a Hangul jamo
sequence is one character, but emoji ZWJ sequences and regional-indicator
pairs are not joined (oracle U-3: `Length ( "👨‍👩‍👧" )` = 5,
`Length ( "🇯🇵" )` = 2, decomposed `각` = 1, `e` + U+0301 = 1).

`Length` of container data returns the byte size of the main stream
(`DBBinaryData::GetMasterSize`, observation), as documented.

### 3.3 Case sensitivity

| Case-insensitive (locale case folding) | Case-sensitive |
|---|---|
| `Position`, `PatternCount` (`TextBuffer::FindFoldingCase`, `FindPrevLiteralFoldingCase`), `FilterValues` (`IsMatchFoldingCase`), the undocumented JSON value functions (`IsMatchWithLocale`) | `Substitute` (`FindLiteral`), `Exact` (`TextBuffer::operator==`), `Filter` (`FindGrapheme`) |

Folding uses a `Locale::Type`; `PatternCount` passes type 0 (observation).
It folds case only: not diacritics, width or `ß` (oracle U-4:
`Position ( "café" ; "CAFE" ; 1 ; 1 )` = 0, `... "CAFÉ" ...` = 1,
`PatternCount ( "Straße" ; "SS" )` = 0, halfwidth `ｱ` vs `ア` = 0).

### 3.4 Lists of values

A "value" ends at any character whose Unicode category entry in
libSupport's table (`UnicodeCategory.cpp`, `sCategoryPage00` ...) is 8 or 9
(`TextBuffer::NextValueBreak` @0x213a10 tests `entry & ~1 == 8`). Reading
the table (observation): U+000A and U+000D (9), U+2028 (8), U+2029 (9) are
separators; U+0085 (NEL), U+000B, U+000C, U+001C-U+001F are not. This matches
the ValueCount page's note. No code pairs CR with a following LF, so `"a" &
Char(13) & Char(10) & "b"` holds three values, the middle one empty
(oracle U-5: `ValueCount` = 3, `GetValue ( ... ; 2 )` = empty; NEL is not
a separator).

`LeftValues`, `MiddleValues`, `RightValues` append CR (U+000D) after every
returned value (observation: `Text::Append` of 13); `GetValue` returns the
value without one (docs). `FilterValues`, `SortValues`, `UniqueValues` end
each value with CR (docs).

Words (`WordCount`, `LeftWords`, `MiddleWords`, `RightWords`) use
`TextIterator::FindWord`, `FindWordFromEnd`, `GetNextWord` and
`TextBuffer::FindNextWordEnd(..., International)`, which first split at
value breaks and then apply the locale's word rules (observation of the
call graph; the rule tables are not decoded, U-6).

### 3.5 Styled text

Functions that rebuild text through `Text` objects keep character styles of
the characters they keep: `Left`/`Middle`/`Right` delete ranges from a copy
(`Text::Delete`); `Substitute` and `Replace` translate the styles of the
inserted text (`Text::TranslateCharacterStyles`, `StyleTranslator`) so the
replacement takes the style at the insertion point (inference from the
callees). `Exact` compares characters only (`TextBuffer` has no styles;
inference). Text formatting functions are in 5.2.

### 3.6 Errors

The engine reports errors through `Error::SetErrorIndexInternal` and the
solver turns them into the `?` result (calc-values.md owns the general
rule). Function-specific error texts:

- JSON functions return `"? "` followed by the parser or path message, with
  every newline replaced by CR (U+000D) and a trailing newline removed
  (observation: `DBData::HandleJsonParseError` @0xe9cb40 prepends `"? "`,
  calls `Unicode::ReplaceAllNewLines(..., 13)` and erases). Path errors use
  the text `Incorrect key, index, or path` (observation, string referenced by
  `DBData::GetResolvedJsonPath` @0xe9cfc0).
- `ExecuteSQL` returns `?` on any error; `ExecuteSQLe` returns `?` plus the
  message (docs).
- `Code` returns `?` for more than 80 characters (observation:
  `kMaxTextSizeForCode = 80` in `DBCalcEngine.cpp`, compared in
  `DBDataArg::Code` @0xde8f90; docs say "too many characters"). 80 x 5
  digits = 400 = `kMaxUserPrecision` (FixPt.h).

### 3.7 Locale

Results that depend on the **file's** locale (the locale stored with the
file, not the OS, inference from `DBFileUser` locale use): `DayName`,
`MonthName` (names via a locale virtual call), `Upper`/`Lower`
(`TextBuffer::FoldCase(direction, Locale::Type)`), `Proper`
(`TextBuffer::ProperCase(Locale)`), the text-to-date/time/number conversions,
`SerialIncrement` (`IncrementSerialNumber(..., Locale)`), and the default
collation of `SortValues`/`UniqueValues`.

### 3.8 Number representation by function

Most numeric functions compute in FileMaker's decimal `FixPt` (value model).
Exceptions (observation):

- `Sin`, `Cos`, `Tan`, `Atan`, `Asin`, `Acos` convert to IEEE double
  (`FixPt::Sin()` etc. return double), compute, and convert back with
  `FixPt::operator=(double)`. Results have double precision, so a clone must
  use the same libm-quality functions and the same double-to-decimal
  conversion (calc-values.md) to match digit for digit.
- `Random` is a double (3.9).
- `Exp`, `Ln`, `Log`, `Lg`, `Sqrt` (as `Power(x; 0.5)`), `Pi`, `Degrees`
  (`x * 180 / Pi`), `Radians` (`x * Pi / 180`) and the financial functions
  stay in `FixPt` with extra precision (`SetExtraPrecision`).

### 3.9 Random

`DBDataArg::Random` @0xdbdf10 (observation): a shared 64-bit state
`sRandSeed` advanced twice by `s = s * 0x045C1E2FF69EA0FD + 0xC90F7 (mod
2^64)`; the result is `((s1 & 0x1FFFFF00000000) | (s2 >> 32)) * 2^-53` as a
double in [0, 1), then converted to a decimal number. The seed is set by
`DBCalc::SetRandSeed(long)`; where and from what it is seeded is not
established (U-9). Exact reproduction of FileMaker's sequence is not a goal;
the range and the 53-bit granularity are.

## 4. Text functions (39)

Documented behavior is in the JSON; additions and settled ambiguities:

- **Left / Right / Middle / Replace**: 3.1 and 3.2. `Replace` positions are
  graphemes; start and count are truncated toward zero.
- **Position ( text ; searchString ; start ; occurrence )**: case-insensitive
  (3.3); negative occurrence searches backward from start (docs) with
  `FindPrevLiteralFoldingCase`; result is a grapheme position (1-based) or 0.
  Unknown: start <= 0 and occurrence = 0 (U-7).
- **PatternCount**: case-insensitive; matches do not overlap (after a match
  the search resumes at match + length of searchString); an empty
  searchString returns 0 (observation, `PatternCount` @0xde9c80: the length
  test before the loop and the advance by the search length).
  `PatternCount ( "aaaa" ; "aa" )` is 2.
- **Substitute**: case-sensitive literal; non-overlapping, left to right,
  replacements are not rescanned (`FindLiteral` then `Text::Replace`,
  inference from the loop in `InternalSubstitute` @0xdea2e0). The list form
  `Substitute ( text ; [a ; b] ; [c ; d] )` applies each pair in order to
  the result of the previous one (`DBDataArg::Substitute` @0xdea5e0 calls
  `InternalSubstitute` once per pair; the format line in the docs does not
  show the list form).
- **Exact**: compares with `DBData::ExactEqual(type)`; for text, characters
  only (case-sensitive, styles ignored).
- **Filter**: keeps each grapheme of textToFilter that occurs in filterText
  (exact grapheme match, case-sensitive), in original order.
- **FilterValues**: case-insensitive value match; converts by the type of
  the values (the function converts to number, date, time and timestamp as
  needed; observation of callees), each result value ends with CR.
- **Trim**: removes leading and trailing characters whose
  `Unicode::ConvertToASCII` is U+0020 (observation, `DBDataArg::Trim`
  @0xdd2060 and libSupport `ConvertToASCII` @0x242fb0). That is U+0020 and
  every code point >= U+00A0 whose category is 7 (space separators such as
  U+2000-U+200A and U+3000). Not removed: tab, CR, LF. **Correction from
  the oracle (U-20):** U+00A0 *is* removed (`Length ( Trim ( Char ( 160 ) &
  "x" ) )` = 1), so the category-11 reading of the table, or the path
  through `ConvertToASCII`, is wrong for U+00A0; implement from the oracle
  result. U+3000 is removed and a trailing tab is kept (`Length ( Trim (
  Char ( 12288 ) & "x" & Char ( 9 ) ) )` = 2).
- **TrimAll**: documented rules (roman/non-roman boundary at U+2F00,
  trimType 0-3); `DBDataArg::TrimAll` @0xe06460. Not decoded further (U-8).
- **Upper / Lower / Proper**: locale case mapping (3.7). Proper's word start
  rule is in `TextBuffer::ProperCase` (libSupport @0x214070), not decoded
  (U-6).
- **Quote**: `Text::MakeLiteral` then surrounds with `"` (observation,
  `Quote` @0xdcd420). MakeLiteral escapes `"` and `\` with a backslash and
  maps CR to the pilcrow `¶`, escaping an existing `¶` as `\¶` (inference
  from the constants 13, 182, 34 and 92 in `Text::MakeLiteral`, libSupport
  @0x2055c0; the constant set also includes tab and U+E004; exact table
  U-10). Docs: "Special characters within text are escaped appropriately."
- **Char / Code**: five-digit groups, low group = first character (docs).
  Code points above 99,999 (for example U+1F600 = 128512) cannot be produced
  by `Char`, because 128512 splits into the groups 28512 and 1 (inference
  from the documented rule, confirm with `Code ( Char ( 128512 ) )`).
  `Code` limit: 3.6.
- **GetValue**: index truncated toward zero; behavior for index <= 0 not
  established (U-7).
- **SortValues / UniqueValues** (`DBDataArg::SortOrUniqueValues`
  @0xdd0cb0): datatype 1-5 (negative = descending), locale names as
  documented; unrecognized datatype sorts as text ascending (docs).
- **SerialIncrement**: `IncrementSerialNumber(unistring, FixPt, Locale)`
  (observation); documented rules (the last numeric run is incremented,
  width kept).
- **GetAsURLEncoded**, **GetAsCSS**, **GetAsSVG**: documented; GetAsCSS/SVG
  are tier 1 but rarely used.
- **GetAsText / GetAsNumber / GetAsDate / GetAsTime / GetAsTimestamp /
  GetAsBoolean**: the conversions are the value model's (calc-values.md).
  `GetAsBoolean` is a virtual `DBData` test (observation) whose rules belong
  there too.

## 5. Other categories

### 5.1 Number (18), Trigonometric (9), Financial (4)

- **Round ( number ; precision )**: `FixPt::Round(AsLongLong(precision))`;
  "always rounds up at 0.5" (docs) - the direction for negative halves
  (`Round ( -2.5 ; 0 )`) is the value model's FixPt::Round (U-11).
- **Truncate**: `FixPt::Truncate(AsLongLong(precision))` after
  `RemoveExtraPrecision`; **Int**: `Truncate(0)`, toward zero
  (`Int ( -2.7 )` = -2).
- **Div**: `Divide` then `FixPt::Floor`: floor division (`Div ( -7 ; 2 )` =
  -4). **Mod**: `FixPt::Mod`; the sign convention is the value model's
  (docs examples give the sign of the divisor; U-11).
- **Floor / Ceiling / Sign / Abs**: FixPt; `Abs` also accepts time and
  timestamp values and returns a positive duration (observation:
  `DateTime::IsNegative` in `Abs` @0xde3420).
- **Factorial ( number {; numberOfFactors} )** and **Combination**: falling
  factorial `FixPt::FallFact`; `Combination` returns `?` via
  `DBDataArg::SetToNaN` for invalid input (observation).
- **SetPrecision**: `DBCalcSolver::SolveSetPrecision`; FixPt limits
  `kDefltFixedPrecision = 16` and `kMaxUserPrecision = 400` (observation,
  FixPt.h). The docs say every function except the trigonometric ones
  honors it, which matches 3.8. Values outside 16-400 are presumably clamped
  (inference, U-21).
- **Random**: 3.9. **Trigonometric**: 3.8.
- **FV, PMT, PV**: `FixPt::Fv`, `Pmt`, `Pv` with extra precision; empty
  arguments give 0 (observation: `FixPt::Zero` path after `IsEmpty`).
  Zero interest rate behavior is not established (U-12).
- **NPV ( payment ; interestRate )**: payments come from a repeating field,
  related values or a list via `NpvAggregator` (observation), so it is tier 2.

### 5.2 Text formatting (10)

`TextColor`, `TextFont`, `TextSize`, `TextStyleAdd`, `TextStyleRemove`,
`TextColorRemove`, `TextFontRemove`, `TextSizeRemove`, `TextFormatRemove`,
`RGB`. Pure functions over styled text; the result keeps the text and
changes only style runs. `RGB` returns `red * 65536 + green * 256 + blue`
(docs). Style keywords (`Plain`, `Bold`, ... `AllStyles`, `HighlightYellow`)
and font scripts (`Roman`, `Greek`, ... `Other`) are keyword constants with
their own opcodes (170-200; JSON `kind=constant`). `TextColor` is not
supported in WebDirect (docs). The style model itself (runs, inheritance,
how styles survive concatenation) needs its own spec; the value-model
workstream should state whether it owns it.

### 5.3 Date (10), Time (4), Timestamp (1)

- **Date ( month ; day ; year )**: `DateTime::SetNormalizedDate(FixPt,
  FixPt, FixPt)`: out-of-range months and days roll over, zero and negative
  values allowed, decimals truncated (docs, consistent with the FixPt
  arguments). Proleptic Gregorian arithmetic with /100 and /400 rules
  (observation of constants in `DateTime::NormalizeAsDate` @libSupport
  0xfcf10). Valid range is the value model's (docs: years 1-4000).
- **Time ( hours ; minutes ; seconds )**: `DateTime::SetNormalizedTime`;
  fractional seconds kept (FixPt), overflow carries; negative results allowed
  (docs).
- **Hour / Minute / Seconds** of a time: via
  `DateTime::NormalizeAsElaspedTime`, so `Hour` of 30:00:00 is 30 (elapsed
  time, inference from the name; U-13).
- **WeekOfYear**: `DateTime::WeekOfYear`; partial weeks at both ends count
  as full weeks, so 1-54 (docs). The first day of the week is not stated;
  Sunday is the expectation (inference, U-22).
- **WeekOfYearFiscal ( date ; startingDay )**: startingDay truncated, must be
  1-7 (observation: range test with 7 in `WeekOfYearFiscal` @0xdf0950);
  out of range gives an error result (U-14).
- **DayName / MonthName**: file locale (3.7).
- **Timestamp ( date ; time )**: `DateTime::SetDate` + `SetTime`, then
  normalized as a date, so a time beyond 24 hours rolls the date (inference;
  docs example confirm with `Timestamp ( Date ( 1 ; 1 ; 2024 ) ; Time ( 25 ;
  0 ; 0 ) )`).

### 5.4 Container (24)

Pure byte functions (tier 1): `Base64Encode` (text as UTF-8 first; RFC
2045 layout: 76-character lines, CRLF; docs), `Base64EncodeRFC` (1421: 64
chars + CRLF, 2045: 76 + CRLF, 3548 and 4648: no breaks, 4880: 76 + CRLF +
CRC-24; unrecognized numbers fall back to 4648; docs), `Base64Decode`, `HexEncode`,
`HexDecode`, `CryptDigest`, `CryptAuthCode`, `CryptEncrypt`,
`CryptDecrypt`, `CryptEncryptBase64`, `CryptDecryptBase64`,
`CryptGenerateSignature`, `CryptVerifySignature`, `TextEncode`,
`TextDecode`. They work on the container's main stream; the result
container's file name and type follow the docs (for example
`encrypted.data`). Algorithms and encodings are the documented lists; the
crypto is OpenSSL in the shipped product (architecture.md). These matter for
integrations (hashes, signatures, Base64), so implement them early even
though they are marked non-core.

Analysis functions (tier 3): `GetContainerAttribute` (attribute names in
`kContainerAttributeStrings`, `DBCalcEngine.cpp`), `GetHeight`, `GetWidth`,
`GetThumbnail`, `VerifyContainer`, `ReadQRCode` (Vision on macOS, ZBar
elsewhere; Linux from Ubuntu 22.04), `GetLiveText`, `GetLiveTextAsJSON`
(Vision, macOS 15 / iOS 18), `GetTextFromPDF` (PDF text; on Windows and
Linux scanned PDFs are not recognized, docs).

### 5.5 JSON (10 documented + 5 undocumented)

Implementation (observation): jsoncpp. Parsing in
`DBData::GetAsConstJsonValue` (@0xe99420) uses
`Json::CharReaderBuilder::setDefaults` then `strictMode`. In jsoncpp,
strict mode means: no comments, **the root must be an object or array**,
no dropped null placeholders, no numeric keys, no single quotes, **extra
text after the value fails**, and **duplicate keys are rejected**. Hence
(inference, exact message text to confirm, U-15):

- `JSONGetElement ( "1" ; "" )` fails with "A valid JSON document must be
  either an array or an object value.";
- `JSONGetElement ( "{\"a\":1,\"a\":2}" ; "a" )` fails with "Duplicate key:
  'a'".

Error result format: 3.6 (`"? "` + jsoncpp's `* Line L, Column C` message
with CR line breaks).

Paths (`keyOrIndexOrPath`): resolved by a FileMaker-modified jsoncpp
`Json::Path` (`Json::Path::makePath` @0x1617190 tests `.` `[` `]` `'` `+`
`:`, observation). Syntax is the docs' (dot and bracket notation, `[n]`,
`['name']`, `[:]` last element, `[+]` append position). A number argument is
used as an array index when it is a whole number (`FixPt::IsWholeNumber`,
then `Json::Value::operator[](unsigned)` in `GetResolvedJsonPath`,
observation).

Output: `Json::FMStreamWriterBuilder` = jsoncpp `StreamWriterBuilder` defaults
plus `emitUTF8 = true` (observation, constructor @0xe0c0a0), so non-ASCII
characters are written as UTF-8, not `\u` escapes. jsoncpp objects are
`std::map`s, so **keys come out sorted** by byte order of their UTF-8
encoding (docs: "sorted alphabetically by key"; byte order is the
inference). Compact output (`JSONSetElement`, `JSONGetElement` of an object)
has no spaces (`{"a":1,"b":2}`); `JSONFormatElements` uses jsoncpp's styled
writer: tab indentation and `" : "` between key and value (docs example and
jsoncpp default `indentation = "\t"`), with CR line ends (inference: the
newline-to-CR constant 13 in `DBData::SetTextFromJsonValue` @0xe9b8e0).
Numbers are written with `valueToString(double, ...)`; the constant 15 in
`SetTextFromJsonValue` gives 15 significant digits, `%.15g` style
(oracle U-16): `1/3` gives `{"a":0.333333333333333}`, `1e20` gives
`{"a":1e+20}`, `.1 + .2` gives `{"a":0.3}`, `12345678901234567890` gives
`{"a":1.23456789012346e+19}`, and the text `"abc"` as JSONNumber gives
`{"a":0}`. JSONMakeArray's example turns `18.0` into `18`
(docs).

Type constants (observation, `DBCalcEngine.cpp`): `kJSONRawValue` 0,
`kJSONStringValue` 1, `kJSONNumberValue` 2, `kJSONObjectValue` 3,
`kJSONArrayValue` 4, `kJSONBooleanValue` 5, `kJSONNullValue` 6 - the values
of the keywords `JSONString` ... `JSONRaw`, of `JSONGetElementType`, and of
`JSONParsedState` (1-6 valid, -1 invalid, 0 not parsed).

Caching (docs, `json-functions.html`): one implicit parsed-JSON cache per
thread (`DBData::JSONThreadCacher`), plus explicit `JSONParse` results kept
with the value. The cache is invisible except to `JSONParsedState`; a clone
needs only the observable states.

### 5.6 Logical (20)

- **If / Case / Choose**: short-circuit (only the selected branch is
  evaluated; `ConstructCase`/`PrepareForCase` build lazily evaluated
  branches, observation of the solver structure). `Choose` index is
  truncated toward zero; an index outside 0..n-1 returns empty (docs).
- **Let**: `SolveLet`; variables named with `$`/`$$` set script/global
  variables (docs, `using-variables.html`); `kDBSetVariableOp` (227) and
  `kDBGetVarRepOp` (228) are the internal operations for variable
  assignment and `$var[n]` repetitions (inference from names).
- **While**: `SolveLoop`, `SolveLoopIf`, `SolveLoopRewind`; default limit
  50,000 iterations, then `?` (docs); `SetRecursion ( expression ;
  maxIterations )` changes the limit for While and recursive custom
  functions; non-tail recursion may also stop on stack depth (docs).
- **Evaluate ( expression {; [fields]} )**: evaluates text as a formula;
  server-side evaluation accepts English function names only (docs).
  `EvaluationError` must enclose `Evaluate` to see syntax errors (docs);
  `kDBSolveEvalErrorOp` (527) is its solver operation.
- **IsEmpty, IsValid, IsValidExpression, GetAsBoolean, GetField, Lookup,
  LookupNext, GetNthRecord, GetSummary, Self, ExecuteSQL, ExecuteSQLe**:
  docs; tier 2 except IsEmpty/GetAsBoolean.
- **ExecuteSQL / ExecuteSQLe**: FileMaker's SQL engine (`FQL::ExecuteSQLCalc`,
  architecture.md 7), specified in [execute-sql.md](execute-sql.md). Both
  set `EvaluationError` (8310 for syntax errors, bad date literals and WITH
  TIES without ORDER BY; 8309 for other SQL errors); `ExecuteSQL` returns
  `?`, `ExecuteSQLe` returns `? ERROR: ...` text. `ROWID` and `ROWMODID` are
  keyword constants (2915, 2916).

### 5.7 Aggregate (10) and Repeating (3)

`DBCalcSolver::SolveAggregates` with one `DBDataArg` method each
(`Average`, `Count`, `Max`, `Min`, `Stdev`, `Stdevp`, `Sum`, `Variance`,
`VarianceP`, `AggregateList` for `List`). Arguments: a related field (all
related records), a repeating field (all repetitions), or several fields
or expressions. `Max`/`Min` keep the argument type (text, number, date,
time, timestamp; docs). Empty values are skipped (inference, as in
summary fields). `Extend`, `GetRepetition`,
`Last` work on repetitions (`Get(CalculationRepetitionNumber)` is the
current repetition).

### 5.8 AI (14), Miscellaneous (11), Design (26), Mobile (5), Japanese (12)

- **AI**: provider calls (`GetEmbedding`, `GetRAGSpaceInfo`, the LLM
  catalog in architecture.md 4.1) are tier 3. Pure vector math
  (`CosineSimilarity`, `AddEmbeddings`, `SubtractEmbeddings`,
  `NormalizeEmbedding`, `GetEmbeddingAsText`, `GetEmbeddingAsFile`) and
  `GetTokenCount` (tiktoken `cl100k_base`) are tier 1 but need the
  embedding container format (`.fve`, `emb_` prefixes appear in DBEngine
  strings), decoded from engine runs in calc-values.md 13 (value-core
  follow-ups); `GetTokenCount` needs the tokenizer data and is the host's. `ComputeModel`/`GetModelAttributes` need CoreML
  (macOS/iOS only); `PredictFromModel` uses models from `Configure Regression
  Model`.
- **Miscellaneous**: `GetFieldName`, `GetBaseTableName` (schema),
  `GetLayoutObjectAttribute` (attribute names in resource 146),
  `GetLayoutObjectOwnerInfo`, `LayoutObjectUUID`, `GetAddonInfo` (add-ons),
  `ConvertFromFileMakerPath`/`ConvertToFileMakerPath` (path formats
  `PosixPath` 2908, `URLPath` 2909, `WinPath` 2910),
  `GetRecordIDsFromFoundSet` (types `ValueNumber`, `ValueNumberRanges`,
  `JSONStringRanges`; constants 2912-2914), `GetPersistentData` and
  `ListPersistentDataIDs` (26.0).
- **Design**: schema introspection over open files; values are lists
  separated by CR. Each needs the schema and layout catalogs
  (schema-path.md, layout-model.md). FileMaker Server's answers are in
  "Design functions on FileMaker Server" below.
- **Mobile**: FileMaker Go only; FileMaker Pro returns empty for Location
  and LocationValues (docs). Sensor names: resource 162.
- **Japanese**: pure conversions (`Hiragana`, `Katakana`, `KanaHankaku`,
  `KanaZenkaku`, `RomanHankaku`, `RomanZenkaku`, `KanjiNumeral`,
  `NumToJText`, `DayNameJ`, `MonthNameJ`, `YearName` with era names) are
  tier 1 non-core; their documented examples are images, so the vectors
  carry `[image: alt text]` and are not machine-checkable. `Furigana` needs a
  reading dictionary (tier 3).

#### Design functions on FileMaker Server [O: script engine, 2026-10-29]

Every Design function was evaluated by FileMaker Server 26.0.3's script
engine (`tools/fms-script-harness`, batteries `s30-design-*`, generated by
`design_battery.py`) over the Contacts and Tasks templates, the test
project, FileMaker Server's sample, the MBS Event Management and
AppleTouchBar examples and a probe file generated from the Tasks template
(`design_probe.py`: every storage and field kind, comments, serial
numbers, value lists of every source, occurrences whose names sort apart
from their ids). 4,199 answers are fixtures
(`crates/om-data/tests/data/design`) that the desktop's host and a server
script session replay exactly. In file context (FileMaker Server's engine
with no session, `tools/fms-calc-harness -d`, battery `fn-design`) every
Design function answers empty but `DatabaseNames`.

- **File names.** `""` or the file's name in any case, with or without
  `.fmp12` in any case; anything else (a leading space, another file)
  answers empty, as for a file that is not open. Every other name
  (occurrence, base table, layout, value list, field, object) matches in
  any case. An argument that fails passes its error on (`TableNames ( 1/0
  )` is error 15).
- **DatabaseNames**: the open files' names without extension (a server
  session has its one file; also in file context). **WindowNames ( {file}
  )**: the session's window, named after the file.
- **TableNames, TableIDs**: every occurrence, other files' included,
  sorted by name in the file's collation (`_under`, `10 ten`, `9 nine`, `a
  lower`, `Assignees`, `Éclair`, `eclair two`, `P_A`, `Tasks`, `Zeta`,
  `日本`), the ids in the same order. **BaseTableNames, BaseTableIDs**: the
  same for base tables. Not the display order.
- **LayoutNames, LayoutIDs**: the layout catalog's display order without
  folders (member flag `0x01`) and folder ends (`--`, `0x02`); separators
  (layouts named `-`, `0x10`) and layouts hidden from the menu (`0x08`)
  stay (file-metadata.md 1.1). **ScriptNames, ScriptIDs**: the same rule
  over the script catalog (a folder keeps an empty step list; its flags
  decide). **ValueListNames, ValueListIDs**: display order.
- **FieldNames, FieldIDs ( file ; name )**: a layout of that name wins
  over an occurrence. A layout lists every field object in object order
  (the tree depth first, a container before its contents), one entry per
  object (a field shown twice is listed twice); fields of the layout's
  occurrence by name, others as `Occurrence::Field`, their ids as
  `occurrenceID::fieldID`; a field of another file's table has an empty
  name. An occurrence lists its table's fields in id order; `""` names the
  window's layout's occurrence.
- **FieldType ( file ; field )**: `Storage Type Indexing Repetitions`.
  Storage `Standard`, `Global` (a global data field), `StoredCalc` (also a
  global calculation), `UnstoredCalc`, `Summary`; types `Text`, `Number`,
  `Date`, `Time`, `Timestamp`, `Container`; `Indexed` when stored with an
  index (a field set to index automatically becomes `Indexed` once
  FileMaker builds it, for example for a value list); a summary of
  repetitions individually has its field's repetitions (`Summary Number
  Unindexed 3`). An unqualified name is a field of the calculation's
  occurrence; a run line after the script has none, so it answers empty
  there.
- **FieldComment**: the field's comment (`[T, 3, 5, F, 03]`).
  **FieldAnnotation**: the field's annotation (`30`) as stored, empty
  without one. **FieldDisplayNames**: empty unless "Customize field display
  names" is on (`29` bit `0x2`); then the result of the field's
  display-names formula (`31`), evaluated on the current record of its
  context occurrence, returned unchanged when it is JSON (`{"fm_common":
  "Given Name","fm_table_view":"First","my_short":"FN"}`, spacing kept;
  an array too) and empty otherwise. The object's keys `fm_common`,
  `fm_export`, `fm_sort` and `fm_table_view` name the field in FileMaker's
  dialogs and Table View; other keys are the developer's
  (file-metadata.md 1.2). **BaseTableComment ( file ; table )**:
  `[3, 16, 5, T, 03]`. **GetNextSerialValue**: the auto-enter serial's
  next value as text (`A0007`, `41`), the hosted file's current one
  (`A0013` after two new records with increment 3); empty for any other
  field.
- **FieldStyle ( file ; layout ; field )**, of the field's first object:
  `Standard`, `Scrolling` (an edit box with a scroll bar, field object
  flag `0x100`), or `Popuplist`, `Popupmenu`, `Checkbox`, `RadioButton`,
  `Calendar` followed by a space and the value list's name, which may be
  empty (`Calendar `). **FieldRepetitions**: `n vertical` or `n
  horizontal`, n the repetitions shown, horizontal when `numOfReps` is
  stored negative (`9 horizontal`, `1 horizontal`). **FieldBounds**: `left
  top right bottom rotation` in layout coordinates, rounded to whole
  points; a portal's fields as placed in its first row, inside the
  portal's left and top borders where it draws them (style `none` and
  `hidden` draw nothing): `+0, +1` for the templates' portals with top and
  bottom borders only, `+1, +1` for the FMS sample's.
- **RelationInfo ( file ; occurrence )**: one block per relationship of
  the occurrence, ordered by the related occurrence's id, blocks separated
  by an empty line: `Source:<file>` (another file's occurrence: its data
  source's name), `Table:<related occurrence>`, `Options:` with the related
  side's checkboxes `Delete`, `Create`, `Sorted` separated by spaces, then
  one line per predicate with the related side first and the operator read
  from it, in ASCII: `P_B::N <= Tasks::N`, `<>`, `>=`, `x` for a cartesian
  join. No space after the colons. A field of another file's table is
  empty on the related side and `<File Missing>` on the named side.
- **ValueListItems ( file ; list )**: custom values split at CR and LF
  (FileMaker's XML importer writes LF), dividers kept; a field's values
  each line of a text value apart, unique in any case (the first wins),
  in the field's index order (file-metadata.md 6.4). It answers only in a
  script: a run line after the script answers empty.
- **LayoutObjectNames ( file ; layout )**: the objects depth first in
  object order. A named object adds its name; a portal, tab panel, slide
  panel [I], popover [I] or button bar adds `<` before its contents and
  `>` after; a tab control adds nothing; a group adds an empty item before
  its contents. An item goes on a new line unless the text already ends
  with one, so empty items show only at the very end: `<¶>¶` when a group
  follows the last bracket (FMS sample, Task Details), `<¶>` otherwise. A
  named container gives its name, then `<`: `Assignee Portal¶<¶>`.

#### Layout objects (`GetLayoutObjectAttribute`) [O: script engine]

Battery `s32-layout-objects-*` (Contacts' Contact Details, Tasks' Task
Details and Task List): every attribute of the named buttons, a web viewer,
fields, a portal and button bar segments, 324 answers replayed by both
hosts (`crates/om-data/tests/data/layout-objects`). FileMaker Server's
session window has no toolbar, so its coordinates are layout coordinates;
FileMaker Pro's are screen points (the Claris reference; Open on
FileMaker Pro O5).

- Object and attribute names match in any case; a name or attribute that
  does not exist answers empty. `objectType`: `field`, `text`, `button`,
  `portal`, `web viewer`, ... (catalogs/layout-object-types.csv; a tab
  control and its panels are both `tab panel` [I]).
- `bounds`: `left top right bottom rotation`; `left`, `right`, `top`,
  `bottom`, `width`, `height`; `startPoint` `left top`, `endPoint` `right
  bottom`. A list layout's body object with no records has no bounds and
  no content (Task List's `Go to details.`). Repetition 0 is the whole
  field; past the repetitions shown, empty.
- `source`: a field's `Occurrence::Field`, a portal's occurrence name,
  empty for buttons; the web viewer's address answers empty on the server.
  `content`: a button's label (`Contacts`), a field's formatted value on
  the current record (empty without one).
- `enclosingObject`: the container's name (an unnamed button bar: empty);
  `containedObjects`: the named objects inside, in object order [I: none
  probed had any].
- `hasFocus`, `containsFocus`, `isFrontTabPanel`, `isFrontPanel`,
  `isObjectHidden`: `0` or `1`; `isActive` answers empty on the server.
  `UUID`: the object's `d8` attribute as `8-4-4-4-12` upper-case hex.
- `LayoutObjectUUID` outside a layout object's calculation: error 10.
  `GetLayoutObjectOwnerInfo` of an unknown id: the JSON object with
  `"Index" : 4294967295` and empty names.

## 6. Get selectors (138 + 2)

All selectors are answered by the host environment (2). The JSON gives each
one a group, tier, capabilities and a server note:

| Group | Tier | Count | Examples |
|---|---:|---:|---|
| session | 2 | 41 | AccountName, ApplicationVersion, SystemPlatform, HostName, UUID, FilePath |
| clock | 2 | 6 | CurrentDate, CurrentTimestamp, CurrentHostTimestamp, CurrentTimeUTCMilliseconds |
| context | 2 | 22 | FoundCount, RecordNumber, RecordID, LayoutName, WindowMode, ModifiedFields |
| script | 2 | 12 | ScriptName, ScriptParameter, ScriptResult, LastError, LastErrorDetail, LastErrorLocation |
| ui | 3 | 38 | Active*, Window*, Screen*, Trigger*, MenubarState, SystemAppearance |
| device | 3 | 6 | GuidedAccessState, TouchKeyboardState, RegionMonitorEvents, NetworkType |
| filesystem | 3 | 9 | DesktopPath, DocumentsPath, TemporaryPath, CacheFilePath |
| plugins | 3 | 2 | InstalledFMPlugins, InstalledFMPluginsAsJSON |
| printing | 3 | 3 | PrinterName, PageNumber, PageCount |
| ai | 3 | 1 | LastStepTokensUsed |

Server-side behavior (PSOS, schedules, Data API/OData scripts; docs
`running-scripts-on-server.html` and the selector pages): the script engine
is a separate client session with no client context (layout, found set,
record, globals, variables); hardware selectors describe the server machine;
UI selectors have no window. `Get ( ApplicationVersion )` identifies the
engine (`Server ...`, `FileMaker Data API Engine ...`, `OData Engine ...`,
`Go ...`, `Go_iPad ...`), which is how solutions branch. Per-selector notes
are in `client_server` and `compatibility_notes`. For OpenMaker the host
abstraction (scripting.md) must therefore let each host (desktop client,
server script engine, Data API) supply its own answers. The engine's
product type enum (`ApplicationInfo::AppType` in libSupport: kDeveloper 0,
kPro 1, kRuntime 2, kServer 3, kCWP 4, kMobile 5, kXDBC 6, kSASE 7, kIWP 8,
kWebIntgrPlat 9, kAdminServer 10, kOData 11, kFCC 12, kTool 99) is the
natural key for those answers (inference).

### 6.0 FileMaker 26.1.1 additions

[O: FileMaker Pro 26.1.1's string resources, catalogs/README.md "Added in
26.1.1" (the catalogs stay at 26.0.3, #338); behavior from the Claris
release notes, FileMaker Pro 26.1.1; nothing executed yet]

- **`Get ( SortOrder )`** (selector 142, stored `9b 8e`): the current sort
  order as a JSON object. The new DBEngine strings `sortOrder`,
  `tableName` and `summary` are candidates for its keys [I]; the shape is
  [U] until run.
- **`GetToolDefinitions ( provider ; functionName {; functionName } )`**
  (resource 140, index 18, stored `f9 12`, variable arguments): the named
  custom functions as tool definitions in JSON for the provider (OpenAI,
  Anthropic, Cohere or Google), for Generate Response from Model's Tool
  Definitions. Built by `DBDataArg::CreateToolDefinitions` (DBEngine
  0x18d3642). The definitions carry the function's and its parameters'
  descriptions (below).
- **Custom function descriptions:** Edit Custom Function takes a
  description of the function and of each parameter (Claris). They are kept
  in the custom function's existing description member `16/5/<id>/3`
  (`KDescription`): `DBFunction::GetComment` (0x1a911fc) returns its text
  up to the first `;`, with `<{?:!}>` standing for an escaped `;`
  (`UnEscapeSemiColon`), and `GetParamCommentList` (0x1a90f02) the rest
  [O code; I: that the parameters follow in order]. Save as XML writes them
  as `ParameterDescription` and `parametersComment` (FMEngine strings).
- **`Get ( InstalledFMPluginsAsJSON )`** adds `environment.pluginpath`, the
  plug-in folder as a FileMaker path (Claris; DBEngine string
  `pluginpath`).

### 6.1 Host matrix [O: server and file context, 2026-10-29]

What each OpenMaker host answers for each selector, and where the answer
comes from. The desktop client is FileMaker Pro's role (OpenMaker's
`om_data::host::SessionInfo` and the desktop's window); a server script is
Perform Script on Server or a schedule (FileMaker Server's script engine,
measured with `tools/fms-script-harness`, batteries `s31-get-server`,
`s31-get-account`, `s31-get-layouts`, and OpenMaker Server's
`scripts::host::get`); file context is a calculation with no session
window or script, as FileMaker Server recomputes stored calculations
(FileMaker Server's engine through `tools/fms-calc-harness`, battery
`fn-get`, the same with and without a table occurrence).

The server column gives FileMaker Server's value in the contacts-en
session (several values: it changes over the probe's steps: a fresh
session on Contact Details, New Record, Set Field, Commit, Go to Layout
Contact List, Sort, Find mode with two requests, Browse, error capture, a
transaction), then in brackets OpenMaker Server's answer: `=` the same
(replayed for the first seven steps, ignored test
`scripts::host::get::tests`), `runtime` the script runtime's own answer
on every host, or the difference. A server session's window is a 640 x
480 window named after the file, at 0, 0 on a 640 x 480 x 32 screen, with
no active field. The harness logs in with FMDeveloperTool's security
bypass, which answers `Guest` and `[Read-Only Access]` for the account;
`s31-get-account` logs in as Admin: `FileMaker File`, `[Full Access]`,
`fmreauthenticate10`, `-1` days, the rest unchanged. The fixture file is
`crates/om-server/tests/data/get/s31-get-server.contacts-en.tsv`.

A desktop cell whose source names "FileMaker Pro 6.3" gives FileMaker
Pro 26.0.3's own answer for a local file (6.3, with the 6.2 item in
brackets). These are measured answers. OpenMaker's desktop host gives
them, except where 9, "Oracle answers (desktop Get selectors and
s33)", says otherwise. Answers that only a hosted file gives are marked
[U]: they remain unmeasured (6.3).

| # | Selector | Desktop | Server script: FileMaker Server [OpenMaker] | File context | Source |
|---:|---|---|---|---|---|
| 1 | ApplicationVersion | `Pro 26.0.3` | `Server 26.0.3` [`Server <version>` (by design)] | `FMDeveloperTool 26.0.3.309 (08-...` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 2 | CurrentDate | local date | server clock [=] | clock / new | Claris reference / spec; server s31 |
| 3 | LastError | runtime | `0` [runtime] | empty | runtime; server s31 |
| 4 | ActiveFieldName | the focused field (`First Name`; `Number` in a portal); empty without one | empty / `Title` / `First Name` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O2); server s31 |
| 5 | FileName | file name | `work` [=] | `work` | Claris reference / spec; server s31 |
| 6 | FileSize | the file's current size in bytes, a multiple of 4096 | `577536` [=] | `573440` | FileMaker Pro 6.3 (O1); server s31 |
| 7 | FoundCount | window found set; in Find mode the number of requests | `0` / `1` / `2` [=] | `0` | Claris reference / spec, FileMaker Pro 6.3 (O3); server s31 |
| 8 | HostName | local file: the Mac's host name; hosted: the connection's [U] | `631bd1d27a4a` [the server's name] | `23112e0a22c1` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 9 | LayoutCount | listed layouts (no folders), display order | `2` [=] | empty | Claris reference / spec; server s31 |
| 10 | LayoutName | window layout | `Contact Details` / `Contact List` [=] | empty | Claris reference / spec; server s31 |
| 11 | LayoutNumber | position among listed layouts | `2` / `1` [=] | empty | Claris reference / spec; server s31 |
| 12 | SystemLanguage | `English` | `English` [=] | `English` | FileMaker Pro 6.3 (O1); server s31 |
| 13 | WindowMode | 0 Browse, 1 Find, 2 Preview (Layout mode not probed); 3 while printing is in progress (Claris reference; OpenMaker also answers 3 while building Save Records as PDF output [I]) | `0` / `1` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O3, O4); server s31 |
| 14 | MultiUserState | `0` for a local file; 2 for a client of a hosted file [U] | `0` [= (unhosted harness copy [U on a hosting server])] | `0` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 15 | PageNumber | `0` outside Preview; the page shown in Preview, also in a layout calculation | empty [=] | empty | FileMaker Pro 6.3 (O4); server s31 |
| 16 | SystemPlatform | `1` macOS (-2 Windows, 8 Linux) | `8` [=] | `8` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 17 | ActivePortalRowNumber | the portal row (`2`); `0` outside a portal and with the portal itself active (Go to Object, Go to Portal Row [ Select ] keeps the row) | `0` [=] | empty | FileMaker Pro 6.3 (O2); server s31, s35 (script-runtime.md 13.24) |
| 18 | PrinterName | ` ` (one space) with no printer configured [U: with one] | `#1223 ` [=] | error 1223 | FileMaker Pro 6.3 (O4); server s31 |
| 19 | TotalRecordCount | table record count | `0` / `1` / `2` [=] | `0` | Claris reference / spec; server s31 |
| 20 | RecordNumber | window record; in Find mode the request; `0` with no records | `0` / `1` / `2` [=] | `0` | Claris reference / spec, FileMaker Pro 6.3 (O3); server s31 |
| 21 | ActiveRepetitionNumber | `1` in a field (also a portal field); `0` without one, and with a portal row or the portal itself active | `0` / `1` [=] | empty | FileMaker Pro 6.3 (O2); server s31, s35 (13.24) |
| 22 | RequestCount | Find mode requests (`2`); kept after Enter Browse Mode without a find | `0` / `1` / `2` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 23 | ScreenDepth | `32` | `32` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 24 | ScreenHeight | screen height in points (`1080`) | `480` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 25 | ScreenWidth | screen width in points (`1920`) | `640` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 26 | ScriptName | runtime | runtime [runtime] | empty | runtime; server s31 |
| 27 | SortState | `0` / `1`; still `1` after New Record in a sorted set | `0` / `1` [=] | `0` | FileMaker Pro 6.3 (O3); server s31 |
| 28 | SystemVersion | macOS product version (`26.6.2`) | `24.04.5` [`/etc/os-release` VERSION] | `24.04.5` | FileMaker Pro 6.3 (O1); server s31 |
| 29 | CurrentTime | local time | server clock [=] | clock / new | Claris reference / spec; server s31 |
| 30 | UserCount | `1` for a local file; hosted [U] | `1` [=] | `1` | FileMaker Pro 6.3 (O1); server s31 |
| 31 | UserName | Settings' User Name: the name typed under Other, else the macOS account's full name (6.1.3) [O] | `Admin` [=] | `Admin` | FileMaker Pro 6.3 (O1); server s31 |
| 32 | AccountName | the account | `Admin` [=] | `Admin` | Claris reference / spec; server s31 |
| 33 | LastMessageChoice | runtime | `0` [runtime] | empty | runtime; server s31 |
| 34 | CurrentPrivilegeSetName | `[Full Access]` | `[Full Access]` [=] | `[Full Access]` | FileMaker Pro 6.3 (O1); server s31 |
| 35 | ActiveModifierKeys | `0` with no key held (Shift: unmeasured) | `0` [=] | empty | FileMaker Pro 6.3 (O2); server s31 |
| 36 | NetworkProtocol | `TCP/IP` | `TCP/IP` [=] | `TCP/IP` | Claris reference / spec; server s31 |
| 37 | RecordID | window record id; `0` with no record | `0` / `1` / `2` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O3); server s31 |
| 38 | RecordModificationCount | stored count; `0` on a new record and with no record | `0` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O3); server s31 |
| 39 | ActiveFieldContents | the active field's text, typing included (`Ann`); in a portal row the row's value, unsaved changes included; empty without a field | empty [=] | empty | FileMaker Pro 6.3 (O2); server s31, s35 (13.24) |
| 40 | FilePath | local file: `file:/<volume>/<path>/<file>.fmp12`; hosted `fmnet:/host/file.fmp12` [U] | `file:/tmp/work.fmp12` [`file:<path>`] | `file:/tmp/work.fmp12` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 41 | LastErrorDetail | runtime | empty [runtime] | empty | runtime; server s31 |
| 42 | LayoutAccess | `2` | `2` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 43 | RecordAccess | `2` | `2` [=] | `0` | FileMaker Pro 6.3 (O1); server s31 |
| 44 | HighContrastState | `0` | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 45 | SystemAppearance | `Aqua` | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 46 | StatusAreaState | `1` toolbar shown, `0` hidden | `1` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 47 | LayoutViewState | the window's view: `0` form, `1` list, `2` table | `0` / `1` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 48 | WindowName | the window's name: New Window's or Set Window Title's, else the file's; each window its own | `work` [=] | empty | Claris reference / spec; server s31, s35 (13.25) |
| 49 | WindowHeight | outer height in points (`900`; `848` after hiding the toolbar) | `480` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 50 | WindowLeft | points from the desktop area's left edge (`100`) | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 51 | WindowTop | points from the desktop area's top, below the menu bar (`120`) | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 52 | WindowVisible | `1` | `1` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 53 | WindowWidth | outer width in points (`1100`) | `640` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 54 | SystemNICAddress | MAC addresses, lowercase hex with colons, one per line | empty [= (harness has no network)] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 55 | SystemIPAddress | IPv4 addresses, one per line | empty [= (harness has no network)] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 56 | ActiveFieldTableName | the focused field's occurrence (`Contacts`, `Phone Numbers`) | empty / `Contacts` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O2); server s31 |
| 57 | ActiveSelectionSize | selection length (`2`; `0` for a caret); empty without a field | empty / `0` [=] | empty | FileMaker Pro 6.3 (O2); server s31 |
| 58 | ActiveSelectionStart | 1-based selection start (`2`; `9` after 8 characters); empty without a field | empty / `1` [=] | empty | FileMaker Pro 6.3 (O2); server s31 |
| 59 | ApplicationLanguage | `English` | `English` [=] | error 1223 | FileMaker Pro 6.3 (O1); server s31 |
| 60 | CurrentHostTimestamp | local file: the local clock (difference `00:00:00`); hosted [U] | server clock [=] | clock / new | FileMaker Pro 6.3 (O1); server s31 |
| 61 | LayoutTableName | layout occurrence | `Contacts` [=] | empty | Claris reference / spec; server s31 |
| 62 | ScriptParameter | runtime | `param` / empty [runtime] | empty | runtime; server s31 |
| 63 | CurrentTimestamp | local timestamp | server clock [=] | clock / new | Claris reference / spec; server s31 |
| 64 | WindowDesktopHeight | desktop area height (`1050`: screen minus the menu bar) | `480` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 65 | WindowDesktopWidth | desktop area width (`1854`: screen minus the Dock) | `640` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 66 | WindowContentHeight | `795` (window minus 105 points of chrome); `820` with the toolbar hidden | `480` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 67 | WindowContentWidth | `1100` | `640` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 68 | CalculationRepetitionNumber | evaluator | `0` [runtime] | `0` | runtime; server s31 |
| 69 | CurrentExtendedPrivileges | the keywords, one per line (`fmextscriptaccess¶fmreauthenticate10`) | `fmreauthenticate10` [the account's set's keywords, from the file (s33)] | `fmreauthenticate10` | FileMaker Pro 6.3 (O1); server s31 |
| 70 | AllowAbortState | runtime | `1` / `0` [runtime] | empty | runtime; server s31 |
| 71 | ErrorCaptureState | runtime | `0` / `1` [runtime] | empty | runtime; server s31 |
| 72 | DesktopPath | the desktop folder; empty in the capture copy (no Desktop in its home [I]) | empty [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 73 | DocumentsPath | the documents folder; empty in the capture copy (no Documents in its home [I]) | empty [the Documents folder (import/export)] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 74 | FileMakerPath | `/<volume>/<folder holding the application>/` | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 75 | HostIPAddress | empty for a local file; hosted [U] | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 76 | RequestOmitState | `1` on an omitting request, else `0`; `0` in Browse | `0` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 77 | PreferencesPath | empty in the capture copy (no Library/Preferences in its home [I]) | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 78 | RecordOpenCount | `1` with the record open (new or edited), else `0` | `0` / `1` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 79 | RecordOpenState | `0`; `1` new record; `2` edited (leaving the field commits) | `0` / `1` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 80 | ScriptResult | runtime | empty [runtime] | empty | runtime; server s31 |
| 81 | SystemDrive | `/Macintosh HD/` (the boot volume) | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 82 | TextRulerVisible | `0` | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 83 | AllowFormattingBarState | `1` | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 84 | UseSystemFormatsState | `1` in the Contacts probe [U: the setting behind it] | `0` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 85 | WindowZoomLevel | `100`, `150` | `100` [=] | `100` | FileMaker Pro 6.3 (O1, O5); server s31 |
| 86 | CustomMenuSetName | empty | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 87 | DBEngineVersion | 260030307 | `260030309` [=] | `260030309` | FileMaker Pro oracle (9.1); server s31 |
| 88 | ActiveLayoutObjectName | the active object's name (`First Name`); in a portal row the portal field object's, with the portal itself active the portal's; empty for an unnamed one or none | empty [=] | empty | FileMaker Pro 6.3 (O2); server s31, s35 (13.24) |
| 89 | TemporaryPath | `/<volume>/var/folders/…/T/S<n>/` | empty [a per-session folder (import/export)] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 90 | HostApplicationVersion | empty for a local file; hosted: the host's version string, `Server 26.0.1` in the Claris reference [U] | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 91 | TriggerModifierKeys | runtime (in a trigger) | empty [runtime] | empty | runtime; server s31 |
| 92 | TriggerKeystroke | runtime (in a trigger) | empty [runtime] | empty | runtime; server s31 |
| 93 | DocumentsPathListing | empty in the capture copy [I: no Documents] | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 94 | AccountPrivilegeSetName | `[Full Access]` | `[Read-Only Access]` [the account's set in the file (real login: `[Full Access]`; s33: `[Data Entry Only]`, `[Read-Only Access]`, custom)] | `[Read-Only Access]` | FileMaker Pro 6.3 (O1); server s31 |
| 95 | AccountExtendedPrivileges | the keywords, one per line | `fmreauthenticate10` [as row 69] | `fmreauthenticate10` | FileMaker Pro 6.3 (O1); server s31 |
| 96 | QuickFindText | empty, also after Perform Quick Find [ "Ann" ] | empty [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 97 | TriggerCurrentPanel | outside a trigger `0¶`; in a trigger runtime | `0¶` [=] | empty | FileMaker Pro 6.3 (O2); server s31 |
| 98 | TriggerTargetPanel | outside a trigger `0¶`; in a trigger runtime | `0¶` [=] | empty | FileMaker Pro 6.3 (O2); server s31 |
| 99 | WindowStyle | `0` | `0` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 100 | InstalledFMPlugins | `<name>;<version>;<state>` per plug-in, one per line | empty [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 101 | UUID | random v4, upper case | new each time [=] | clock / new | Claris reference / spec; server s31 |
| 102 | PersistentID | 32 upper-case hex digits, the same in every session on the Mac | `D41D8CD98F00B204E9800998ECF8427E` [the server id (harness: MD5 of nothing)] | `D41D8CD98F00B204E9800998ECF8427E` | FileMaker Pro 6.3 (O1); server s31 |
| 103 | ConnectionState | `0` for a local file; hosted [U] | `0` [=] | `0` | FileMaker Pro 6.3 (O1); server s31 |
| 104 | CurrentTimeUTCMilliseconds | UTC clock | new each time [=] | clock / new | Claris reference / spec; server s31 |
| 105 | Device | `1` Mac (2 Windows, 6 Linux) | `6` [=] | `6` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 106 | WindowOrientation | `-1` | `-1` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 107 | TriggerGestureInfo | runtime (in a trigger) | empty [runtime] | empty | runtime; server s31 |
| 108 | EncryptionState | `0` (unencrypted local file); hosted [U] | `0` [=] | `0` | FileMaker Pro 6.3 (O1); server s31 |
| 109 | ScriptAnimationState | runtime | `0` [runtime] | empty | runtime; server s31 |
| 110 | ModifiedFields | changed fields by name: Set Field lists the field and stored calculations on it; typing is listed only after the field is left; a new record lists its auto-entered fields; a script's later repetitions list their field | empty / `PrimaryKey¶CreatedBy¶ModifiedBy...` / `First Name¶Initial¶PrimaryKey¶C...` [=] | empty | FileMaker Pro 6.3 (O3); server s31 |
| 111 | NetworkType | empty (Go only) | empty [=] | empty | Claris reference / spec; server s31 |
| 112 | ConnectionAttributes | empty for a local file; hosted [U] | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 113 | ScreenScaleFactor | `1` | `1` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 115 | ApplicationArchitecture | `arm64` (Apple silicon) / `x86_64` | `x86_64` [=] | `x86_64` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 116 | TriggerExternalEvent | runtime (in a trigger) | empty [runtime] | empty | runtime; server s31 |
| 117 | TouchKeyboardState | `0` | `0` [=] | empty | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 118 | MenubarState | `1` | `1` [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 119 | RegionMonitorEvents | empty (Go only) | empty [=] | empty | Claris reference / spec; server s31 |
| 120 | AccountGroupName | empty | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 121 | ActiveRecordNumber | window record | `0` / `1` / `2` [=] | empty | Claris reference / spec; server s31 |
| 122 | UUIDNumber | a UUID's 128 bits [I] | new each time [a UUID's 128 bits [I]] | clock / new | Claris reference / spec; server s31 |
| 123 | OpenDataFileInfo | open data files | empty [=] | empty | Claris reference / spec; server s31 |
| 124 | AccountType | `FileMaker File` | `Guest` [`FileMaker File` (real login; the harness bypass says `Guest`)] | `Guest` | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 125 | PageCount | empty, also in Preview, a layout calculation and print output | empty [=] | empty | FileMaker Pro 6.3 (O4); server s31 |
| 126 | SystemLocaleElements | the system's formats, `Active` true | JSON [= (en-US server formats, `Active` true)] | JSON (`Active` false) | FileMaker Pro 6.3 (O1); server s31 |
| 127 | FileLocaleElements | the file's saved formats, `Active` false while system formats are in use | JSON [= (`Active` false)] | JSON (`Active` true) | Claris reference / spec, FileMaker Pro 6.3 (O1); server s31 |
| 128 | InstalledFMPluginsAsJSON | JSON: `APIVers` 2, `environment`, `plugins` | JSON [=] | JSON | FileMaker Pro 6.3 (O1); server s31 |
| 129 | SessionIdentifier | runtime | empty [runtime] | empty | runtime; server s31 |
| 130 | CacheFileName | `FMTEMPFM<process id>_<n>.tmp` | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 131 | CacheFilePath | `/<volume>/var/folders/…/T/` | empty [=] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 132 | TransactionOpenState | runtime | `0` / `1` [runtime] | empty | runtime; server s31 |
| 133 | CurrentTimeUTCMicroseconds | UTC clock | new each time [=] | clock / new | Claris reference / spec; server s31 |
| 134 | LastErrorLocation | runtime | empty [runtime] | empty | runtime; server s31 |
| 135 | LastStepTokensUsed | empty | empty [=] | empty | Claris reference / spec; server s31 |
| 136 | RevertTransactionOnErrorState | runtime | `1` [runtime] | empty | runtime; server s31 |
| 137 | SystemStorageAvailable | free bytes (positive) | `472567406592` [free bytes of the hosted file's volume] | `419607437312` | FileMaker Pro 6.3 (O1); server s31 |
| 138 | StudioDatasource | empty | `0` [=] | `0` | FileMaker Pro oracle (9.1); server s31 |
| 139 | AccountPasswordDaysRemaining | `-1` | `-1` [=] | `-1` | FileMaker Pro 6.3 (O1); server s31 |
| 140 | WindowUUID | 32 upper-case hex digits, new per window | `CB1FCC2C5F55438CA3EC703A52FA87BA` [per session, MD5 form] | empty | FileMaker Pro 6.3 (O1); server s31 |
| 141 | GuidedAccessState | empty (Go only) | empty [=] | empty | Claris reference / spec; server s31 |

### 6.1.1 Authenticated account facts in OpenMaker

For a local file, OpenMaker resolves the successful login reply's account
key against that file's private host. `Get ( AccountType )` answers
[`FileMaker File`](https://help.claris.com/en/pro-help/content/get-accounttype.html)
for the admitted file account. `Get (
AccountPasswordDaysRemaining )` uses that account's decoded status date and
its privilege set's password lifetime: -1 when there is no password or no
limit, otherwise the remaining calendar days, clamped to zero. A related
file's calculation uses that file's authenticated account facts, not the
window's root file. Script elevation does not change the account.

This uses the status and lifetime decoding described in local-files.md 2.2;
the remaining-days arithmetic is an inference from that login expiry rule
and the [Claris selector description](https://help.claris.com/en/pro-help/content/get-accountpassworddaysremaining.html).
Only -1 for unlimited passwords and `FileMaker File` were observed in native
Pro (6.3); positive values, rollover and the expired case need native
acceptance. Logins, local and to OpenMaker Server, refuse expired,
inactive and external accounts and accounts in sets OpenMaker does not
enforce (local-files.md 2.2, 2.3).

A hosted login's reply does not carry the authentication kind or its
password lifetime, and the desktop does not read them from the hosted
file. OpenMaker therefore answers these selectors empty for a hosted file;
calculation read provenance also marks them unknown, so an object hide
condition cannot mistake the empty fallback for a real zero: Browse draws a
disabled undetermined stand-in. This is an unavailable answer, not a claim
that FileMaker itself returns empty. OpenMaker Server's script engine
answers them as the local host does, from the file's account the session
logged in as, whose stored password authenticated it. No FileMaker Pro
UserName preference is read.

### 6.1.2 `Get ( RecordNumber )` in a portal row [Claris documentation, I]

FileMaker's documentation says the Record Number symbol `{{RecordNumber}}`
shows a row number when it is in a portal row, and numbers portal rows that
way ([Inserting variables on a
layout](https://help.claris.com/en/pro-help/content/inserting-variables-on-layout.html),
[Selecting and working with
portals](https://help.claris.com/en/pro-help/content/selecting-working-portals.html)).
The `Get ( RecordNumber )` page itself only describes the found-set
position. No FileMaker Pro capture exists yet.

OpenMaker shows the symbol by evaluating its `Get` selector (calc-values.md,
layout calculations), so the row number is part of the row's evaluation
context (`Context::portal_row_number`). The symbol answers the row's
position in the displayed set, from 1. The rest is inference [I]: with
initial row 3, the first visible row answers 3; `Get ( RecordNumber )` in
the row's other layout calculations (hide conditions, conditional
formatting, tooltips, labels) answers the same; a portal filter evaluates
each related record with its position in the sorted set before filtering;
and an unstored calculation field the row reads through a field reference
evaluates in its own record's context, so it answers that record's
found-set position. That last point is the least certain: solutions
commonly number portal rows with an unstored `Get ( RecordNumber )` field
in the related table, which suggests FileMaker Pro answers the row number
there too. Item O8 (6.2) settles these.

### 6.1.3 `Get ( UserName )` and the User Name setting [O: FileMaker Pro 26.0.3, macOS, 2026-11-18]

`Get ( UserName )` is FileMaker Pro's User Name setting, not the system's
name for the account:

- Settings (the application menu's Settings...) > General has a "User
  Name" group: a radio button "System: “<name>”", where the name is the
  macOS account's full name (`id -F`), and a radio button "Other" with a
  text field. This is the dialog's accessibility tree on the capture Mac.
- There, with Other chosen and a name typed, `Get ( UserName )` answered
  the typed name in a calc-oracle run. The account's full name, shown in
  the System label, was a different name. The typed name is
  `Preferences:UserName` in the `com.filemaker.client.pro12` preferences
  domain.
- FileMaker Server's `Access.log` names each FileMaker Pro client
  `"<user name> (<computer name>)"`. The lab server lists the capture
  Mac's FileMaker Pro under the typed name, not the account's.

So a script that compares `Get ( UserName )` with a list of names behaves
differently for the same person when the client gives the account's name
rather than the name chosen in Settings.

Unknowns: which choice a new installation starts with; where the System
or Other choice is stored (perhaps a bit of `Preferences:AppGenOptions`
[U]); what System gives on Windows; whether changing the setting while
connected changes the name the host lists. OpenMaker's choices are in
[desktop-client.md](../desktop-client.md#settings).

### 6.2 Open on FileMaker Pro

Answered for local files on 2026-10-29: see 6.3. Still open:

- the hosted-only selectors of O1, against `test project` on a host;
- `Get ( ActiveModifierKeys )` with Shift held (O2), which needs real
  keyboard input;
- `Get ( FileSize )` of a hosted file and how FileMaker Pro learns it
  (O7).
- what `Get ( RecordNumber )` answers in a portal row (O8).

FileMaker Pro runs only on another machine; these settle what the desktop
answers. Each item gives the file, the context and the expressions; record
each result exactly (`¶` as CR, with the error when not 0, as `→ value`,
`error`). Unless an item says otherwise: FileMaker Pro 26.0.3 on macOS,
logged in to `test project` on the lab FileMaker Server as its full access
account, window on Contact Details in Browse mode on the first record of all
records, the status toolbar shown, zoom 100%, the window moved so that its top left
corner is at screen point 100, 120 (note the menu bar height). Evaluate in
a script that ends with `Show Custom Dialog [ $r ]` or in the Data Viewer.

- **O1 Session, window and machine.** One expression per selector:
  `Get ( FileSize )`, `Get ( SystemLanguage )`, `Get ( ScreenDepth )`,
  `Get ( ScreenHeight )`, `Get ( ScreenWidth )`, `Get ( SystemVersion )`,
  `Get ( UserCount )`, `Get ( UserName )`, `Get ( CurrentPrivilegeSetName
  )`, `Get ( AccountPrivilegeSetName )`, `Get ( CurrentExtendedPrivileges
  )`, `Get ( AccountExtendedPrivileges )`, `Get ( AccountGroupName )`, `Get
  ( AccountPasswordDaysRemaining )`, `Get ( LayoutAccess )`, `Get (
  RecordAccess )`, `Get ( HighContrastState )`, `Get ( SystemAppearance
  )`, `Get ( StatusAreaState )`, `Get ( WindowHeight )`, `Get ( WindowLeft
  )`, `Get ( WindowTop )`, `Get ( WindowVisible )`, `Get ( WindowWidth )`,
  `Get ( WindowDesktopHeight )`, `Get ( WindowDesktopWidth )`, `Get (
  WindowContentHeight )`, `Get ( WindowContentWidth )`, `Get (
  WindowZoomLevel )`, `Get ( WindowStyle )`, `Get ( WindowOrientation )`,
  `Get ( WindowUUID )`, `Get ( MenubarState )`, `Get ( TextRulerVisible )`,
  `Get ( AllowFormattingBarState )`, `Get ( CustomMenuSetName )`, `Get (
  SystemNICAddress )`, `Get ( SystemIPAddress )`, `Get ( HostIPAddress )`,
  `Get ( HostApplicationVersion )`, `Get ( CurrentHostTimestamp ) - Get (
  CurrentTimestamp )`, `Get ( PersistentID )`, `Get ( ConnectionState )`,
  `Get ( ConnectionAttributes )`, `Get ( EncryptionState )`, `Get (
  ScreenScaleFactor )`, `Get ( SystemStorageAvailable ) > 0`, `Get (
  FileMakerPath )`, `Get ( PreferencesPath )`, `Get ( SystemDrive )`, `Get
  ( DocumentsPathListing )`, `Get ( InstalledFMPluginsAsJSON )`, `Get (
  CacheFileName )`, `Get ( CacheFilePath )`, `Get ( SystemLocaleElements
  )`, `Get ( ApplicationLanguage )`, `Length ( Get ( UUIDNumber ) )`, `Get
  ( UUIDNumber )` twice. Then hide the status toolbar and evaluate the
  window and content sizes again.
- **O2 Focus and selection.** Click into First Name, type `Ann`, select
  `nn` (characters 2-3), hold Shift while the script runs from a button:
  `Get ( ActiveFieldName )`, `Get ( ActiveFieldTableName )`, `Get (
  ActiveFieldContents )`, `Get ( ActiveRepetitionNumber )`, `Get (
  ActiveSelectionStart )`, `Get ( ActiveSelectionSize )`, `Get (
  ActiveModifierKeys )`, `Get ( ActiveLayoutObjectName )`, `Get (
  ActivePortalRowNumber )`; again with the cursor in the second row of the
  Phone portal; and with no field active. Outside any trigger: `Length (
  Get ( TriggerCurrentPanel ) )`, `Code ( Get ( TriggerCurrentPanel ) )`,
  same for `TriggerTargetPanel` (FileMaker Server: `0¶`).
- **O3 Found sets and records.** On Contact List: `Get ( LayoutViewState
  )` in List view and as Table; after Enter Find Mode with two requests
  (the second omitting) and Enter Browse Mode without finding: `Get (
  RequestCount )`, `Get ( RequestOmitState )` (in Find mode on each
  request too); after Sort Records: `Get ( SortState )`; after New Record
  and typing in First Name, before committing: `Get ( RecordOpenState )`,
  `Get ( RecordOpenCount )`, `Get ( ModifiedFields )` (FileMaker Server
  lists the auto-entered fields too); after Quick Find `Ann`: `Get (
  QuickFindText )`; on an empty found set: `Get ( RecordID )`, `Get (
  RecordModificationCount )`, `Get ( ActiveRecordNumber )`.
- **O4 Printing.** In Preview mode on Contact List (two pages or more):
  `Get ( PageNumber )` from a footer merge `{{PageNumber}}` and from a
  calculation on page 2, `Get ( PageCount )`, `Get ( PrinterName )`.
- **O5 Layout objects.** On Contact Details with the window at 100, 120:
  `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` with the toolbar
  shown and hidden and at zoom 150%; `GetLayoutObjectAttribute ( "First
  Name" ; "content" )`, `"isActive"`, `"hasFocus"` with First Name active
  and not; `GetLayoutObjectAttribute ( "website" ; "source" )` and
  `"content"` once the web viewer has loaded; `GetLayoutObjectAttribute (
  "Company" ; "bounds" ; 2 )`; a layout calculation in the web viewer's
  address `If ( LayoutObjectUUID = GetLayoutObjectAttribute ( "website" ;
  "UUID" ) ; "data:text/html,1" ; "data:text/html,0" )`; and
  `GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( "website" ;
  "UUID" ) )`. On Tasks' Task List (Tasks template, local, with records):
  `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 2 )`.
- **O6 Other open files.** With test project and the Contacts template
  (opened locally as `Contacts`) both open: `DatabaseNames`, `WindowNames`,
  `WindowNames ( "Contacts" )`, `TableNames ( "Contacts" )`, `LayoutNames (
  "Contacts.fmp12" )`, `FieldNames ( "Contacts" ; "Contact Details" )` from
  test project; and `TableNames ( "contacts" )` (case).
- **O7 FileSize of a hosted file.** No capture holds the answer: op 83
  GetFileSize is in no recorded session (s01-s06), and its request and
  reply are not specified (network-protocol.md 5.1). A disposable copy
  of the Contacts template as `filesize-probe.fmp12`, its Admin account
  given a password (FileMaker Server will not host a file whose
  full-access account has none), hosted by the lab FileMaker Server
  26.0.3 and opened in FileMaker Pro through the plaintext capture that
  recorded s03 (README, "Live observations"). A layout with an unstored
  calculation field `zSize` = `Get ( FileSize )`. The script:
  1. `Set Variable [ $a ; Get ( FileSize ) ]`, then `Set Variable [ $b ;
     Get ( FileSize ) ]`;
  2. 500 times `New Record/Request` and `Set Field [ Contacts::Company ;
     Get ( UUID ) ]`, then `Commit Records/Requests` (enough new pages for
     the size to change);
  3. `Set Variable [ $c ; Get ( FileSize ) ]`;
  4. `Show Custom Dialog [ $a & "¶" & $b & "¶" & $c & "¶" & Contacts::zSize ]`.

  Record:
  - the four answers;
  - the hosted file's size on the server's disk before step 1 and after
    step 3, with the time of each (`stat`, read only; FileMaker Server writes
    its cache to disk on its own schedule);
  - each Perform the client sends while a `Get ( FileSize )` is
    evaluated: its opcode (83 expected) and the request and reply bytes
    after the 20-byte header;
  - whether `$b` and `zSize` send again or reuse an answer;
  - when nothing is sent, the Open reply's fields of the session.

  Then the same file hosted by OpenMaker Server with `--record <dir>`.
  OpenMaker Server answers op 83 with status 3, "not supported". Record
  whether FileMaker Pro sends op 83 there and what it shows. Remove the
  scratch file, the copies and the records afterwards; the captures hold
  credentials.

- **O8 Portal rows (6.1.1).** On a layout with a portal of at least three
  related records, sorted, and a `{{RecordNumber}}` text in the first row
  (expected `1`, `2`, `3`): a conditional format in the row with `Get (
  RecordNumber ) = 2` (which row it formats); an unstored calculation `Get (
  RecordNumber )` in the related table, placed in the row; a portal filter
  `Get ( RecordNumber ) > 1` (which related records remain); and the same
  `{{RecordNumber}}` text with the portal's initial row set to 2.

### 6.3 FileMaker Pro answers (26.0.3, macOS, local files) [O: FileMaker Pro, 2026-10-29]

The items of 6.2, answered by FileMaker Pro 26.0.3 (arm64, English, macOS
26.6.2) on local files only. Nothing connected to the lab server or any
other host, so where 6.2 names `test project` the local Contacts probe stands
in. Hosted-only answers stay unmeasured:

- O1's host address and version (`HostIPAddress`, `HostApplicationVersion`);
- `UserCount`, `EncryptionState`, `ConnectionState` and
  `ConnectionAttributes` of a hosted file;
- `Get ( CurrentHostTimestamp )` against a host's clock.

`Get ( ActiveModifierKeys )` with Shift held is also unmeasured, because
no keyboard or mouse input was synthesized.

**Method.** `tools/calc-oracle/desktop_probe.py` builds `Contacts.fmp12` and
`Tasks.fmp12` from the starter templates with FileMaker Server's
FMUpgradeTool `--generateDBFile` (`make-desktop-probe.sh`). The probe files
add the following to each template:

- a global text field `zOut` for the answers;
- `fmextscriptaccess` for `[Full Access]`;
- in Contacts:
  - an unstored calculation `zPage` = `Get ( PageNumber ) & "/" & Get (
    PageCount )`;
  - a footer on Contact List with a text object `pageMerge` holding
    `{{PageNumber}}` and a field object `pageCalc` showing `zPage`;
  - the web viewer's address calculation of O5;
- one probe script per state.

`runner.py --scripts` opens the files in the capture copy and runs each
script with a PID-addressed `do script`; the event returns when the
script ends. It then reads `zOut` through Apple events. Every script:

- starts from the same state:
  - Browse mode, Contact Details, all records unsorted, record 1;
  - status toolbar shown, zoom 100 %;
  - `Move/Resize Window` to left 100, top 120, 1100 × 900;
  - Company focused and left again, which brings the first tab panel to
    the front;
- reaches its state with script steps only;
- evaluates each expression as `GetAsText ( Evaluate ( expression ) )`,
  with the error from `EvaluationError ( Evaluate ( expression ) )`.

The setup scripts create 40 contacts (record 1 `Alice Archer`, record 2 `Ann
Baker`, then `NameNN ProbeNN`), two phone numbers on record 1 (through the
portal) and three tasks.

Results are exact. `¶` is a carriage return, an error is given when it
is not 0, and `″` means the same context as the row above.
Machine-identifying values are replaced by their format. The raw answers
are in the git-ignored `re/work/oracle/desktop-probe/results-26.0.3.json`.
The last three full runs agreed on everything except:

- per-session values: `WindowUUID`, `CacheFileName` and `UUIDNumber`;
- `FileSize` and the object UUIDs, which belong to each build of the
  files;
- the order of `DatabaseNames` (below).

**Environment.**

- Screen: 1920 × 1080 points, depth 32, scale factor 1.
- Desktop area: 1854 × 1050. That leaves 30 points at the top, the
  menu bar, and 66 on the left, the Dock [I].
- The capture copy ran in the background. System Events reported
  `frontmost` false throughout, and no window was ever activated. This
  is probably why `isActive` is `0` even for the field with focus [I].
- `Get ( WindowLeft )` and `Get ( WindowTop )` are relative to the
  desktop area, not the screen. After `Move/Resize Window` to 100, 120,
  System Events placed the window at screen point 166, 150. After 10, 40
  it was at 76, 70.
- Object bounds use the same origin.

#### O1 Session, window and machine (Contact Details, toolbar shown)

| Expression | Result |
|---|---|
| `Get ( FileSize )` | `921600` |
| `Get ( SystemLanguage )` | `English` |
| `Get ( ScreenDepth )` | `32` |
| `Get ( ScreenHeight )` | `1080` |
| `Get ( ScreenWidth )` | `1920` |
| `Get ( SystemVersion )` | `26.6.2` |
| `Get ( UserCount )` | `1` |
| `Get ( UserName )` | the name typed in Settings' User Name, Other [redacted] (6.1.3) |
| `Get ( CurrentPrivilegeSetName )` | `[Full Access]` |
| `Get ( AccountPrivilegeSetName )` | `[Full Access]` |
| `Get ( CurrentExtendedPrivileges )` | `fmextscriptaccess¶fmreauthenticate10` |
| `Get ( AccountExtendedPrivileges )` | `fmextscriptaccess¶fmreauthenticate10` |
| `Get ( AccountGroupName )` | empty |
| `Get ( AccountPasswordDaysRemaining )` | `-1` |
| `Get ( LayoutAccess )` | `2` |
| `Get ( RecordAccess )` | `2` |
| `Get ( HighContrastState )` | `0` |
| `Get ( SystemAppearance )` | `Aqua` |
| `Get ( StatusAreaState )` | `1` |
| `Get ( WindowHeight )` | `900` |
| `Get ( WindowLeft )` | `100` |
| `Get ( WindowTop )` | `120` |
| `Get ( WindowVisible )` | `1` |
| `Get ( WindowWidth )` | `1100` |
| `Get ( WindowDesktopHeight )` | `1050` |
| `Get ( WindowDesktopWidth )` | `1854` |
| `Get ( WindowContentHeight )` | `795` |
| `Get ( WindowContentWidth )` | `1100` |
| `Get ( WindowZoomLevel )` | `100` |
| `Get ( WindowStyle )` | `0` |
| `Get ( WindowOrientation )` | `-1` |
| `Get ( WindowUUID )` | 32 upper-case hex digits, new for each window [redacted] |
| `Get ( MenubarState )` | `1` |
| `Get ( TextRulerVisible )` | `0` |
| `Get ( AllowFormattingBarState )` | `1` |
| `Get ( CustomMenuSetName )` | empty |
| `Get ( SystemNICAddress )` | 15 MAC addresses, lowercase hex with colons, one per line [redacted] |
| `Get ( SystemIPAddress )` | 2 IPv4 addresses, dotted decimal, one per line [redacted] |
| `Get ( HostIPAddress )` | empty |
| `Get ( HostApplicationVersion )` | empty |
| `Get ( CurrentHostTimestamp ) - Get ( CurrentTimestamp )` | `00:00:00` |
| `Get ( PersistentID )` | 32 upper-case hex digits, the same in every session on this Mac [redacted] |
| `Get ( ConnectionState )` | `0` |
| `Get ( ConnectionAttributes )` | empty |
| `Get ( EncryptionState )` | `0` |
| `Get ( ScreenScaleFactor )` | `1` |
| `Get ( SystemStorageAvailable ) > 0` | `1` |
| `Get ( FileMakerPath )` | `/Macintosh HD/<folder holding the application>/` [redacted] |
| `Get ( PreferencesPath )` | empty |
| `Get ( SystemDrive )` | `/Macintosh HD/` |
| `Get ( DocumentsPathListing )` | empty |
| `Get ( InstalledFMPluginsAsJSON )` | JSON, see below |
| `Get ( InstalledFMPlugins )` | `<name>;<version>;<state>` per plug-in, one per line (one plug-in, `Enabled`) [redacted] |
| `Get ( CacheFileName )` | `FMTEMPFM<process id>_2.tmp` |
| `Get ( CacheFilePath )` | `/Macintosh HD/var/folders/…/T/` |
| `Get ( SystemLocaleElements )` | JSON, see below |
| `Get ( FileLocaleElements )` | JSON, see below |
| `Get ( ApplicationLanguage )` | `English` |
| `Length ( Get ( UUIDNumber ) )` | `58` |
| `Get ( UUIDNumber )` | a 58-digit decimal number, new each time |
| `Get ( UUIDNumber )` | a 58-digit decimal number, new each time |
| `Get ( HostName )` | the Mac's host name, a DNS name [redacted] |
| `Get ( FilePath )` | `file:/Macintosh HD/<folder>/Contacts.fmp12` |
| `Get ( DocumentsPath )` | empty |
| `Get ( TemporaryPath )` | `/Macintosh HD/var/folders/…/T/S<n>/` |
| `Get ( DesktopPath )` | empty |
| `Get ( SystemPlatform )` | `1` |
| `Get ( ApplicationVersion )` | `Pro 26.0.3` |
| `Get ( ApplicationArchitecture )` | `arm64` |
| `Get ( Device )` | `1` |
| `Get ( WindowName )` | `Contacts` |
| `Get ( WindowMode )` | `0` |
| `Get ( LayoutName )` | `Contact Details` |
| `Get ( LayoutViewState )` | `0` |
| `Get ( AccountName )` | `Admin` |
| `Get ( AccountType )` | `FileMaker File` |
| `Get ( MultiUserState )` | `0` |
| `Get ( NetworkProtocol )` | `TCP/IP` |
| `Get ( NetworkType )` | empty |
| `Get ( DBEngineVersion )` | `260030307` |
| `Get ( StudioDatasource )` | `0` |
| `Get ( LayoutCount )` | `2` |
| `Get ( LayoutNumber )` | `2` |
| `Get ( LayoutTableName )` | `Contacts` |
| `Get ( FoundCount )` | `40` |
| `Get ( RecordNumber )` | `1` |
| `Get ( TotalRecordCount )` | `40` |
| `Get ( ActiveFieldName )` | empty |
| `Get ( SortState )` | `0` |
| `Get ( UseSystemFormatsState )` | `1` |
| `Get ( OpenDataFileInfo )` | empty |
| `Get ( SessionIdentifier )` | empty |
| `Get ( ErrorCaptureState )` | `1` |
| `Get ( AllowAbortState )` | `1` |
| `Get ( ScriptAnimationState )` | `0` |
| `Get ( TransactionOpenState )` | `0` |
| `Get ( RevertTransactionOnErrorState )` | `1` |
| `Get ( TouchKeyboardState )` | `0` |
| `Get ( GuidedAccessState )` | empty |
| `Get ( RegionMonitorEvents )` | empty |
| `Get ( LastStepTokensUsed )` | empty |

`Get ( FileSize )` is the file's current size in bytes, a multiple of
4096. With this build of the file it was 921600 in both runs, against
897024 as built. With two earlier builds it still equalled the size as
built after the same setup, in four runs. It is probably the size on
disk at that moment [I].
`CurrentHostTimestamp - CurrentTimestamp` is `00:00:00`. This file's
saved time format has `"0h" : true`; the calc-oracle file shows
`0:00:00`. `DesktopPath`, `DocumentsPath`, `DocumentsPathListing` and
`PreferencesPath` are empty in the capture copy. Its home folder
(`CFFIXED_USER_HOME`) has no `Desktop`, `Documents` or
`Library/Preferences`, which is probably why [I]. `UseSystemFormatsState` is `1` here
where FileMaker Server's session answered `0` for the same template [U:
the client's or the generated file's setting]. `TouchKeyboardState` is
`0`, not empty.

`Get ( SystemLocaleElements )` gives the system's formats; the same
answer as in the earlier calc-oracle run. FileMaker writes it with CR
line ends and one tab per level; it is shown here with LF and two
spaces. Keys are sorted:

```
{ "APIVers" : 1,
  "Currency" : { "Leading" : true, "Symbol" : "$" },
  "Date" : { "DMQ" : { "1stDayOfWeek" : 1,
      "DaysOfWeek" : { "AbbrvList" : [ "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat" ],
        "NameList" : [ "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday" ] },
      "Months" : { "AbbrvList" : [ "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec" ],
        "NameList" : [ "January", "February", "March", "April", "May", "June", "July", "August",
          "September", "October", "November", "December" ] },
      "Quarters" : { "AbbrvList" : [ "Q1", "Q2", "Q3", "Q4" ],
        "NameList" : [ "1st Quarter", "2nd Quarter", "3rd Quarter", "4th Quarter" ] } },
    "DateNums" : { "0d" : false, "0m" : false, "YYyy" : false },
    "DateOrderID" : 0, "DateOrderName" : "MDY",
    "MDY" : { "ElementArray" : { "NameList" : [ "M$", "D#", "YYYY#", "D$" ],
        "SepList" : [ ", ", " ", ", ", "" ] }, "MustUseLocalesSep" : false },
    "Sep" : "/" },
  "LocaleID" : { "IDNum" : 21, "IDStr" : "English", "ISOLangCode" : "en", "LID" : "English", "Name" : "English" },
  "Misc" : { "Active" : true, "Metric" : false },
  "Num" : { "1000s" : ",", "Decimal" : ".", "Lead0" : true },
  "Text" : { "SQuotLead" : "“", "SQuotTrail" : "”", "Sep" : "," },
  "Time" : { "12h" : true, "HMS" : { "0h" : false, "24h" : "", "Seconds" : false },
    "NightDay" : { "12hSuffix" : true, "amStr" : " AM", "pmStr" : " PM" }, "Sep" : ":" } }
```

`Get ( FileLocaleElements )` is the same document with the Contacts
template's saved formats. It differs in:

- `Date.DateNums`: `{ "0d" : true, "0m" : true, "YYyy" : true }`;
- `Date.MDY.ElementArray`: `NameList` `[ "M#", "D#", "D$", "D$" ]` and
  `SepList` `[ " ", " ", ", ", "" ]`;
- `Time.HMS."0h"`: `true`;
- `Misc.Active`: **false**.

The system document's `Misc.Active` is **true**. With
`UseSystemFormatsState` `1`, `Active` therefore marks the set of formats
in use [I]. In the calc-oracle file the file's document also says
`false`.

`Get ( InstalledFMPluginsAsJSON )`, in the same layout:
`{ "APIVers" : 2, "environment" : { "architecture" : "arm64",
"platform" : 1, "version" : "26.6.2" }, "plugins" : [ { "description",
"filepath" (a `filemac:` path), "id", "name", "state" : "Enabled",
"version" } ] }`. The plug-in's values are redacted.

With the status toolbar hidden (`Show/Hide Toolbars [ Hide ]`) and after
moving the window:

| Expression | Context | Result |
|---|---|---|
| `Get ( StatusAreaState )` | toolbar hidden | `0` |
| `Get ( WindowTop )` | ″ | `120` |
| `Get ( WindowLeft )` | ″ | `100` |
| `Get ( WindowHeight )` | ″ | `848` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `Get ( WindowContentHeight )` | ″ | `820` |
| `Get ( WindowContentWidth )` | ″ | `1100` |
| `Get ( WindowDesktopHeight )` | ″ | `1050` |
| `Get ( WindowDesktopWidth )` | ″ | `1854` |
| `Get ( WindowZoomLevel )` | ″ | `100` |
| `Get ( LastError )` | window moved to top 0, left 0 | `0` |
| `Get ( WindowTop )` | ″ | `0` |
| `Get ( WindowLeft )` | ″ | `0` |
| `Get ( WindowHeight )` | ″ | `900` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `0 105 200 149 0` |
| `Get ( LastError )` | window moved to top 40, left 10 | `0` |
| `Get ( WindowTop )` | ″ | `40` |
| `Get ( WindowLeft )` | ″ | `10` |
| `Get ( WindowHeight )` | ″ | `900` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `10 145 210 189 0` |

Hiding the toolbar shrinks the window by 52 points (900 to 848) and adds
25 to the content (795 to 820). The chrome above the content is therefore
105 points with the toolbar and 28 without it. That is probably a 28
point title bar, the 52 point status toolbar and a 25 point layout bar
[I]. The top-left object of the Top Navigation part ("Back to Contacts",
layout 0, 0) sits at `WindowLeft`, `WindowTop + 105`.

#### O2 Focus and selection (Contact Details)

The first state is reached with these steps:

1. `Go to Field [ Select: On ; Contacts::First Name ]`;
2. `Insert Text [ "Ann" ]`, which replaces the selection, as typing
   does;
3. `Set Selection [ Contacts::First Name ; 2 ; 3 ]`.

The portal row comes from `Go to Field [ Phone Numbers::Number ]` and
`Go to Portal Row [ Select: Off ; 2 ]`.

| Expression | Context | Result |
|---|---|---|
| `Get ( LastError )` | First Name: Ann typed, nn selected | `0` |
| `Get ( ActiveFieldName )` | ″ | `First Name` |
| `Get ( ActiveFieldTableName )` | ″ | `Contacts` |
| `Get ( ActiveFieldContents )` | ″ | `Ann` |
| `Get ( ActiveRepetitionNumber )` | ″ | `1` |
| `Get ( ActiveSelectionStart )` | ″ | `2` |
| `Get ( ActiveSelectionSize )` | ″ | `2` |
| `Get ( ActiveModifierKeys )` | ″ | `0` |
| `Get ( ActiveLayoutObjectName )` | ″ | `First Name` |
| `Get ( ActivePortalRowNumber )` | ″ | `0` |
| `Get ( RecordOpenState )` | ″ | `2` |
| `Get ( RecordOpenCount )` | ″ | `1` |
| `Get ( ModifiedFields )` | ″ | empty |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Ann` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `1` |
| `GetLayoutObjectAttribute ( "First Name" ; "containsFocus" )` | ″ | `1` |
| `GetLayoutObjectAttribute ( "First Name" ; "source" )` | ″ | `Contacts::First Name` |
| `Get ( LastError )` | First Name: Ann typed, field left | `0` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( RecordOpenCount )` | ″ | `0` |
| `Get ( ModifiedFields )` | ″ | empty |
| `Get ( ActiveFieldName )` | ″ | empty |
| `Get ( LastError )` | Phone portal row 2 | `0` |
| `Get ( ActiveFieldName )` | ″ | `Number` |
| `Get ( ActiveFieldTableName )` | ″ | `Phone Numbers` |
| `Get ( ActiveFieldContents )` | ″ | `555-0102` |
| `Get ( ActiveRepetitionNumber )` | ″ | `1` |
| `Get ( ActiveSelectionStart )` | ″ | `9` |
| `Get ( ActiveSelectionSize )` | ″ | `0` |
| `Get ( ActiveModifierKeys )` | ″ | `0` |
| `Get ( ActiveLayoutObjectName )` | ″ | empty |
| `Get ( ActivePortalRowNumber )` | ″ | `2` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( RecordOpenCount )` | ″ | `0` |
| `Get ( ModifiedFields )` | ″ | empty |
| `Count ( Phone Numbers::Number )` | ″ | `2` |
| `Get ( ActiveFieldName )` | no active field | empty |
| `Get ( ActiveFieldTableName )` | ″ | empty |
| `Get ( ActiveFieldContents )` | ″ | empty |
| `Get ( ActiveRepetitionNumber )` | ″ | `0` |
| `Get ( ActiveSelectionStart )` | ″ | empty |
| `Get ( ActiveSelectionSize )` | ″ | empty |
| `Get ( ActiveModifierKeys )` | ″ | `0` |
| `Get ( ActiveLayoutObjectName )` | ″ | empty |
| `Get ( ActivePortalRowNumber )` | ″ | `0` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( RecordOpenCount )` | ″ | `0` |
| `Get ( ModifiedFields )` | ″ | empty |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "containsFocus" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "source" )` | ″ | `Contacts::First Name` |
| `Length ( Get ( TriggerCurrentPanel ) )` | ″ | `2` |
| `Code ( Get ( TriggerCurrentPanel ) )` | ″ | `1300048` |
| `Length ( Get ( TriggerTargetPanel ) )` | ″ | `2` |
| `Code ( Get ( TriggerTargetPanel ) )` | ″ | `1300048` |
| `Get ( TriggerCurrentPanel )` | ″ | `0¶` |
| `Get ( TriggerTargetPanel )` | ″ | `0¶` |
| `Get ( TriggerKeystroke )` | ″ | empty |
| `Get ( TriggerModifierKeys )` | ″ | empty |
| `Get ( TriggerExternalEvent )` | ″ | empty |
| `Get ( TriggerGestureInfo )` | ″ | empty |

Leaving the field with `Go to Field [ ]` **commits** the record:
`RecordOpenState` goes to `0` and the typed text is saved, so the probe
puts the old value back. While the cursor is still in the field, the
typed text counts for `ActiveFieldContents` and the object's `content`,
and the record is open (`RecordOpenState` `2`). `ModifiedFields`,
however, does not list the field. The trigger panels outside a trigger
are `0¶`, as on FileMaker Server (`Code` gives `1300048`: CR then `0`).

#### O3 Found sets and records

The view states come from `View As` on Contact List. The prologue's
`Unsort Records` follows Go to Layout, so Contact List's OnLayoutEnter
sort is undone.

| Expression | Context | Result |
|---|---|---|
| `Get ( LastError )` | Contact List as List | `0` |
| `Get ( LayoutViewState )` | ″ | `1` |
| `Get ( LayoutName )` | ″ | `Contact List` |
| `Get ( SortState )` | ″ | `0` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( PageNumber )` | ″ | `0` |
| `Get ( PageCount )` | ″ | empty |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | `?` |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | `0/` |
| `Contacts::zPage` | ″ | `0/` |
| `Get ( LastError )` | Contact List as Table | `0` |
| `Get ( LayoutViewState )` | ″ | `2` |
| `Get ( LayoutName )` | ″ | `Contact List` |
| `Get ( SortState )` | ″ | `0` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( PageNumber )` | ″ | `0` |
| `Get ( PageCount )` | ″ | empty |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | empty |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | empty |
| `Contacts::zPage` | ″ | `0/` |
| `Get ( LastError )` | Contact List as Form | `0` |
| `Get ( LayoutViewState )` | ″ | `0` |
| `Get ( LayoutName )` | ″ | `Contact List` |
| `Get ( SortState )` | ″ | `0` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( PageNumber )` | ″ | `0` |
| `Get ( PageCount )` | ″ | empty |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | `?` |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | `0/` |
| `Contacts::zPage` | ″ | `0/` |
| `Get ( LastError )` | Find mode, request 2 of 2 (omit) | `0` |
| `Get ( RequestCount )` | ″ | `2` |
| `Get ( RequestOmitState )` | ″ | `1` |
| `Get ( WindowMode )` | ″ | `1` |
| `Get ( RecordNumber )` | ″ | `2` |
| `Get ( ActiveRecordNumber )` | ″ | `2` |
| `Get ( FoundCount )` | ″ | `2` |
| `Get ( RequestCount )` | Find mode, request 1 of 2 | `2` |
| `Get ( RequestOmitState )` | ″ | `0` |
| `Get ( WindowMode )` | ″ | `1` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( ActiveRecordNumber )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `2` |
| `Get ( RequestCount )` | Browse mode, after Find mode without a find | `2` |
| `Get ( RequestOmitState )` | ″ | `0` |
| `Get ( WindowMode )` | ″ | `0` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( ActiveRecordNumber )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( LastError )` | Set Field into First Name, not committed | `0` |
| `Get ( RecordOpenState )` | ″ | `2` |
| `Get ( RecordOpenCount )` | ″ | `1` |
| `Get ( ModifiedFields )` | ″ | `First Name¶Initial` |
| `Get ( ActiveFieldName )` | ″ | empty |
| `Get ( LastError )` | unsorted | `0` |
| `Get ( SortState )` | ″ | `0` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( LastError )` | after Sort Records | `0` |
| `Get ( SortState )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( LastError )` | sorted, new record open | `0` |
| `Get ( SortState )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `41` |
| `Get ( RecordNumber )` | ″ | `2` |
| `Get ( LastError )` | sorted, new record reverted | `0` |
| `Get ( SortState )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( RecordNumber )` | ″ | `2` |
| `Get ( LastError )` | new record, Zoe typed, not committed | `0` |
| `Get ( ActiveFieldName )` | ″ | `First Name` |
| `Get ( ActiveFieldTableName )` | ″ | `Contacts` |
| `Get ( ActiveFieldContents )` | ″ | `Zoe` |
| `Get ( ActiveRepetitionNumber )` | ″ | `1` |
| `Get ( ActiveSelectionStart )` | ″ | `4` |
| `Get ( ActiveSelectionSize )` | ″ | `0` |
| `Get ( ActiveModifierKeys )` | ″ | `0` |
| `Get ( ActiveLayoutObjectName )` | ″ | `First Name` |
| `Get ( ActivePortalRowNumber )` | ″ | `0` |
| `Get ( RecordOpenState )` | ″ | `1` |
| `Get ( RecordOpenCount )` | ″ | `1` |
| `Get ( ModifiedFields )` | ″ | `PrimaryKey¶CreatedBy¶ModifiedBy¶CreationTimestamp¶ModificationTimestamp` |
| `Get ( RecordID )` | ″ | `42` |
| `Get ( RecordModificationCount )` | ″ | `0` |
| `Get ( FoundCount )` | ″ | `41` |
| `Get ( RecordNumber )` | ″ | `41` |
| `Get ( LastError )` | after Revert Record | `0` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( RecordOpenCount )` | ″ | `0` |
| `Get ( ModifiedFields )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( LastError )` | new record, Zoe typed, field left | `0` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( RecordOpenCount )` | ″ | `0` |
| `Get ( ModifiedFields )` | ″ | empty |
| `Get ( ActiveFieldName )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `41` |
| `Get ( LastError )` | after Delete Record | `0` |
| `Get ( FoundCount )` | ″ | `40` |
| `Get ( TotalRecordCount )` | ″ | `40` |
| `Get ( LastError )` | after Perform Quick Find Ann | `0` |
| `Get ( QuickFindText )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `1` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( WindowMode )` | ″ | `0` |
| `Get ( LastError )` | empty found set (Perform Find, no match) | `401` |
| `Get ( FoundCount )` | ″ | `0` |
| `Get ( RecordID )` | ″ | `0` |
| `Get ( RecordModificationCount )` | ″ | `0` |
| `Get ( ActiveRecordNumber )` | ″ | `0` |
| `Get ( RecordNumber )` | ″ | `0` |
| `Get ( WindowMode )` | ″ | `0` |
| `Get ( RecordOpenState )` | ″ | `0` |
| `Get ( TotalRecordCount )` | ″ | `40` |
| `Get ( SortState )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | empty |
| `Get ( QuickFindText )` | ″ | empty |

Some notes on these answers:

- In Find mode, `FoundCount` is the number of requests, and
  `RecordNumber` and `ActiveRecordNumber` give the current request.
- `RequestCount` keeps the requests' count after `Enter Browse Mode`
  without a find, as FileMaker Server's engine does.
- `SortState` stayed `1` after `New Record/Request` in a sorted set. No
  `2` was seen. The new record went in after the current one
  (`RecordNumber` `2`).
- `Get ( ModifiedFields )` lists field names without an occurrence:
  - Set Field lists the field and a stored calculation that depends on
    it (`Initial`).
  - A new record lists its auto-entered creation fields, but not the
    text still being typed.
- `Perform Quick Find [ "Ann" ]` found `Ann Baker` but left
  `QuickFindText` empty. That selector reads the toolbar's search box,
  which the step does not fill [I].
- In Table view the footer objects do not exist (empty).

#### O4 Printing (Contact List, Preview mode, 40 records: 4 pages)

| Expression | Context | Result |
|---|---|---|
| `Get ( LastError )` | Preview, page 1 | `0` |
| `Get ( WindowMode )` | ″ | `2` |
| `Get ( PageNumber )` | ″ | `1` |
| `Get ( PageCount )` | ″ | empty |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | `?` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "bounds" )` | ″ | empty |
| `Contacts::zPage` | ″ | `1/` |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | `1/` |
| `Get ( PrinterName )` | ″ | ` ` (one space) |
| `Get ( LastError )` | Preview, page 2 | `0` |
| `Get ( WindowMode )` | ″ | `2` |
| `Get ( PageNumber )` | ″ | `2` |
| `Get ( PageCount )` | ″ | empty |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | `?` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "bounds" )` | ″ | empty |
| `Contacts::zPage` | ″ | `2/` |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | `2/` |
| `Get ( LastError )` | Preview, last page | `0` |
| `Get ( WindowMode )` | ″ | `2` |
| `Get ( PageNumber )` | ″ | `4` |
| `Get ( PageCount )` | ″ | empty |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( FoundCount )` | ″ | `40` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "content" )` | ″ | `?` |
| `GetLayoutObjectAttribute ( "pageMerge" ; "bounds" )` | ″ | empty |
| `Contacts::zPage` | ″ | `4/` |
| `GetLayoutObjectAttribute ( "pageCalc" ; "content" )` | ″ | `4/` |
| `Get ( LastError )` | pdf saved | `0` |
| `ConvertFromFileMakerPath ( $pdf ; PosixPath )` | pdf | `/var/folders/…/T/S<n>/o4-contact-list.pdf` |

`Get ( PageCount )` is empty everywhere: in Browse, in Preview, in a
layout calculation (`zPage` shows `1/`) and in printed output. In
`Save Records as PDF` of the same found set, page *n*'s footer reads `n`
for the `{{PageNumber}}` merge and `n/` for `zPage`, on all 4 pages.
`Get ( PageNumber )` is `0` outside Preview and the current page inside
it, both in a script and in a layout calculation. A text object's
`content` is `?` with error 0, and its `bounds` are empty in Preview.
This Mac has no printer configured (`lpstat`: no destinations), and
`Get ( PrinterName )` is a single space. Printing cuts the 1024-point
layout at the page edge, so `pageCalc` sits in the first column.

#### O5 Layout objects (Contact Details; Tasks' Task List)

Each geometry state paused 2 seconds after changing zoom or toolbar.

| Expression | Context | Result |
|---|---|---|
| `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` | zoom 100%, toolbar shown | `132 841 499 885 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" )` | ″ | `542 431 842 475 0` |
| `GetLayoutObjectAttribute ( "website" ; "bounds" )` | ″ | `542 486 1167 989 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `100 225 300 269 0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `0` |
| `Get ( StatusAreaState )` | ″ | `1` |
| `Get ( WindowTop )` | ″ | `120` |
| `Get ( WindowLeft )` | ″ | `100` |
| `Get ( WindowHeight )` | ″ | `900` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `Get ( WindowContentHeight )` | ″ | `795` |
| `Get ( WindowContentWidth )` | ″ | `1100` |
| `Get ( WindowDesktopHeight )` | ″ | `1050` |
| `Get ( WindowDesktopWidth )` | ″ | `1854` |
| `Get ( WindowZoomLevel )` | ″ | `100` |
| `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` | zoom 100%, toolbar hidden | `132 789 499 833 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" )` | ″ | `542 354 842 398 0` |
| `GetLayoutObjectAttribute ( "website" ; "bounds" )` | ″ | `542 409 1167 937 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `100 148 300 192 0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `0` |
| `Get ( StatusAreaState )` | ″ | `0` |
| `Get ( WindowTop )` | ″ | `120` |
| `Get ( WindowLeft )` | ″ | `100` |
| `Get ( WindowHeight )` | ″ | `848` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `Get ( WindowContentHeight )` | ″ | `820` |
| `Get ( WindowContentWidth )` | ″ | `1100` |
| `Get ( WindowDesktopHeight )` | ″ | `1050` |
| `Get ( WindowDesktopWidth )` | ″ | `1854` |
| `Get ( WindowZoomLevel )` | ″ | `100` |
| `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` | zoom 150%, toolbar shown | `148 793 585 859 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" )` | ″ | `649 512 1099 578 0` |
| `GetLayoutObjectAttribute ( "website" ; "bounds" )` | ″ | `649 595 1587 1015 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `100 225 300 269 0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `0` |
| `Get ( StatusAreaState )` | ″ | `1` |
| `Get ( WindowTop )` | ″ | `120` |
| `Get ( WindowLeft )` | ″ | `100` |
| `Get ( WindowHeight )` | ″ | `900` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `Get ( WindowContentHeight )` | ″ | `795` |
| `Get ( WindowContentWidth )` | ″ | `1100` |
| `Get ( WindowDesktopHeight )` | ″ | `1050` |
| `Get ( WindowDesktopWidth )` | ″ | `1854` |
| `Get ( WindowZoomLevel )` | ″ | `150` |
| `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` | zoom 150%, toolbar hidden | `148 716 585 782 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" )` | ″ | `649 435 1099 501 0` |
| `GetLayoutObjectAttribute ( "website" ; "bounds" )` | ″ | `649 518 1587 938 0` |
| `GetLayoutObjectAttribute ( "Company" ; "bounds" ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Back to Contacts" ; "bounds" )` | ″ | `100 148 300 192 0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `0` |
| `Get ( StatusAreaState )` | ″ | `0` |
| `Get ( WindowTop )` | ″ | `120` |
| `Get ( WindowLeft )` | ″ | `100` |
| `Get ( WindowHeight )` | ″ | `848` |
| `Get ( WindowWidth )` | ″ | `1100` |
| `Get ( WindowContentHeight )` | ″ | `820` |
| `Get ( WindowContentWidth )` | ″ | `1100` |
| `Get ( WindowDesktopHeight )` | ″ | `1050` |
| `Get ( WindowDesktopWidth )` | ″ | `1854` |
| `Get ( WindowZoomLevel )` | ″ | `150` |
| `GetLayoutObjectAttribute ( "website" ; "source" )` | web viewer from its layout calculation | `file:///` |
| `GetLayoutObjectAttribute ( "website" ; "content" )` | ″ | `1` |
| `GetLayoutObjectAttribute ( "website" ; "objectType" )` | ″ | `web viewer` |
| `GetLayoutObjectAttribute ( "website" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "website" ; "isObjectHidden" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "website" ; "UUID" )` | ″ | 8-4-4-4-12 upper-case hex (the object's id, made when the file was generated) |
| `GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( "website" ; "UUID" ) )` | ″ | JSON, see below |
| `GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( "First Name" ; "UUID" ) )` | ″ | JSON, see below |
| `GetLayoutObjectOwnerInfo ( Lower ( GetLayoutObjectAttribute ( "website" ; "UUID" ) ) )` | ″ | JSON, see below |
| `GetLayoutObjectOwnerInfo ( Substitute ( GetLayoutObjectAttribute ( "website" ; "UUID" ) ; "-" ; "" ) )` | ″ | JSON, see below |
| `LayoutObjectUUID` | ″ | `?` (error 10) |
| `GetLayoutObjectAttribute ( "First Name" ; "UUID" )` | ″ | 8-4-4-4-12 upper-case hex (as above) |
| `Get ( LastError )` | after Set Web Viewer Go to URL | `0` |
| `GetLayoutObjectAttribute ( "website" ; "source" )` | ″ | `file:///` |
| `GetLayoutObjectAttribute ( "website" ; "content" )` | ″ | `<p>probe</p>` |
| `Get ( LastError )` | First Name active | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "content" )` | ″ | `Alice` |
| `GetLayoutObjectAttribute ( "First Name" ; "isActive" )` | ″ | `0` |
| `GetLayoutObjectAttribute ( "First Name" ; "hasFocus" )` | ″ | `1` |
| `GetLayoutObjectAttribute ( "First Name" ; "containsFocus" )` | ″ | `1` |
| `GetLayoutObjectAttribute ( "First Name" ; "source" )` | ″ | `Contacts::First Name` |
| `GetLayoutObjectAttribute ( "First Name" ; "bounds" )` | ″ | `132 841 499 885 0` |
| `Get ( ActiveLayoutObjectName )` | ″ | `First Name` |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" )` | Task List, record 1, no pause | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 1 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 3 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 4 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 1 ; 2 )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `3` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( LayoutViewState )` | ″ | `1` |
| `Get ( LayoutName )` | ″ | `Task List` |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" )` | Task List, record 1 | `1156 299 1200 359 0` |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 1 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 3 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 4 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 1 ; 2 )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `3` |
| `Get ( RecordNumber )` | ″ | `1` |
| `Get ( LayoutViewState )` | ″ | `1` |
| `Get ( LayoutName )` | ″ | `Task List` |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" )` | Task List, record 2 | `1156 359 1200 419 0` |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 1 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 2 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 3 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 0 ; 4 )` | ″ | empty |
| `GetLayoutObjectAttribute ( "Go to details." ; "bounds" ; 1 ; 2 )` | ″ | empty |
| `Get ( FoundCount )` | ″ | `3` |
| `Get ( RecordNumber )` | ″ | `2` |
| `Get ( LayoutViewState )` | ″ | `1` |
| `Get ( LayoutName )` | ″ | `Task List` |

How the screen bounds are made up, from these answers:

- **Origin.** The origin is the desktop area, as for `WindowLeft` and
  `WindowTop`. The content starts at `WindowLeft`, `WindowTop + 105`
  with the toolbar, or `+ 28` without it.
- **Zoom.** The Top Navigation part is not zoomed: "Back to Contacts"
  keeps `100 225 300 269` at 150 %. The parts below it are scaled from
  their top: First Name at 150 % is 225 + 44 + (393 − 44) × 1.5 ≈ 793,
  and its left is 100 + 32 × 1.5 = 148.
- **Anchoring at 100 %.** The window (1100 × 795 of content) is larger
  than the layout (1024 wide), so anchored objects move or stretch:
  - First Name is 76 points wider: 367 against the layout's 291.
  - It sits 223 points lower, so it is anchored to the bottom [I].
  - Company and the web viewer move right by 76.
  - At 150 % the layout is larger than the window and nothing stretches.
- **Repetition 2.** `GetLayoutObjectAttribute ( "Company" ; "bounds" ;
  2 )` of a field with one repetition is empty.
- **List layout.** Task List's "Go to details." (layout 980, 104) has no
  bounds until the window has drawn: the reading right after Go to
  Layout was empty, and after a pause it is the current record's row.
  The portal-row argument (`0 ; 1` … `0 ; 4`, `1 ; 2`) is always empty
  on a list layout.
- **Web viewer.**
  - Its layout calculation `If ( LayoutObjectUUID =
    GetLayoutObjectAttribute ( "website" ; "UUID" ) ; "data:text/html,1"
    ; "data:text/html,0" )` loaded the page `1`, so `LayoutObjectUUID`
    inside an object's own calculation is that object's `UUID`
    attribute.
  - The `source` attribute answers `file:///` for a `data:` URL, both
    from the calculation and after `Set Web Viewer [ Go to URL ]`.
  - `content` is the page's HTML (`1`, `<p>probe</p>`).
- **`GetLayoutObjectOwnerInfo`** of the web viewer's or First Name's
  UUID, in any case or without hyphens, gives the not-found document,
  as on the server: `{ "APIVers" : 1, "Object" : { "Index" :
  4294967295, "Name" : "", "Owners" : { "Layout" : { "Name" : "",
  "UUID" : "" } } } }`, with CR and tabs as above. FileMaker Pro 26.0.3
  did not find either object.
- **`isActive`** is `0` even with focus. The application was in the
  background (above).

#### O6 Other open files (Contacts and Tasks, opened in that order)

| Expression | Context | Result |
|---|---|---|
| `Get ( LastError )` | setup | `0` |
| `Get ( TotalRecordCount )` | ″ | `40` |
| `Get ( FoundCount )` | ″ | `40` |
| `Count ( Phone Numbers::Number )` | ″ | `2` |
| `List ( Phone Numbers::Number )` | ″ | `555-0101¶555-0102` |
| `Get ( LastError )` | setup | `0` |
| `Get ( TotalRecordCount )` | ″ | `3` |
| `DatabaseNames` | from Tasks, Contacts open | `Contacts¶Tasks` |
| `WindowNames` | ″ | `Tasks¶Contacts` |
| `WindowNames ( "Contacts" )` | ″ | `Contacts` |
| `WindowNames ( "Tasks" )` | ″ | `Tasks` |
| `TableNames ( "Contacts" )` | ″ | `Addresses¶Contacts¶Email Addresses¶Phone Numbers` |
| `TableNames ( "contacts" )` | ″ | `Addresses¶Contacts¶Email Addresses¶Phone Numbers` |
| `TableNames ( "CONTACTS.FMP12" )` | ″ | `Addresses¶Contacts¶Email Addresses¶Phone Numbers` |
| `TableNames ( " Contacts" )` | ″ | empty |
| `LayoutNames ( "Contacts.fmp12" )` | ″ | `Contact List¶Contact Details` |
| `LayoutNames ( "Contacts" )` | ″ | `Contact List¶Contact Details` |
| `FieldNames ( "Contacts" ; "Contact Details" )` | ″ | `Website¶Company¶Phone Numbers::Number¶Phone Numbers::Type¶Email Addresses::Address¶Email Addresses::Type¶Addresses::Country¶Addresses::Postal Code¶Addresses::State¶Addresses::City¶Addresses::Address Line 2¶Addresses::Address Line 1¶Addresses::Type¶Job Title¶Last Name¶First Name¶Title¶Photo¶Photo` |
| `FieldNames ( "Contacts" ; "Contacts" )` | ″ | `First Name¶Last Name¶Photo¶Title¶Company¶Job Title¶Website¶Initial¶PrimaryKey¶CreatedBy¶ModifiedBy¶CreationTimestamp¶ModificationTimestamp¶zOut¶zPage` |
| `LayoutIDs ( "Contacts" )` | ″ | `2¶3` |
| `TableIDs ( "Contacts" )` | ″ | `1065091¶1065090¶1065093¶1065092` |
| `Get ( FileName )` | ″ | `Tasks` |
| `Get ( WindowName )` | ″ | `Tasks` |
| `DatabaseNames` | from Contacts, Tasks open | `Contacts¶Tasks` |
| `WindowNames` | ″ | `Contacts¶Tasks` |
| `WindowNames ( "Tasks" )` | ″ | `Tasks` |
| `TableNames ( "Tasks" )` | ″ | `Assignees¶Assignments¶Attachments¶Tasks` |
| `LayoutNames ( "Tasks" )` | ″ | `Task List¶Task Details¶Assignee List¶Assignee Details¶-¶Assignee Chooser¶Add Assignee¶Assignments` |
| `FieldNames ( "Tasks" ; "Task List" )` | ″ | `Due Date¶Category¶Status¶Task` |
| `Get ( FileName )` | ″ | `Contacts` |
| `Get ( WindowName )` | ″ | `Contacts` |

`DatabaseNames` lists the open files without extension. Their order is
not stable: four of six runs gave `Contacts¶Tasks` and two gave
`Tasks¶Contacts`, with the same open order. `WindowNames` lists the
front window first; a script brings its file's window to the front. A
file name argument matches in any case and with `.fmp12` in any case, and
a leading space answers empty, as on FileMaker Server.
`FieldNames` of another file's layout follows the server rule:

- fields of the layout's occurrence by name, others as
  `Occurrence::Field`;
- one entry per object, so `Photo` appears twice.

`FieldNames ( "Contacts" ; "Contacts" )` includes the probe's own `zOut`
and `zPage`.

## 7. Implementation tiers

From the JSON (`tier`, `core`). Tier 1 core is what common solutions need
first.

**Tier 1, core (86)**: Char, Code, Exact, Filter, FilterValues, GetAsDate,
GetAsNumber, GetAsText, GetAsTime, GetAsTimestamp, GetAsURLEncoded,
GetValue, Left, LeftValues, LeftWords, Length, Lower, Middle, MiddleValues,
MiddleWords, PatternCount, Position, Proper, Quote, Replace, Right,
RightValues, RightWords, SerialIncrement, SortValues, Substitute, Trim,
TrimAll, UniqueValues, Upper, ValueCount, WordCount, Abs, Ceiling, Div,
Floor, Int, Mod, Random, Round, SetPrecision, Sign, Sqrt, Truncate, Date,
Day, DayName, DayOfWeek, DayOfYear, Month, MonthName, WeekOfYear,
WeekOfYearFiscal, Year, Hour, Minute, Seconds, Time, Timestamp,
Base64Encode, CryptDigest, HexEncode, the ten documented JSON functions, Pi,
Case, Choose, GetAsBoolean, If, IsEmpty, Let, SetRecursion, While; plus the
keyword constants.

**Tier 1, other (64)**: text formatting (10), GetAsCSS, GetAsSVG,
Combination, Exp, Factorial, Lg, Ln, Log, the remaining container byte
functions (12), Japanese conversions (11), FV, PMT, PV, trigonometry (8),
AI vector math and GetTokenCount (7), the five undocumented JSON functions.

**Tier 2 (56 functions + 81 selectors)**: core: Average, Count, List, Max,
Min, Sum, Extend, GetRepetition, Last, Evaluate, EvaluationError,
ExecuteSQL, GetField, GetNthRecord, GetSummary, IsValid, IsValidExpression,
Self; then StDev, StDevP, Variance, VarianceP, NPV, ExecuteSQLe, Lookup,
LookupNext, GetBaseTableName, GetFieldName, GetPersistentData,
GetRecordIDsFromFoundSet, ListPersistentDataIDs, all 26 Design functions;
session, clock, context and script Get selectors. Evaluate and
EvaluationError are pure unless the evaluated text refers to fields, so
they can be built with tier 1.

**Tier 3 (30 functions + 59 selectors)**: container analysis (9), Furigana,
AI provider/model functions (7), path conversion (2), GetAddonInfo,
GetLayoutObjectAttribute, GetLayoutObjectOwnerInfo, LayoutObjectUUID,
WindowNames, mobile (5), IndexItems; UI, device, filesystem, plugin,
printing and AI Get selectors.

## 8. Test vectors

`calc-function-vectors.tsv`: 580 rows, tab-separated with **no quoting**
(expected values may start with `"`; read with quoting disabled). Columns `function`, `expression`,
`expected`, `source` (`doc`, `engine`, `inferred`), `conditions` (empty when
the expression is self-contained; otherwise the fields or variables it
needs, and `context described on the Claris page` when the example's sentence
names a field, record or condition; the sentence itself is not kept), `doc_url`. Backslash escapes:
`\n` (a line break shown in the docs, which FileMaker returns as CR), `\r`,
`\t`, `\\`.

- 549 `doc` rows are every `<code>expression</code> returns
  <samp>result</samp>` pair on the Claris pages, verbatim (235 of them are
  self-contained). Some expected values are prose (`an empty string ("")`)
  or images (`[image: ...]`) and need a human mapping before use; examples
  whose markup gave no pair are prose and are not kept. Non-breaking spaces the docs use for layout were
  turned into spaces and the entry is flagged `nbsp_normalized` (TrimAll's
  first example really contains them).
- `engine` rows follow directly from observed code; `inferred` rows still
  need an oracle run (they are the U-items below in vector form).

## 9. Unknowns and the expressions that settle them

| # | Question | Oracle expressions |
|---|---|---|
| U-1 | Negative counts (3.1) | `Left ( "abcdef" ; -1 )`, `Right ( "abcdef" ; -1 )`, `Middle ( "abcdef" ; 3 ; -1 )`, `LeftWords ( "a b c" ; -1 )`, `LeftValues ( "a¶b" ; -1 )` |
| U-2 | Counts modulo 2^32 | `Left ( "abc" ; 4294967296 )`, `Left ( "abc" ; 4294967297 )` |
| U-3 | Grapheme rules version | (type the text, `Char` can't reach these code points) `Length ( "👨‍👩‍👧" )`, `Length ( "🇯🇵" )`, `Length ( "각" )` (Hangul jamo sequence) |
| U-4 | Folding beyond case | `Position ( "café" ; "CAFE" ; 1 ; 1 )`, `PatternCount ( "Straße" ; "SS" )`, `Position ( "ｱ" ; "ア" ; 1 ; 1 )` |
| U-5 | CR LF in value lists | `ValueCount ( "a" & Char ( 13 ) & Char ( 10 ) & "b" )`, `GetValue ( "a" & Char ( 13 ) & Char ( 10 ) & "b" ; 2 )` |
| U-6 | Word and proper-case rules | `WordCount ( "don't stop-here x.y 3,5 a_b" )`, `LeftWords ( "e-mail me@x.com" ; 2 )`, `Proper ( "o'neil mcdonald-smith 3rd" )` |
| U-7 | Position/GetValue edge arguments | `Position ( "abc" ; "b" ; 0 ; 1 )`, `Position ( "abc" ; "b" ; 1 ; 0 )`, `Position ( "abcb" ; "b" ; 10 ; -1 )`, `GetValue ( "a¶b" ; 0 )`, `GetValue ( "a¶b" ; -1 )` |
| U-8 | TrimAll details | `TrimAll ( "a  b" ; 0 ; 0 )` and trimType 1-3 with mixed roman/kana input |
| U-9 | Random seeding | first `Random` after launch in two sessions (not reproducible by design) |
| U-10 | Quote escapes | `Quote ( "a" & Char ( 9 ) & "b" )`, `Quote ( "¶" )`, `Quote ( Char ( 13 ) )`, `Quote ( "\" )` |
| U-11 | Round/Mod signs | `Round ( -2.5 ; 0 )`, `Round ( 2.45 ; 1 )`, `Mod ( -7 ; 3 )`, `Mod ( 7 ; -3 )`, `Mod ( 7.5 ; 2 )` (value model) |
| U-12 | Financial edge cases | `PMT ( 1000 ; 0 ; 12 )`, `FV ( 100 ; 0 ; 10 )`, `PV ( 100 ; 0 ; 10 )`, `NPV ( "" ; .1 )` |
| U-13 | Hour/Minute/Seconds of long or negative times | `Hour ( Time ( 30 ; 0 ; 0 ) )`, `Hour ( -Time ( 1 ; 30 ; 0 ) )`, `Seconds ( Time ( 0 ; 0 ; 1.25 ) )` |
| U-14 | WeekOfYearFiscal out of range | `WeekOfYearFiscal ( Date ( 1 ; 1 ; 2024 ) ; 0 )`, `... ; 8 )`, `... ; 1.9 )` |
| U-15 | JSON strict-mode messages | `JSONGetElement ( "1" ; "" )`, `JSONGetElement ( "{\"a\":1,\"a\":2}" ; "a" )`, `JSONGetElement ( "{\"a\":1} x" ; "a" )`, `JSONGetElement ( "[1,]" ; "" )`, `JSONGetElement ( "{\"a\":1}" ; "b.c" )` |
| U-16 | JSON number formatting | `JSONSetElement ( "{}" ; "a" ; 1/3 ; JSONNumber )`, `... ; 1e20 ; JSONNumber )`, `... ; .1 + .2 ; JSONNumber )`, `... ; 12345678901234567890 ; JSONNumber )` |
| U-17 | JSON key order for non-ASCII keys | `JSONSetElement ( "{\"é\":1,\"z\":2}" ; "a" ; 1 ; JSONNumber )` |
| U-18 | Undocumented JSON functions | `JSONValueExists ( "[1,\"a\",\"A\"]" ; "" ; "a" )`, `JSONValueCount ( "[\"a\",\"A\",\"a\"]" ; "" ; "a" )`, `JSONUniqueValues ( "[1,1,\"1\",\"a\",\"A\"]" ; "" )`, `JSONRemoveValues ( "[1,2,3]" ; "" ; "[2]" )`, `JSONAnyValuesExist ( "[1,2]" ; "" ; "[3,2]" )`; first check that the parser accepts the names |
| U-19 | Hidden Get selectors | `Get ( DBEngineVersion )`, `Get ( StudioDatasource )` (does the parser accept them; what do they return) |
| U-20 | Trim and NBSP | `Length ( Trim ( Char ( 160 ) & "x" ) )` (engine table says 2) |
| U-21 | SetPrecision bounds | `SetPrecision ( 1/3 ; 10 )`, `SetPrecision ( 1/3 ; 500 )`, `Length ( SetPrecision ( Pi ; 400 ) )` |
| U-22 | WeekOfYear first weekday | `WeekOfYear ( Date ( 1 ; 7 ; 2024 ) )` (a Sunday), `WeekOfYear ( Date ( 1 ; 6 ; 2024 ) )` |
| U-23 | JSON nesting and array size beyond the reader's limit: does the engine build the result, fail, or abort (run on a disposable server) | `JSONSetElement ( "[]" ; "[16777216]" ; 1 ; JSONNumber )`, `JSONSetElement ( "[]" ; "[1000000][1000000]" ; 1 ; JSONNumber )`, `JSONSetElement ( "{}" ; "a" ; $deep ; JSONRaw )` and `JSONMakeArray ( $deep ; "" ; JSONRaw )` with `$deep` an array 1,000 levels deep, `JSONSetElement ( "{}" ; $path ; 1 ; JSONNumber )` with `$path` 20,000 keys `a.a.a...` |

### 9.1 Oracle answers (FileMaker Pro 26.0.3, English locale, 2026-10-28)

Run through `tools/calc-oracle` (cases and results:
`tools/calc-oracle/batches/functions-unknowns.{cases,results}.json`).
These are observations of the shipping client and override the inferences
above where they differ.

| # | Answer |
|---|---|
| U-1 | A negative count gives empty text: `Left`, `Right`, `Middle` (count -1), `LeftWords`, `LeftValues` all return "" |
| U-2 | Counts wrap modulo 2^32: `Left ( "abc" ; 4294967296 )` = "", `Left ( "abc" ; 4294967297 )` = "a" |
| U-3 | Legacy clusters (combining marks, jamo); ZWJ sequences and flags are not joined (section 3.2) |
| U-4 | Case folding only (section 3.3) |
| U-5 | CR LF is two separators (section 3.4) |
| U-6 | `WordCount ( "don't stop-here x.y 3,5 a_b" )` = 7; `LeftWords ( "e-mail me@x.com" ; 2 )` = `e-mail` (`-` separates words, so `e` and `mail` are the first two, and the separator between them is kept); `Proper ( "o'neil mcdonald-smith 3rd" )` = `O'neil Mcdonald-Smith 3rd`; `MiddleWords ( "one, two; three. four" ; 2 ; 2 )` = `two; three` (inner punctuation kept) |
| U-7 | `Position ( "abc" ; "b" ; 0 ; 1 )` = 2 (start 0 acts as 1); occurrence 0 gives 0; `Position ( "abcb" ; "b" ; 10 ; -1 )` = 4 (start past the end clamps for backward search); `GetValue` with 0 or -1 gives "" |
| U-8 | `TrimAll ( "a  b" ; 0 ; 0 )` = `a b`; for `"a  b  アイ  c"` with includeSpaces 1: trimType 1 and 2 give `a b アイ c`, trimType 3 gives `abアイc` |
| U-10 | `Quote` escapes `"` and `\` with a backslash, keeps tab literally, and writes CR (and `¶`) as `¶` |
| U-11 | `Round` is half away from zero on exact decimals (`Round ( -2.5 ; 0 )` = -3, `Round ( 2.45 ; 1 )` = 2.5, `Round ( 1234.5 ; -2 )` = 1200); `Mod` takes the divisor's sign (`Mod ( -7 ; 3 )` = 2, `Mod ( 7 ; -3 )` = -2, `Mod ( 7.5 ; 2 )` = 1.5); `Div ( -7 ; 2 )` = -4; `Int ( -2.5 )` = -2; `Truncate ( -2.567 ; 2 )` = -2.56; `Ceiling ( -2.5 )` = -2; `Floor ( -2.5 )` = -3 |
| U-12 | Zero rate: `PMT ( 1000 ; 0 ; 12 )` = `83.3333333333333333`, `FV ( 100 ; 0 ; 10 )` = 1000, `PV ( 100 ; 0 ; 10 )` = 1000; `NPV ( "" ; .1 )` = `?` with error 1214; `PMT ( 1000 ; .05/12 ; 12 )` = `85.6074817884671147` |
| U-13 | `Hour ( Time ( 30 ; 0 ; 0 ) )` = 30; `Hour ( -Time ( 1 ; 30 ; 0 ) )` = -1, `Minute` = -30; `Seconds ( Time ( 0 ; 0 ; 1.25 ) )` = 1.25; unary minus on a time returns a number (`GetAsText ( -Time ( 1 ; 30 ; 0 ) )` = -5400) |
| U-14 | Not settled: Jan 1 2024 is week 1 for start days 0, 1.9, 2 and 8; needs a date where the start day matters |
| U-15 | Strict-mode messages: `? * Line 1, Column 1` + CR + `  A valid JSON document must be either an array or an object value.`; duplicate key: `? * Line 1, Column 8` CR `  Duplicate key: 'a'`; trailing text: `? * Line 1, Column 9` CR `  Extra non-whitespace after JSON value.`; `[1,]`: `? * Line 1, Column 4` CR `  Syntax error: value, object or array expected.`; a missing key or path gives "" with no error |
| U-16 | 15 significant digits (section 5) |
| U-17 | Keys sort by UTF-8 bytes: `{"1":4,"B":2,"_":3,"a":1,"b":1}`; `é` sorts after `z` |
| U-18 | The parser accepts all five undocumented JSON names. Matching folds case and compares numbers and text by value: `JSONValueExists ( "[1,\"a\",\"A\"]" ; "" ; "a" )` = 1, `JSONValueCount ( "[\"a\",\"A\",\"a\"]" ; "" ; "a" )` = 3, `JSONUniqueValues ( "[1,1,\"1\",\"a\",\"A\"]" ; "" )` = `[1,"a"]`, `JSONRemoveValues ( "[1,2,3]" ; "" ; "[2]" )` = `[1,3]`, `JSONAnyValuesExist ( "[1,2]" ; "" ; "[3,2]" )` = 1 |
| U-19 | `Get ( DBEngineVersion )` = `260030307`; `Get ( StudioDatasource )` = 0 |
| U-20 | Trim removes U+00A0 (section 5, correction) |
| U-21 | Default division keeps 16 fractional digits (`1/3` = `.3333333333333333`, `2/3` = `.6666666666666667`, `PMT` above has 16 decimals); `SetPrecision` below 16 has no effect (`SetPrecision ( 1/3 ; 10 )` and `... ; 0 )` give 16 places); `SetPrecision ( 2/3 ; 20 )` = `.66666666666666666667`; precision is capped at 400 (`SetPrecision ( 1/3 ; 500 )` gives 400 places; `Length ( SetPrecision ( Pi ; 400 ) )` = 402); `Pi` = `3.1415926535897932`; `Sqrt ( 2 )` = `1.414213562373095` (15 decimals, double precision) |
| U-22 | Week 1 contains January 1 and weeks start on Sunday: `WeekOfYear ( Date ( 1 ; 6 ; 2024 ) )` = 1 (Saturday), `Date ( 1 ; 7 ; 2024 )` = 2 (Sunday), `Date ( 12 ; 31 ; 2024 )` = 53 |

### Oracle answers (date, time, text formatting and Japanese implementation)

Settled with `tools/fms-calc-harness` (FileMaker Server 26.0.3.309, the
Contacts file of each locale, 2026-10-28) while implementing
`om_calc::functions::{datetime, formatting, japanese}`. Batteries
`tools/fms-calc-harness/batteries/fn-{datetime,formatting,japanese}.txt`;
results in `crates/om-calc/tests/data/`. `→` gives the result, then the
error when not 0.

**Date**

- U-14: the starting day is truncated and clamped to 1..7 without error (0,
  `""`, text and `?` act as 1, 8 and above as 7). Week 1 is the first week
  with four or more days of the year; earlier days are in the last week of
  the year before. `WeekOfYearFiscal ( Date ( 1 ; 1 ; 2004 ) ; s )` for s =
  0..8 → 53, 53, 1, 1, 1, 1, 52, 52, 52 (the rule matches 2,700 values over
  15 years).
- `WeekOfYear` counts Sunday weeks in every locale, also where the first day
  of the week is Monday (de, fr, sv).
- `Date`: month and day must fit 16 bits (`Date ( 1 ; 32767 ; 2000 )` →
  9/16/2089, `Date ( 1 ; 32768 ; 2000 )` → 500); a year of 0 is 500 even
  when the month brings it into range (`Date ( 13 ; 1 ; 0 )` → 500); a
  negative year rolled into range gives the date but displays `?` without
  error (`Date ( 25 ; 1 ; -1 )` → `?`, `GetAsNumber` of it → 1); an empty
  component is 500, text counts by its digits (`Date ( "abc" ; 1 ; 2020 )`
  → 12/1/2019, `Date ( "a2" ; 1 ; 2020 )` → 2/1/2020).
- Date parts (`Day`, `Month`, `Year`, `DayOfWeek`, `DayOfYear`, `DayName`,
  `MonthName`, `WeekOfYear`, `WeekOfYearFiscal`, `DayNameJ`,
  `MonthNameJ`): an empty argument is returned as it is (`Day ( "" )` →
  empty text, `Day ( GetAsDate ( " " ) )` → empty date); a time gives an
  empty time without error (`Day ( Time ( 1 ; 0 ; 0 ) ) + 1` → 0:00:01);
  blank text is 500 (`Day ( " " )`); numbers outside 1..1460970 are 500.
- `Time`: `hours * 3600 + minutes * 60` is truncated to whole seconds before
  the seconds are added: `Time ( 1/3 ; 0 ; 0 )` → 0:19:59, `Time ( 0 ; 1/3
  ; 1/3 )` → 0:00:19.333333, `Time ( 0 ; -0.01 ; 0.5 )` → 0:00:00.5. Empty
  and text components count as their digits or 0 (`Time ( "" ; 1 ; "" )` →
  0:01:00).
- Hours are 32-bit: `Time ( 2147483647 ; 59 ; 59.999999 )` works, `Time (
  2147483648 ; 0 ; 0 )` → 501; `Time ( -2147483648 ; 0 ; 0 )` displays
  `-0:00:00` and `Time ( -2147483649 ; 0 ; 0 )` → 501. Elsewhere the limit is
  symmetric: `GetAsTime ( -7730941132800 )`, `GetAsTime ( 10^20 )`, `Hour (
  10^20 )` and `Time ( 2147483647 ; 0 ; 0 ) + 3600` → 501.
- `Hour`, `Minute`, `Seconds` split the elapsed time with its sign (`Time (
  -1 ; -30 ; -5 )` → -1, -30, -5; `Seconds ( Time ( 0 ; 0 ; 1/3 ) )` →
  .333333); a timestamp gives its time of day, a date its elapsed time
  (`Hour ( Date ( 1 ; 2 ; 1 ) )` → 24); empty arguments are returned as
  they are; blank text and text that is a date without a time are 0
  (`Hour ( " " )`, `Hour ( "1/2/2003" )` → 0, although `GetAsTime (
  "1/2/2003" )` → 501); `Hour ( "abc" )` → 501.
- `Timestamp`: an empty or blank time gives an empty timestamp (both empty:
  the date argument, `Timestamp ( "" ; "" ) + 1` → 1); a `?` time returns
  the date argument unchanged (`Timestamp ( Date ( 1 ; 1 ; 2001 ) ; 1/0 )`
  → 1/1/2001, error 15); every failure is 500, bad time text included; the
  typed date separator is dropped. Date text that is not a date is read
  field by field and rolled over, and text after the first word is ignored:
  `Timestamp ( "2/30/2003" ; 0 )` → 3/2/2003, `"13/45/2003"` → 2/14/2004,
  `"1/2/2003 abc"` → 1/2/2003, en `"2003-01-02"` → 11/1/2168. The time's
  whole days are added to the 16-bit day of the month, which wraps:
  `Timestamp ( Date ( 1 ; 31 ; 2001 ) ; 32738 * 86400 )` → 4/16/1911,
  `Timestamp ( Date ( 1 ; 1 ; 2001 ) ; Time ( -2000000 ; 0 ; 0 ) )` →
  4/10/1952 4:00 PM; the month and day text fields wrap the same way
  (`Timestamp ( "1/99999/2003" ; 0 )` → 12/4/1917). Time text that is a date
  counts as 0:00. Open: `Timestamp ( "1/2/0000" ; 0 )` → `12:00 AM` (a
  timestamp shown as its time alone).
- Date text (calc-values.md 4.6, correction): any punctuation or symbol of any
  script separates the fields, `+` and `:` excepted, as do CJK ideographs
  and the no-break space: `1,5,2003`, `1;5;2003`, `1_5_2003`, `1・5・2003`,
  `1年5年2003`, `1–5–2003`; other spaces, the zero-width space, digits and
  letters do not (`1 5 2003`, `1x5x2003`, `1あ5あ2003`, `1①5①2003` → 500).
  The typed separator is kept (`GetAsDate ( "1,5" )` → `1,5,2026`), and two
  fields joined by the decimal separator are still a number (`"2,1"` in
  de-DE → 500).

**Text formatting**

- Keyword values: `Plain` 0, `Strikethrough` 1, `SmallCaps` 2,
  `Superscript` 4, `Subscript` 8, `Uppercase` 16, `Lowercase` 32,
  `Titlecase` 48, `WordUnderline` 64, `DoubleUnderline` 128, `Bold` 256,
  `Italic` 512, `Underline` 1024, `HighlightYellow` 4096, `Condense` 8192,
  `Extend` 16384, `AllStyles` 32767; scripts `Roman` 0, `Greek` 1,
  `Cyrillic` 2, `CentralEurope` 3, `ShiftJIS` 4, `TraditionalChinese` 5,
  `SimplifiedChinese` 6, `OEM` 7, `Symbol` 8, `Other` 10. Bits 2048 and
  32768 are kept but show in no CSS.
- The harness's `~` style dump prints nothing for any result; `GetAsSVG`
  shows the runs (CSS with first and last UTF-16 position) and is what the
  tests compare. FileMaker leaves adjacent runs unmerged after
  `TextColorRemove`, `TextStyleRemove` and the like (`&` merges them).
- Numeric arguments are truncated, negatives count as 0, larger values wrap:
  colors modulo 2^24 (`TextColor ( t ; 16777217 )` → `#000001`, `TextColor
  ( t ; -1 )` and `TextColor ( t ; "" )` → black), sizes and style sets
  modulo 2^16 (`TextSize ( t ; 65548 )` → 12, `TextStyleAdd ( t ; 2^32 + 256
  )` → bold). A size of 0 changes nothing; `TextSizeRemove ( t ; 0 )`
  removes nothing. Results are text: `TextStyleAdd ( Date ( 1 ; 2 ; 2003 ) ;
  Bold ) + 1` → 122004.
- `RGB` does no clamping: `RGB ( 256 ; 0 ; 0 )` → 16777216, `RGB ( -1 ; 0 ;
  0 )` → -65536, `RGB ( 0 ; 0 ; 1.5 )` → 1.5, `RGB ( "" ; "" ; "" )` → 0.
- `Plain`, and every style value that wraps to 0 (`-1`, 65536, `""`,
  `"abc"`, `?`), sets every style explicitly normal, which `GetAsCSS` shows:
  `font-size: 12px;background-color: normal;font-weight: normal;font-style:normal;text-decoration:none;font-variant:normal;letter-spacing: normal;text-transform: none;`.
  Adding a style clears that group's normal state; `TextStyleRemove` keeps
  the normal state only of the styles it removes (`TextStyleRemove (
  TextStyleAdd ( t ; Plain ) ; Bold )` → `font-size: 12px;font-weight:
  normal;letter-spacing: normal;`), always keeps the position and spacing
  ones, and keeps the case one only when removing `Uppercase` or `Lowercase`
  alone. `TextStyleRemove ( t ; Plain )` changes nothing.
- Style groups: case (`Uppercase`, `Lowercase`, `Titlecase` = both),
  underlines (`Underline`, `WordUnderline`, `DoubleUnderline`, all shown as
  `underline`), position (`Superscript`, `Subscript`) and spacing
  (`Condense`, `Extend`). Adding one replaces the group (`Superscript` then
  `Subscript` → subscript); adding two at once keeps both (`Superscript +
  Subscript` → superscript, `Condense + Extend` → condensed). Removing
  clears a case style only when the removed bits cover it (`Titlecase`
  minus `Uppercase` stays `Titlecase`, `Uppercase` minus `Titlecase` →
  none) and clears the other groups when they overlap (`Condense + Extend`
  minus `Extend` → none, `Underline` minus `DoubleUnderline` → underline).
- CSS order: `font-family`, `font-size` (superscript and subscript show half
  the size rounded up, 12 when unset: `font-size: 6px`, size 13 → `7px`),
  `color`, `background-color` (`#FFFF00`), `font-weight`, `font-style`,
  `text-decoration:underline line-through ;`, `font-variant:small-caps;`,
  `letter-spacing: -2px;` / `2px;`, `text-transform: uppercase|lowercase|capitalize;`,
  `vertical-align: text-top;` / `text-bottom;`.
- `TextFont`: the name is used as given (`""` and `12` are names); the
  script argument has no visible effect and `TextFontRemove` ignores it.
  `TextFontRemove` matches names ignoring case and width (`"arial"`,
  `"Ａｒｉａｌ"`), not spaces. Font names are interned per session ignoring
  case and width, and the first spelling seen is the one displayed
  (`TextFont ( t ; "ARIAL" )` after `"Arial"` shows `'Arial'`), which a pure
  function cannot reproduce.

**Japanese** (results identical in the en-US, de-DE and ja-JP files except
through date text and number text)

- `DayNameJ` → `日曜日` .. `土曜日`; `MonthNameJ` → `1月` .. `12月` (ASCII
  digits); both take their argument like `DayName`.
- `YearName`: format 0 `平成20`, 1 `(平)20` (ASCII parentheses), 2 `H20`;
  the first year of an era is `元` in formats 0 and 1 (`令和元`, `(令)元`,
  `R1`). Eras start 1868-09-08 明治, 1912-07-30 大正, 1926-12-25 昭和,
  1989-01-08 平成, 2019-05-01 令和; earlier dates give `西暦1868`, `(西)1868`,
  `1868 CE`. Other formats act as 0 (1.9 as 1). Arguments that are not dates
  give year 0 without error (`YearName ( "abc" ; 0 )` → `西暦0`), numbers of
  1 or more are day numbers without an upper limit (`YearName ( 1460971 ; 0
  )` → `令和1983`), and date text that is not a date is read field by field:
  a month outside 1..12 counts as January, days roll over (`"4/31/2019"` →
  `令和元`, `"13/1/2019"` → `平成31`).
- Character conversions keep each character's style and return the argument
  unchanged (type included) when nothing changes; a number keeps its value
  under the new text (`KanjiNumeral ( 123 ) + 1` → 124).
  - `Hiragana`: katakana ァ..ヶ (ヴヵヶ → ゔゕゖ) and halfwidth katakana, voiced
    pairs combined (`ｶﾞ` → が), except `ｳﾞ`, `ﾜﾞ`, `ｦﾞ` → ヴ, ヷ, ヺ; `ｰ` → ー;
    ヷヸヹヺ, ヽヾ, ・ and halfwidth punctuation stay.
  - `Katakana`: ぁ..ゖ and ゝゞ → katakana; halfwidth katakana stay.
  - `KanaHankaku`: katakana with a halfwidth form (ガ → ｶﾞ, ヴ → ｳﾞ, ヷ → ﾜﾞ,
    ヺ → ｦﾞ), ・ → ･, ー → ｰ; ヮヰヱヵヶヸヹ, 。「」、 and ゛゜ stay.
  - `KanaZenkaku`: halfwidth katakana, voiced marks combined where a voiced
    katakana exists (`ｱﾞ` → アﾞ), ･ → ・; ｡｢｣､ and lone marks stay.
  - `RomanHankaku`: U+FF01..U+FF5E → ASCII, U+3000 → space, ￠￡￢￣￤￥￦ →
    ¢£¬¯¦¥₩, 〝〞 → “”. `RomanZenkaku`: ASCII → fullwidth, space → U+3000, “” →
    〝〞 (¢£¬¯¦¥₩ and ‘’ stay).
  - `KanjiNumeral`: ASCII and fullwidth digits → 〇一二三四五六七八九.
- `NumToJText`: separator and type outside 0..3 (or empty, text, `?`) are
  0. Types: 0 ASCII digits, 1 fullwidth (signs, `．` and `，` too), 2 kanji
  digits, 3 traditional (壱弐参四伍六七八九, 拾陌阡, 萬); kanji types keep
  ASCII `-`, `.` and `,`. Separators 0 and 1 write the display digits in full
  (no exponent: `10^400` has 401 digits, `1.5E-30` → `.0000…015`) and return
  text; separator 1 puts a comma every three characters left of the first
  `.`, so in comma-decimal locales the decimals join in (`1234567,5` →
  `123,456,7,5`). Separators 2 and 3 return a number shown as the text:
  separator 2 writes four-digit groups with 万 億 兆 京 垓 し 穰 溝 澗 正 載
  極 (10^24 is the hiragana `し`), skipping zero groups (`110000000` →
  `1億1000万`); separator 3 writes places, leaving out 1 before 十百千
  except in type 3 (`1010101010` → `十億千十万千十`, type 3 `壱拾`), with 穣
  for 10^28 in type 3. Past 極 the units stop: separator 2 writes the higher
  groups bare (`10^52 + 12345` → `11万2345`), separator 3 writes each
  non-zero digit from 10^48 up bare and adds 極 only after a non-zero digit
  in its place (`1234 * 10^48` → `一二三四極`, `10^49` → `一`). Their
  fraction is the display text of the fractional part (`10.05` → `壱拾.〇伍`,
  `1/3` → `0.3333333333333333`, `1.5E-30` → `01.5E-30`). Text without
  digits comes back unchanged (`"abc"`, `"-"`), blank text is 0 for
  separators 2 and 3 and empty otherwise; styles are dropped.
- `LunarDate` (one argument) and `SolarDate` (two) parse but fail when
  evaluated with 1223, "function referred to is not yet implemented"
  (`If ( 1 ; "a" ; LunarDate ( 1 ) )` → `a`).

### Oracle answers (Number, Trigonometric, Financial and Aggregate implementation)

Settled with `tools/fms-calc-harness` (FileMaker Server 26.0.3.309,
2026-10-28; batteries `fn-number`, `fn-trig`, `fn-financial`,
`fn-aggregate` and their `-intl` variants, results in
`crates/om-calc/tests/data/fn-*.tsv`), plus record-context runs on the
FileMaker Server sample (`calc-values.md` 12.1: Tasks with 12 related
records whose `TASK ID MATCH FIELD` is 7 1 3 12 5 6 8 11 9 2 10 4). `→` gives
the result, then the error code when not 0. Remember that `-10^n` is
`(-10)^n`.

**Number**

- Empty arguments: a single-argument function returns its argument
  (`Abs("")` → empty text, `Abs(GetAsDate(" "))` → empty date, also `Round`
  and `Truncate`). `Div` and `Mod` follow the operators: `Div("";2)`,
  `Mod("";2)` → 0, `Div("";"")`, `Mod("";"")` → empty, `Div(5;"")` → 15, and
  `Mod(5;"")` → 15 although `Mod(5;"abc")` → 5. An empty digits argument is
  0 (`Round(2.5;"")` → 3). `Factorial` of an empty number is 1 whatever the
  count (`Factorial("";2)`, `Factorial("";-1)` → 1; `Factorial(" ";2)`,
  `Factorial(0;2)` → 14). `Combination("";2)`, `Combination(5;"")` → empty.
- Text without digits is 0: `Abs("abc")` → 0, `Exp("abc")` → 1, `Lg("abc")`
  → 14, `Combination("abc";2)` → 14, `Acos("abc")` → 1.5707963267948966. The
  text `?` gives `?` without an error (`Abs("?")`, `Combination(5;"?")`).
- Dates are day numbers, times and timestamps seconds (`Abs(Date(1;1;2001))`
  → 730486, `Int(Time(1;30;0.5))` → 5400); `Abs` keeps a time a time
  (`Abs(-Time(1;0;0))` → 3600 because unary minus made a number) and returns
  a timestamp unchanged.
- The digits of `Round` and `Truncate` are `FixPt::AsLongLong` (the three
  lowest base-10^9 words with wrapping 64-bit arithmetic, sign after)
  narrowed to a 32-bit `int`: `Round(1.5;4294967296)` → 2,
  `Round(1.5;4294967297)` → 1.5, `Round(15;-4294967297)` → 20,
  `Round(1.55;18446744073709551617)` → 1.6, `Round(1.55;10^20)` → 1.55,
  `Round(1.55;10^30)` → 2, `Round(1.55;-0.9)` → 2. The smallest `int` leaves
  the number unchanged (`Round(123;-2147483648)` → 123) and digits from
  2147483641 up give 0 (`Round(1.55;2147483640)` → 1.55, `...41` → 0).
- `Mod` floors the quotient of `/` (rounded at p + 2), so a quotient that
  rounds to 0 leaves the dividend: `Mod(-1;9*10^18)` → -1, `Mod(-7;10^20)` →
  -7, while `Mod(-1;10^18)` → 999999999999999999 and `Mod(-7;3*10^18)` →
  2999999999999999993. This also gives the earlier `Mod(-1;1/3)` answer.
- `Factorial` and `Combination` take n of any size (`Factorial(10^30;3)` →
  `9.99999999999999999999999999997000000000000000000000000000002E+89`,
  `Combination(10^20;2)` → `4.99999999999999999995E+39`). `Combination(n;k)`
  is FallFact(n;k) / k! and fails with 14 when FallFact reaches 10^405
  (`Combination(330;165)` works, `Combination(350;175)`,
  `Combination(1000;500)` → 14).
- `Exp`: an argument above 5000 is 14 (`Exp(5000)` works,
  `Exp(5000.0000000001)` → 14); between -5000 and -801·ln 10 ≈ -1844.3707 it
  is 14 (`Exp(-1844.37)` → 0, `Exp(-1844.38)` and `Exp(-2000)` → 14); at -5000
  and below it is 0. `Exp(5000)` holds about 800 significant digits.
- Integer powers of non-integers are not exact for large exponents
  (square-and-multiply at p + 10 digits with every product truncated,
  calc-values.md 14): `1.5^100` → `...0416` (exact `...04167`), `1.5^360` has about
  37 correct significant digits, `1.333333333333333333^360` about 27.
- `GetAsBoolean` is truthiness (`GetAsBoolean(GetAsTimestamp(0))` → 0,
  `GetAsBoolean("１")` → 1, `GetAsBoolean("٣")` → 0, `GetAsBoolean(1/0)` → 1
  with 15).

**Trigonometric**

- `Degrees(x)` is `x * 180 / Pi` and `Radians(x)` `x * Pi / 180` with `Pi`
  held at p + 2 digits, p the larger of the evaluation precision and x's:
  `Degrees(1)*10^18` → 57295779513082320885 (180/π is ...876798),
  `Radians(10^20)` → `1745329251994329576.6666666666666667`,
  `Degrees(1.0000000000000000001)` → `57.2957795130823208825`,
  `Degrees(Atan(1))` → `44.9999999999999983`.
- Beyond the double range: `Atan(10^309)` → 1.5707963267948966 but
  `Atan(10^400)` → 0; `Asin(10^400)` → 1218.

**Financial**

- `FV`, `PV`, `PMT`: the exact algorithm is in calc-values.md 14 (the
  power at p + 4, so p + 14 inside it, then subtraction, product and
  division at p + 4), which also explains the observations below.
  Observed: they round (1 + r)^±n at w = p + 6 digits, the quotient by r
  (for `PMT`, r over 1 - (1 + r)^-n) at w, truncate the product with the
  payment at w, and round the result to p + 2: 135 of 145 held-digit runs
  match (`FV(50;.11/12;60)*10^18` → 3975903984291350008506; the exact value
  ends 8506572). The rest are FileMaker's inexact power for n = 360 and its
  exp/ln series for fractional n. A negative integer power is 1 / (1 + r)^n
  rounded directly; `PV(100;-.5;500)` → 15 because 0.5^500 rounds to 0.
- p is the larger of the evaluation precision and the payment's (principal's)
  precision: `PMT(1.0000000000000000001;.1;3)` → `.4021148036253776435`;
  the rate's and the periods' precisions do not count.
- Empty and digitless arguments count as 0 (`FV(100;"";10)` → 1000,
  `PMT(1000;.1;"")` → 15) and only all three empty give empty; `?` → `?`.
- `PMT(100;.1;0)`, `PMT(100;0;0)`, `PV(100;-1;10)`, `PMT(100;-1;10)` → 15;
  `FV(100;-1;10)` → 100; `FV(100;-1.5;2.5)` → 14; `FV(100;1;2000)` → 14
  while `PV(100;1;2000)` → 100; `FV(100;.1;-2)` → `-173.5537190082644628`;
  `FV(100;.1;2.5)` → `269.0587062858833719`.
- `NPV`'s payment must be a field: `NPV(100;.1)`, `NPV("";.1)`,
  `NPV(x * 1;.1)` → 1214. Over the 12 related values: rate .1 →
  42.7359122453516229, rate 0, "" or "abc" → 78, -1 → 15, -2 → -6, 1/0 → `?`
  (15); a plain field is one payment (12 / 1.1); no related records → 0. The
  value is the exact sum to within the last held digit, but FileMaker keeps
  only about 23 significant digits, so day-number payments differ in the
  displayed digits (`NPV(Rel::DueDate;1/3)*10^18` →
  2136908379656732084203907, exact ...084294609).

**Aggregate**

- A single argument that is not a field is 1214 (`Sum(5)`, `List("")`,
  `Sum(1/0)`, `Average(Abs(1))`); one field argument walks all repetitions
  or related records (`calc-values.md` 8.6, 8.7).
- Empty values are skipped everywhere and `?` numbers too (`Sum(1/0;2)` → 2
  with 15, `Count(GetAsNumber("?");1)` → 1, `Max(2;GetAsNumber("?"))` → 2),
  except that `List` shows them (`List(GetAsNumber("?");2)` → `?¶2`). `Count`
  counts every other value, digitless and `?` text included
  (`Count("";0;"x")` → 2, `Count("?";1)` → 2). `Sum`, `Average` and the
  variance functions skip values whose number is empty (`Sum("a";"b")` →
  empty, `Average("a";2)` → 2, `Sum("12abc";"3")` → 15).
- With no value: `Sum` and `Average` → empty number, `Max`, `Min`, `StDev`,
  `StDevP`, `Variance`, `VarianceP` → empty text, `Count` → 0, `List` →
  empty; the same for a relationship without records.
- `Sum` is exact at the largest precision (`Sum(0.1234567890123456789;1)` →
  `1.1234567890123456789`); `Average` divides at the evaluation precision
  (`Average(0.1234567890123456789;1)` → `.5617283945061728`). Dates add as
  day numbers (`Sum(Date(1;1;2001);1)` → 730487, a number).
- The variance is the exact sum of squared deviations over n - 1 or n,
  rounded at p + 2 (`Variance(1/3;2/3)*10^18` → 55555555555555556); `StDev`
  is the square root of that rounded value (`StDev(1;2;3;4)*10^18` →
  1290994448735805629 where √(5/3) gives ...628). With one value `StDev`,
  `Variance` and `VarianceP` → 15, `StDevP` → 0.
- `List` separates values with CR and adds none at the end; a value's own
  CRs stay (`List("a¶";"b")` → `a¶¶b`, `List("a";"¶")` → `a¶¶`).
- `Max` keeps the first value and takes a later one when it is `>`; `Min`
  takes a later one unless it is `≥`; so ties keep the first and `?` text
  wins against a number in `Max(1;"?")` but not in `Max("?";1)`, `Min(2;"?")`
  → 2, `Min("?";1)` → `?`. The winner is returned unchanged (`Max("10";9) <
  9.5` → 1: still text). Order: text against text by collation
  (`Max("10";"9")` → 9); a number against anything, and text against a
  number, as numbers (`Max("10";9)` → 10, `Max("abc";-1)` → `abc`,
  `Max(Date(1;1;2001);10^7)` → 10000000); a kept text value against a date
  or time by text (`Max("5";Date(1;1;2001))` → 5); a kept date, time or
  timestamp converts the candidate to its type (`Max(Date(1;1;2001);
  "1/2/2001")` → 1/2/2001, `Max(Time(1;0;0);Date(1;2;1))` → the date, 86400
  s, `Max(Date(1;1;2001);Timestamp(Date(1;1;2001);Time(1;0;0)))` → the date,
  same day). A candidate that does not convert is incomparable to a kept
  date or time (`Max(Date(1;1;1);Time(1;0;0))` → the date, `Min` → the time;
  `Max(Date(1;1;2001);"9999999")` → the date, `Min` → 9999999) and like `?`
  against a kept timestamp (`Max(Timestamp(…);"abc")` → abc, `Min` → the
  timestamp) and for date-like text against a date
  (`Max(Date(1;1;2001);"13/45/2001")` → 13/45/2001, `Min` → the date; also
  `2003-01-03` in de-DE and `03.01.2003` in sv-SE, which `GetAsDate`
  rejects with 500) [the last rule is inferred].
- Related records: `Sum` 78, `Average` 6.5, `StDev` 3.6055512754639893,
  `StDevP` 3.4520525295346632, `Variance` 13, `VarianceP`
  11.9166666666666667; `Max` and `Min` of a date field are dates, `Sum` and
  `Average` of it numbers (8830399, 735866.5833333333333333). Open: `Average`
  of a related text field without digits is 0 for one field (Category) and
  error 15 for another (Task), `Sum` empty for both.
- Collation (for the text owner): in de-DE FileMaker's `<` gives 0 both ways
  for `"ä"` against `"b"` and against `"z"`, `"af" < "ä"` and `"ä" < "zz"`
  → 1, `"ø" < "ö"` → 0, and `Max("ä";"b")` → ä; in sv-SE `Max("æ";"ä")` →
  æ.

### Oracle answers (text implementation)

FileMaker Server 26.0.3.309 through `tools/fms-calc-harness`, 2026-10-28,
Contacts templates in en-US, de-DE, fr-FR, sv-SE and ja-JP (battery
`batteries/fn-text.txt`, fixtures `crates/om-calc/tests/data/fn-text.*.tsv`;
per-character tables from every BMP code point, `text_tables.py`). These
override sections 3 and 4 where they differ. `→` gives the result, then the
error when not 0.

**Integer arguments** (3.1). `AsLongLong` keeps the low three base-10^9
words of the integer part, wrapping in 64 bits (`Left ( "abcdef" ;
10^30 + 2 )` → `ab`), truncates held digits (`2.99999999999999999999` → 2),
and functions then read the low 32 bits differently:

| Reading | Functions | -1 | 2^31 | 2^32 + 1 |
|---|---|---|---|---|
| count, unsigned | `Left`, `Right`, `Middle` count, `LeftWords`, `RightWords`, `*Values` counts | 0 | all | 1 |
| count, signed | `Replace` count, `MiddleWords` count | 0 | 0 | 1 |
| start, signed (≤ 0 → 1) | `Middle`, `Replace`, `MiddleWords` | 1 | 1 | 1 |
| start, unsigned (0 → 1) | `Position` start, `MiddleValues` start | 1 | past the end | 1 |
| two's complement | `Position` occurrence, `GetValue`, `TrimAll` trimType | -1 | -2^31 | 1 |

A negative value counts as negative whatever its low bits
(`Left ( "abcdef" ; -4294967295 )` → ""), but in the last row `-4294967295`
is 1 (`GetValue ( "a¶b" ; -4294967295 )` → `a`). Empty, `?` and text without
digits are 0 (`Left ( "abcdef" ; 1/0 )` → "" with 15).

**Characters and searching** (3.2, 3.3)

- `Position`, `PatternCount` and `Substitute` match code points, not
  characters: `PatternCount ( "e" & Char ( 769 ) ; "e" )` → 1,
  `Substitute ( "a" & ¶ & Char ( 10 ) & "b" ; Char ( 10 ) ; "x" )` → `a¶xb`.
  `Position` reports the characters before the match plus one
  (`Position ( "e" & Char ( 769 ) & Char ( 769 ) ; Char ( 769 ) ; 1 ; 1 )`
  → 2); start is a character position; a backward search finds matches
  starting at or before it, not overlapping (`Position ( "aaaa" ; "aa" ;
  4 ; -2 )` → 1); no normalization (`Position ( "é" ; "e" & Char ( 769 ) ;
  1 ; 1 )` → 0).
- Case folding is the `Lower` mapping: `İ` matches `i`, `ı` does not match
  `I`, `ς` does not match `σ`, `ǅ` matches `ǆ`.
- `Upper`, `Lower` and `Proper` map each UTF-16 unit by a Unicode 5.2-era
  simple table, the same in every locale: `ß`, `ﬁ`, `ŉ`, `ǰ` stay, `İ` → `i`,
  ypogegrammeni capitals upper to title-case forms (`ᾀ` → `ᾈ`), later
  additions (`ꭰ`, `Ɥ`, Georgian Mtavruli) and every supplementary character
  stay. `Lower` writes `ς` for a `Σ` after a non-space and before the end or
  a space (tab-CR, space, U+1680, U+180E, U+2000-U+200B, U+2028, U+2029,
  U+205F, U+3000): `Lower ( "ΑΣ." )` → `ασ.`, `Lower ( "1Σ" )` → `1ς`;
  `Proper` does not (`ΟΔΟΣ` → `Οδοσ`).
- `Exact` compares the text forms by code point, styles ignored:
  `Exact ( 1 ; "1" )` → 1, `Exact ( Date ( 1 ; 2 ; 2003 ) ; 731217 )` → 0,
  `Exact ( 1/0 ; "?" )` → 1 with 15.
- `Filter` keeps characters equal to a character of filterText:
  `Filter ( "a" & ¶ & Char ( 10 ) ; ¶ )` → "" (CR LF is one character).
- `Char`: five-digit groups from the right, each taken modulo 2^16 as a
  UTF-16 unit, groups that come to 0 dropped (`Char ( 97098 )` → U+7B4A,
  `Char ( 128512 )` → U+6F60 U+0001, `Char ( 65536 )` → "",
  `Char ( 5683255357 )` → `😀`); fractions truncated, negative → "",
  `Char ( 1/0 )` → `?`. A lone surrogate is a character of its own.
- `Code` counts UTF-16 units: more than 80 → `?` with error **503**
  (`Code` of 79 letters and `😀` too).
- `Trim` removes U+0020, U+00A0, U+1680, U+180E, U+2000-U+200B, U+205F,
  U+3000 (not U+202F, tab, CR, LF, U+FEFF).
- `Quote` escapes `"` and `¶` (U+00B6) with a backslash, a backslash only
  where it would start an escape (before `"`, `\`, `¶`, a return or the end:
  `Quote ( "a" & Char ( 92 ) & "b" )` → `"a\b"`, `Quote ( Char ( 92 ) )` →
  `"\\"`), writes CR as `¶` and U+E004 as CR, and keeps LF, tab, U+2028.

**Words** (3.4, U-6). Classes are FileMaker's own per-character table.
Letters, digits and other word characters (controls, U+00A0, ZWJ, emoji)
run together; Han characters (with Hangul syllables, Yi, the private use area
and most code points from U+2E80, U+20000 and up), hiragana, and katakana
each form runs but break against each other and against letters
(`漢字かな abc` → 3 words, `日本語のテキストです` → 4); `ー` and `゛` `゜`
continue a kana run (`コーヒー` → 1, `あーア` → 2), `゛` also lets a digit
follow (`ア゛1` → 1, `1゛2` → 2); a combining mark joins whatever surrounds
it (`1` + U+0301 + `漢` → 1). Joiners hold a word between two letters (`'`
`‘` `’` `＇` `։`), two digits (`,` `-` `:` U+00AD `−` and the `/` and `.`
families) or either (`.` `۔` `﹒` `．`); the character after a joiner must be
the letter or digit itself, not a mark (`a'` + U+0301 + `b` → 2). The file
changes three families: `.` joins digits only where `.` is the decimal,
thousands or date separator (not fr-FR, sv-SE: `1.2` → 2 words), `/` only
where it is one (not de-DE, sv-SE), and fr-FR separates at apostrophes
(`don't` → 2, `Proper ( "o'neil" )` → `O'Neil`). `LeftWords`, `MiddleWords`,
`RightWords` return from the first word's start to the last word's end,
inner punctuation kept. `Proper` upper-cases the first character of each
word, lower-cases the rest of it and leaves the rest alone (`X..Y`, `1.Y`,
`A1,2b`).

**Value lists** (3.4)

- `LeftValues`, `MiddleValues`, `RightValues` copy the text from the first
  value's start to the last value's end with the separators between as they
  were, then add CR in the style of the last character:
  `LeftValues ( "a" & Char ( 10 ) & "b" ; 2 )` → `a` LF `b` CR,
  `RightValues ( "a¶b¶¶" ; 1 )` → `¶`. `GetValue` index ≤ 0 → "".
- `FilterValues` compares text with case folded (not by type:
  `FilterValues ( "1¶01¶1.0" ; "1" )` → `1¶`, `é` ≠ `e`, `ß` ≠ `ss`, `ｱ` ≠
  `ア`), keeps duplicates and order, and ends each kept value with its own
  separator, or CR for the last (`FilterValues ( "a" & Char ( 10 ) & "b" ;
  "a" )` → `a` LF).
- `SortValues` / `UniqueValues`: datatype is truncated, ±1-5 select the
  type and direction, anything else (0, 6, -6, text) is text ascending;
  descending reverses the comparison and keeps ties in order. Typed values
  sort blank values first and values that do not convert last (equal to each
  other: `UniqueValues ( "1¶01¶1.0¶abc¶¶x" ; 2 )` → `1¶abc¶¶`). Locale names
  match without case (`english`), `""` is the file's, an unknown name (or
  `" English"`) → `?` with no error. Text sorting collates whole values with
  DUCET 5.2: primary, then accents (French from the end: `cote côte coté
  côté`), case ignored and stable (`GREEN¶green¶Green` stays), `ss` < `ß` <
  `st`; German is phonebook (`ab ae äb af`, `ä` < `ae` on a tie, `o ø ö oe`);
  Swedish puts `å ä ö ő ø` after `z`, `ü ű` after `y`, `w` with `v`, but
  not `æ`; Japanese adds a third level (lower case, hiragana and narrow forms
  first, small kana last: `あ ア ぁ ァ`, `か ｶ カ`); Turkish dotless `ı`
  sorts before `i`. `UniqueValues` compares index keys: primary weights
  (`é¶e¶E` → `é`, `ß¶ss` → `ß`, `a` ≠ `a` + tab), Unicode_Standard also
  accents, Unicode_Raw code units, German `ä` = `ae` = `æ`, Swedish `v` = `w`,
  `ü` = `y`, `ø` = `ö`, Japanese tells voiced, small and katakana forms apart
  but not width, case or other accents. Each result value ends with CR in
  the style of its last character.

**Other functions**

- `TrimAll`: spaces are U+0020 and, with trimSpaces (truncated, non-zero),
  U+3000, which then becomes U+0020; without it U+3000 is never removed and
  a run holding one keeps only those (`TrimAll ( "a 　 b" ; 0 ; 0 )` →
  `a　b`). Inner runs become one space or none by the characters around them
  (roman below U+2F00): two non-roman none; two roman one (type 3 none);
  mixed none for 0 and 3, one for 1 and 2; type 1 also inserts one between
  touching roman and non-roman characters; other types act as 0. The kept
  space has the style of the run's first space, an inserted one that of the
  character before it.
- `SerialIncrement`: the last run of Unicode 5.2 decimal digits (not `²`,
  not U+0DE6) plus the increment truncated toward zero, at any size
  (`abc99999999999999999999` + 1), written at least as wide as the run with
  zeros on the left of the sign (`abc000` - 1 → `abc0-1`, `abc10` - 11 →
  `abc-1`), full width when the run had a full-width digit (`x０` - 1 →
  `x－１`); digits of other scripts read as 0 (`x١٢` + 15 → `x15`); a `-`
  before the run is not a sign (`a-5` + 1 → `a-6`); without digits the
  increment is appended (`abc` - 1 → `abc-1`, `abc` + `""` → `abc`); `?`
  increment → `abc0?`. Styles are dropped.
- `GetAsURLEncoded`: UTF-8, every byte but `A-Z a-z 0-9 : - _ . ! ~ * ' ( )`
  as `%XX` in **upper-case** hex (`français` → `fran%C3%A7ais`; the Claris
  example shows lower case).
- `GetAsCSS` escapes CR and LF as `<br/>`, `"` `&` `<` `>` and 248 Latin-1,
  Greek and symbol characters as HTML 4 named entities (with `&prop;` at
  U+221E and `&infin;` at U+221F), other characters outside printable ASCII
  as decimal entities, and leaves supplementary characters as they are.
  `GetAsSVG` numbers every run (repeated styles too) with 1-based inclusive
  UTF-16 positions: `<stylelist>` CR, `<style#N>"css", begin: B, end:
  E</style> ` CR per run, `</stylelist>` CR `<data>` CR, `<span
  style="N">text</span>` CR per run, `</data>`; empty text → "".
- `Substitute`'s list form takes pairs only: `Substitute ( t ; [ a ; b ] )`
  works, `[ a ; b ; c ]` and an odd argument count → 1202, `[ a ]` → 1201, a
  list followed by plain arguments → 1211 (parser errors). Pairs apply in
  order to the previous result. A replacement takes the style of the first
  character it replaces (for an insertion the next character, at the end
  the last) with its own attributes laid over it (bold under an italic
  replacement → bold italic; a `Plain` replacement → explicitly normal).
- `GetAsNumber ( "" )` → "" (text); `GetAsNumber` drops styles.

### Oracle answers (container encoding and JSON implementation)

FileMaker Server 26.0.3.309 through `tools/fms-calc-harness`, 2026-10-28,
Contacts templates (batteries `fn-container.txt`, `fn-json.txt` in en-US,
`fn-container-intl.txt`, `fn-json-intl.txt` in all five locales; fixtures
`crates/om-calc/tests/data/fn-{container,json}*.tsv`). These override 5.4,
5.5 and U-15..U-18 where they differ. `→` gives the result, then the error
when not 0.

**Data and results** (5.4). A data argument is container data's main
stream, else its text as UTF-8. Only `Base64Encode` rewrites text line
breaks (CR, LF, CRLF, U+2028, U+2029) as CRLF (`Base64Encode ( "a¶b" )` →
`YQ0KYg==` CRLF); `Base64EncodeRFC`, `HexEncode`, the digests and the crypt
functions read CR as `0D`. Empty text gives empty text from the Base64
encoders and the crypt functions, but empty container data is still
encoded (`Base64Encode` → CRLF, `Base64EncodeRFC ( 4880 ; ... )` → CRLF
`=twTO` CRLF). Decoded bytes returned as text are UTF-8 with each invalid
sequence shown as U+FFFD and CRLF/LF folded to CR. Container results:
`digest.data`, `hmac.data`, `encrypted.data`, `signature.data`,
`<encoding>.txt` (lower case) for `TextEncode`; `GetAsText` of a container is
that name, `GetAsNumber` 0, `Length` the byte count; `=` on two containers
compares name and bytes and orders nothing (`<`, `>` → 0, `≤`, `≥` → 1 when
they differ).

- `Base64EncodeRFC`: the RFC number is read like a count, sign dropped,
  modulo 2^32 (`-1421`, `1421 + 2^32`, `"1421"` act as 1421); every line
  including the last ends with CRLF; 4880 appends `=` + Base64 of the
  OpenPGP CRC-24 and CRLF.
- `Base64Decode`: characters outside the alphabet are skipped (`QU!JD` →
  `ABC`), the data ends at the first `=` (`QQ==QQ==` → `A`); a lone digit in
  the last group, or 2-3 digits without `=`, → `?` 17.
- `HexDecode`: either case; spaces, tabs, CR, LF are skipped between digits
  (`4 669` → `Fi`); an odd digit count, any other character (`0x`, `:`,
  U+00A0) → `?` 17; whitespace after the last digit is accepted only when the
  digit count is a multiple of 4 (`4641 ` works, `46 ` and `464142 ` → 17).
- File names (`fileNameWithExtension`): first non-empty line (lines end at
  CR only), a `type:` prefix (a `:` before the first `/`) and directories
  removed, other `:` → `.`, `\ * ? < > |` → `_`, nothing left →
  `Untitled.dat` (`a:b:c.txt` → `b.c.txt`, `a/b:c.txt` → `b.c.txt`,
  `filewin:/C:/a.txt` → `a.txt`); an empty name returns text.
- `TextEncode`/`TextDecode`: names exactly as on the Claris page, any case
  (`utf8`, `latin1`, `utf-16` → `?` 4); `lineEndings` is truncated (`3.7` is
  LF) and 2/3/4 turn CR, LF, CRLF, U+2028, U+2029 into CR, LF, CRLF; other
  values change nothing. `macintosh` is old Mac Roman (`0xDB` `¤`, `0xBD`
  U+2126, `0xF0` unmapped); `shift_jis` is JIS X 0208 (`0x5C` `¥`, `0x7E`
  `‾`, `0x8160` U+301C, `0x817C` U+2212, user-defined rows to the PUA, no NEC
  or IBM rows); `gb18030` reads `0x80` as nothing. Decoding skips undecodable
  bytes (`c3 41` → `A`, an unmapped single byte vanishes) but a sequence cut
  off by the end → `?` 17; empty container data → `?` 2, which ends the whole
  evaluation (`HexEncode ( TextDecode ( HexDecode ( "" ; "a" ) ; "utf-8" ) )`
  → `?` 2). Encoding leaves out characters the target lacks after trying
  their compatibility form (`－` → `-`, `™` → `TM`, `…` → `...`); the engine
  goes further with a transliteration table (`€` → `EUR`, `é` → `´e` in
  `shift_jis` and `euc-kr`, `'e` in `windows-1251` and `big5`, `①` → `(1)`,
  `Ω` → `Ohm`), not reproduced.
- Digests and HMAC: `MD5`, `SHA1`, `SHA224`, `SHA256`, `SHA384`, `SHA512`
  in any case, empty = SHA-512; anything else (`SHA-256`, ` SHA256`,
  `SHA3-256`, `RIPEMD160`) → `?` 4. `CryptAuthCode` with an empty key returns
  the data itself (`HexEncode ( CryptAuthCode ( "abc" ; "MD5" ; "" ) )` →
  `616263`); a container key uses its bytes.
- `CryptEncrypt` format (no public description; recovered by round trips in
  both directions, the engine decrypts what OpenMaker writes): 4 random salt
  bytes in clear, then AES-128 in counter mode starting at block counter 2
  (GCM's keystream, no tag) over: 16 random bytes; a byte whose low nibble
  counts trailing padding bytes; 1 for text or 0 for container data; u16 BE
  file name length; `FP12`; u64 BE data length; the data (UTF-8 text or the
  main stream); the file name; `0x20`; SHA-256 of everything from the 16
  random bytes through the name; 0-15 random bytes. Key: PBKDF2-HMAC-SHA1 of
  the key text's UTF-8, the salt, 15 iterations, 60 bytes: AES key = bytes
  0-15, GCM nonce = bytes 48-59. Decryption checks `FP12` and the digest; a
  wrong key, too few bytes or another format → `?` without an error. An empty
  key → `?`; empty text → empty text; a result is 69 bytes plus the data,
  the name and the padding. Decrypted text keeps LF (`Code` 10).
- Signatures are RSASSA-PKCS1-v1_5, identical to `openssl dgst -sign`; keys
  as PKCS#1, PKCS#8, OpenSSL-encrypted PKCS#1 (`DEK-Info`) and encrypted
  PKCS#8, public keys as SPKI or PKCS#1, CR or LF line ends. The algorithm
  is checked first (→ 4), then an empty key or signature → `?` without an
  error, then an unreadable key or wrong password → `?` 5. A signature that
  is not one (garbage, wrong length) verifies as 0.

**JSON reading** (5.5, U-15). The reader is jsoncpp strict mode with its
error recovery: after an error it skips to the next `]` or `}` (whatever
the nesting) and goes on, so a later `Extra non-whitespace after JSON
value.` is still reported (`[1 2] x` → two messages; `[[1 2] x` → one).
Messages: `Syntax error: value, object or array expected.`, `Missing ','
or ']' in array declaration`, `Missing ',' or '}' in object declaration`,
`Missing '}' or object member name`, `Missing ':' after object member
name`, `Duplicate key: 'k'`, `'1e' is not a number.`, `Bad escape sequence
in string`, `Bad unicode escape sequence in string: four digits expected.`
/ `hexadecimal digit expected.`, `additional six characters expected to
parse unicode surrogate pair.`, `expecting another \u token to begin the
second half of a unicode surrogate pair` (string errors add `See Line L,
Column C for detail.`), the root message at 1:1, and `? Exceeded stackLimit
in readValue().` beyond 1000 nested levels. Columns count UTF-8 bytes; CR,
LF and CRLF end lines. Empty text → syntax error at 1:1 plus the root
message. Quirks kept: `01` is 1, `-` is 0, `+1` and `1.` are doubles,
`1e400` is not a number, comments are accepted after an element (`[1/*c*/]`)
but not before a value, `{"":1,}` is accepted, a lone `\udc00` reads as
three U+FFFD, a leading BOM is skipped. Values given as `JSONObject`,
`JSONArray`, `JSONRaw` use jsoncpp's defaults: comments, trailing commas, any
root, duplicate keys (last wins), trailing text ignored (`"1 2"` → 1);
`JSONRaw` that does not read is stored as a string. JSON text arguments are
the value's text (`JSONGetElement ( 5 ; "" )` reports a bad root), but
`JSONParse`, `JSONFormatElements` and the value arrays of
`JSONRemoveValues`/`JSONAnyValuesExist` read a number, date or time as
empty text. `JSONSetElement` with a number as its JSON returns the number
11.11 (dates and times `?`), not reproduced.

**Numbers** (U-16). Parsed integers stay exact up to 2^64 - 1, negatives to
-2^63; FileMaker numbers become integers when whole and within ±(2^63 - 1),
else doubles from every held digit including guard digits (`10^-5/7` →
`0.000001428571428571`), null beyond the double range (`10^400`), 0 for
empty, `?` or text without digits. Doubles print with 15 significant digits
in fixed notation for decimal exponents -19..14 and `%g` exponent form
outside (`1e-7` → `0.0000001`, `1e-19` → `0.0000000000000000001`, `1e-20`
→ `1e-20`, `1e14` → `100000000000000.0`, `1e15` → `1e+15`), with `.0` when
nothing follows the point (`1e2` → `100.0`, `123456789012345.678` →
`123456789012346.0`). Number text in `JSONGetElement` and `JSONListValues`
results uses the file's decimal separator (`0,1`, `100,0` in German); JSON
text keeps `.`.

**Paths** (5.5). `.`-separated keys with empty keys skipped (`a..b`, `.a.`),
`[n]` (`[]` is `[0]`, digits wrap modulo 2^32), `[:]`, `[+]`, `['k']` up to
the next `']` (`['q'r']` is the key `q'r`); a key runs to the next `.` or
`[` (`a]`, `x y`, `a.b[2]e`). Syntax errors: `? * Offset N in "path"` CR `
JSON path syntax error.` with N the byte where it failed (`a[` 2, `[a]` 1,
`['a` 3, `a.b[ 1 ]` 4, `a.b[1.5]` 5). Dot keys never index (`a.1` on an
array → "", `a.0` in `JSONSetElement` → objectValue error). A whole number
given as the whole argument indexes a root array (`1`, `" 1"`, `"1.0"`,
`"+1"`, `"1e0"`, `4294967296` → 0; in a German file `"1,0"` indexes and
`"1.0"` does not); on an object it is a key path (`1.5` → keys `1`, `5`).
`JSONSetElement` creates objects and arrays through null, pads arrays with
null, does nothing for `[:]` of an empty array, and fails with `? in
Json::Value::resolveReference(key, end): requires objectValue` (a key into
an array or scalar) or `? in Json::Value::operator[](ArrayIndex): requires
arrayValue` (an index into an object or scalar). `JSONDeleteElement` with
an empty path returns its JSON argument unchanged and unparsed; `.` and
paths naming nothing change nothing.

**Results** (5.5). `JSONGetElement`: objects/arrays compact, numbers and
booleans as numbers shown as their JSON text (`100.0`, `1`), strings with
LF, CRLF, U+2028, U+2029 as CR (NEL stays), null and missing elements "".
`JSONListValues` leaves nulls out, keeps strings' line breaks; `JSONListKeys`
lists every index. `JSONGetElementType` of a missing element → `? Incorrect
key, index, or path`. `JSONSetElement` with path "" replaces the root, also
with a scalar (shown as `JSONGetElement` would show it). `JSONFormatElements`
is jsoncpp's styled writer: tab indent, `" : "` then a line break before an
object or multi-line array value, arrays of plain values on one line `[ 1, 2 ]`
unless they have 25+ elements or reach 74 bytes, empty `[]`/`{}`, no trailing
line break, CR line ends. Output strings escape `"`, `\`, `\b \f \n \r \t` and
other controls as `\u00XX`, everything else raw (DEL, NEL, BOM).
`JSONMakeArray`: empty separator splits values (a trailing return adds
nothing, CRLF makes an empty value), other separators split literally,
case-sensitive (a trailing one adds ""); empty input → `?` -1. Types:
`JSONNumber` reads text as a number (`"abc"` → 0, `"12abc"` → 12, `"1,5"` →
15 in English); `JSONBoolean` is true for the text `true` (only lower case)
and otherwise the value's truth (`"a1"`, `"?"` true); `JSONObject`/`JSONArray`
anything else → `{}`/`[]`; unknown type numbers (7, -1, "") act as `JSONRaw`.

**JSONParsedState** (cache). JSON results carry a parsed state: the type for
`JSONParse`, `JSONSetElement`, `JSONDeleteElement`, `JSONFormatElements`,
`JSONMakeArray`, `JSONUniqueValues`, `JSONRemoveValues` and object/array
results of `JSONGetElement`; -1 for their parse or path errors; 0 for scalar
`JSONGetElement` results, its errors, `JSONListKeys`/`Values` and anything a
text operation produced (`& ""`, `GetAsText`, `Left`; `If` passes it on).
`JSONParse` returns its text unchanged (`"  [1]  "`). A value with state -1
reports its original error to the next JSON function
(`JSONFormatElements ( JSONParse ( "[3" ) )` → the missing `]`).

**Undocumented value functions** (U-18). They compare jsoncpp's `asString`
of each element (null "", booleans `true`/`false`, doubles with 17 digits:
`1.0`, `0.10000000000000001`) with the value's text, equal at collation
secondary strength in English, German, French and Swedish files (case, width
and kana type ignored: `a` = `Ａ`, `あ` = `ア`; accents count: `é` ≠ `e`) and
exactly in Japanese files. `JSONValueExists`/`Count` give 0 for a non-array
or missing element; `JSONUniqueValues` keeps first occurrences and returns a
non-array element as it is (null when missing); `JSONRemoveValues` and
`JSONAnyValuesExist` read their value array strictly (a scalar → root error,
an object counts as no values). The engine aborts (uncaught
`Json::Value::asString` exception) as soon as one of these functions meets
an object or array element; OpenMaker treats such elements as matching
nothing. Extra arguments → `?` -1. OpenMaker computes each value's
collation key once and compares keys in a set, so these functions take time
linear in their arrays (before, 5,000 distinct strings took 7 s in
`JSONUniqueValues`).

**Limits** (U-23). The reader's 1,000 levels are the only limit observed.
jsoncpp keeps an array as a map from index to element and pads it with
nulls only when it writes the text, and its writer and destructor recurse
once per level, so FileMaker presumably builds values deeper than its
reader accepts and arrays of any length (inference from jsoncpp's source;
unmeasured).
OpenMaker's choices, for a server where a stack overflow or a failed
allocation ends the process: no JSON function builds a value deeper than
the reader's limit (a `JSONSetElement` or `JSONMakeArray` result that would
be deeper is `? Exceeded stackLimit in readValue().` with state -1, what
reading it back gives); one `JSONSetElement` call adds at most 1,000,000
elements to arrays over all its triples and path segments (nulls up to the
indexes it names, and `[+]`), and returns `?` beyond that; a path of 1,000
or more segments names nothing (its syntax is still checked to the end).
Reading, writing and dropping values keep their nesting on the heap, not
the thread's stack. `JSONFormatElements` indents each line by its depth as
jsoncpp does, so its result can be about 500 times its input for a
document nested near the limit (a line of 999 tabs for each `0,`); it stops
with error 7 once its text passes the evaluation's text limit, as every
function that builds text does (scripting.md, Calculation limits).

### Oracle answers (container analysis)

FileMaker Server 26.0.3.309 through `tools/fms-calc-harness`, 2026-10-29,
Contacts templates (battery `fn-container-analysis.txt` in en-US,
`fn-container-analysis-intl.txt` in all five locales; fixtures
`crates/om-calc/tests/data/fn-container-analysis*.tsv`). Samples are inline
Base64 decoded with a file name (`Base64Decode ( "<data>" ; "a.png" )`).
These override 5.4 where they differ. `→` gives the result, then the error
when not 0.

**Typing decoded data.** Data a calculation makes is typed by its file
name's extension after the last `.`, any case, as FileMaker types a file it
stores: `jpg`, `jpeg`, `jpe`, `jfif` (JPEG, with photo attributes), `png`,
`gif` are pictures whatever the bytes (PNG bytes named `a.jpg` are a 3 x 2
JPEG-typed picture; text named `a.png` is a picture with `Width: ?`); `pdf`
is a PDF; `mp3`, `m4a` are audio; `wav`, `aif`, `aiff`, `au`, `mov`, `qt`,
`mp4`, `m4v`, `avi`, `wmv`, `wma`, `mpg`, `mpeg` are media without
attributes; `bmp`, `tif`, `tiff`, `webp` are pictures only when the bytes
decode (FileMaker converts them, TIFF and WebP with photo attributes);
everything else is a plain file (`jp2`, `psd`, `svg`, `heic` on Linux,
`aifc`, `snd`, `m4p`, `flac`, `aac`, `txt`, `html`, a name without `.`, or
`a.png ` with a trailing space). Stored streams (Internal Size): the data, an
`FNAM` of `4 + 1 + len(type) + 2 + len(SCSU name)` with type `image`,
`movie` or `file` (PDF too), `SIZE` (4) for a readable picture, `DPI_` (4)
when it declares a resolution, a 14-byte stream when its EXIF orientation
is 2-8, each plus an 8-byte directory entry (a 73-byte PNG named `a.png` is
118; `日本.png` 123). BMP, TIFF, WebP and PDF also store a converted copy
(a 2,359-byte PDF is 6,659), which OpenMaker does not reproduce.

**GetWidth, GetHeight.** Pixels after EXIF orientation (orientation 5-8
swap: an `o6` 4 x 3 JPEG is 3 x 4); a PDF's first page in points: the crop
box, swapped by `/Rotate 90`, truncated (612.6 x 791.5 → 612 x 791); 0 for
text, numbers, empty, `?` (with its error), files, media and pictures that
do not decode.

**GetContainerAttribute.** Names match with spaces removed, any ASCII case
(`" all "`, `"dpi width"`, `"File Size"`, `"FILENAME"`); `file_size`, a tab,
full-width letters, `""` or a number → `?`. Groups give `Label: value`
lines joined by CR (`Label:` when empty); `all` gives every group that
applies, each headed `[General]`, `[Image]`, `[Photo]`, `[Audio]` (then
`[Barcode]`, `[Signature]`), separated by an empty line; a group that does
not apply is empty text.

- General: `Filename`, `Storage Type` (`Embedded`), `MD5` (upper-case hex
  of the data), `File Size`, `Internal Size`, `External Size: 0`, `External
  Files: 0`. Text, numbers and dates: `Filename:` empty, `Storage Type:
  Text`, MD5 of the text's UTF-8 (CR as `0D`), `File Size: ?`, Internal Size
  in UTF-16 units (`😀a` → 3), no other group. Empty → `?` for everything.
- Image: `Width`, `Height`, `DPI Width`, `DPI Height`, `Transparency: 1
  (True)` / `0 (False)`; a PDF has only `Width` and `Height`; a picture that
  does not decode shows `Width: ?`, `Height: ?`, 72 dpi. Resolution is the
  server's image library's number in its own unit, rounded half up, 72 when
  none: PNG `pHYs` per meter / 100 (11811 → 118, 2835 → 28, 7850 → 79, 100
  → 1), unit 0 as stored (300 → 300), 0 → none; JFIF as stored whatever the
  unit (per cm 100 → 100), but a 1:1 density with no unit is none; EXIF
  XResolution/YResolution win over JFIF; BMP per meter / 100 (3779 → 38).
  Transparency: an alpha channel or a transparent color (PNG gray+alpha,
  RGBA even fully opaque, `tRNS`, a GIF transparent index).
- Photo (JPEG, TIFF, WebP): `Orientation: n (name)` with the server's code
  for the EXIF value: 1 → `1 (Normal)`, 2 → `4 (Flipped vertically)`, 3 →
  `3 (Rotated 180 degrees)`, 4 → `2 (Flipped horizontally)`, 5 → `5
  (Rotated 90 degrees counterclockwise and flipped vertically)`, 6 and 8 →
  `8 (Rotated 90 degrees clockwise)`, 7 → `7 (Rotated 90 degrees clockwise
  and flipped vertically)` (code 6 is `Rotated 90 degrees counterclockwise`;
  the Claris example shows it for EXIF 6 on FileMaker Pro); none or 0 → `1
  (Normal)`, 9 and more → empty. `Created` is EXIF DateTimeDigitized (not
  DateTimeOriginal), `Modified` is DateTime, shown only when later than
  Created, when Created is missing, or when one of them is `?` (both `?` →
  empty). `YYYY:MM:DD HH:MM:SS` becomes a timestamp whose date keeps the
  written separator with `:` as `/` (`1/15/2020 1:02:03 AM` in en-US,
  `15/01/2020 01:02:03` in de-DE, `2020/01/15 1:02:03` in ja-JP;
  `2021-03-04 05:06:07` → `3-4-2021 5:06:07 AM`, `04-03-2021 05:06:07` in
  German); impossible values, blanks or other text → `?`; a bare
  `2021:03:04` stays as written. `Latitude`/`Longitude` from the GPS
  rationals and references, `%.6f` with `.` in every locale (`-33.868800`,
  `0.000000`); `Make`, `Model` as stored (spaces kept).
- Audio (`mp3` with an ID3v2 tag at the start, `m4a` that parses; an MP3
  with only ID3v1 or no tag, MP3 bytes named `.m4a` and the reverse have no
  audio group): `Title`, `Artist`, `Album`, `Year`, `Track`, `Genre`,
  `Composer` from ID3v2.2-2.4 text frames or the `ilst` atoms; `Year` with a
  full date `2019-05-06` (ID3v2.4 TDRC) → `2019 (5/6/2019)` in the file's
  date format; M4A `trkn` 5 of 0 → `5`; `Cover Art: png`/`jpg`/empty;
  `Duration` h:mm:ss (MP3: Xing/Info frame count, else size at the first
  frame's rate, truncated, 1.94 s → `0:00:01`; M4A: movie header duration
  rounded half up, 1.6 s → `0:00:02`, 2.4 s → `0:00:02`); `Bit Rate: n kbps`:
  MP3 the first frame's rate, which for a constant-rate file is the Info
  frame's own (an 8 kbps file at 8 kHz shows 24, a 32 kbps file at 22.05
  kHz 56; a 64 kbps file without one 64), a `Xing` file's average truncated
  with ` (VBR)`; M4A the sample bytes over the duration, truncated.
- Single attributes are typed: numbers (`fileSize`, `internalSize`,
  `externalSize`, `width`, `height`, `dpiWidth`, `dpiHeight`,
  `transparency`, `bitRate`), timestamps (`created`, `modified`; empty when
  missing), a time (`duration`), a container (`coverArt`, named
  `Untitled.png`/`Untitled.jpg`, typed as a picture), text for the rest.
  One that does not apply is `?` (photo attributes of a PNG, `signed`,
  `width` of an MP3 or of an unreadable picture, `dpiWidth` of a PDF),
  except `barcodeText`, `barcodeType` and `externalFiles` (empty text) and
  the photo text of a JPEG without them (empty, `orientation` `1 (Normal)`).

**GetThumbnail.** Empty → empty text; text and data that is neither a
picture nor a PDF → `?`; a picture that does not decode → the container
unchanged. The box is the arguments' absolute values with fractions; 0,
empty or text → an empty result; `?` → its error and an empty result. A
picture that fits the box and needs no turning is returned unchanged
(never enlarged; `= original` is 1); otherwise the limiting side takes the
box and the other side its share, each rounded half up and at least 1 (3 x
2 in 2 x 2 → 2 x 1, in 2.4 → 2 x 2, in 2.5 → 3 x 2; 33 x 100 in 50 → 17 x
50; 640 x 480 in 101 → 101 x 76, in 1 x 1000 → 1 x 1). EXIF orientation is
applied, so a turned photo always gets an upright copy (orientation `1
(Normal)`, EXIF dropped). JPEG stays JPEG named `<stem>.jpg` (`a.JPEG` →
`a.jpg`); everything else becomes a PNG named `<stem>.png` (`A.GIF` →
`A.png`, `a.b.gif` → `a.b.png`, `.gif` → `.png`), transparency kept, 72
dpi, streams data, `SIZE`, `DPI_`. A PDF gives its first page as a PNG
(`a.pdf` in 20 x 20 → `a.png`, 14 x 20) or itself when the page fits. The
server's bytes come from its image library (PNG text chunks with dates),
so only size, name, format, resolution and transparency are comparable.

**ReadQRCode.** The first code's text (of two side by side, the left one),
from PNG, JPEG, GIF, BMP, TIFF, WebP and turned or 1-pixel-module codes;
an EAN-13 barcode is no QR code. Byte data is read as UTF-8 after a BOM,
else as the first of Big5 (leads 0xA1-0xF9, rows 0xC6A1-0xC8FE in the
private use area from U+F6B1), Shift_JIS, UTF-8, ISO 8859-1 that reads it
without error: UTF-8 `café` → `caf矇`, UTF-8 `日本` → `譌･譛ｬ`, `c4 80 62`
→ `Āb`, Latin-1 `café` → `café`, `a4 a4` → `中`, EUC-KR `c7 d1 b1 db` →
U+F77E `旋`, kanji mode → Shift_JIS; CR LF, LF, CR and tab are kept.
Errors: not container data (`"abc"`, 5) → `?` 413; data that is not a
picture (`a.dat`, PDF, MP3) → `?` 408; a picture without a readable code or
that does not decode (empty data named `a.png`) → `?` 8; `""` → `?`.

**GetTextFromPDF.** Text in content-stream order (a line drawn first comes
first even when lower on the page), a new line where the baseline moves by
more than 0.7 em (8 points at 12 stays, 9 breaks), a line's pieces in
left-to-right order, the same text repeated at the same place once, a
space where pieces leave a gap over 0.15 em (1.5 points at 12 joins, 2
separates) or overlap; runs of spaces become one (also at the ends: `"
lead and trail "`), tabs stay; lines and pages joined by CR, pages without
text skipped, no CR at the end; invisible text (OCR layers) counts, the
crop box does not clip, `/Rotate` changes nothing; WinAnsi and standard
encodings with ligatures as letters (`ﬁ` → `fi`). A PDF without text → empty
text; data not typed PDF (`a.dat`), not a PDF, text or empty → `?`.

**VerifyContainer** → `?` for every value a calculation makes (decoded
data, text, empty; `?` keeps its error), and for a container field's
embedded data too, before and after its commit (script oracle
s34-containers-setfield, 2026-10-29). Only externally stored data is
verified (OpenMaker asks the host: `CalcHost::container_data` returns the
streams, whether they are intact and whether they are stored externally,
containers.md 8; external storage is not read yet).

**GetLiveText, GetLiveTextAsJSON** → `?` 3 for any arguments (also `""`):
the server has no text recognition (Apple's Vision only).

Known deviations (OpenMaker): the converted copies in Internal Size;
thumbnail bytes; a tab drawn from a character code without a glyph name
(`(a\011b)` in WinAnsi Helvetica) is dropped by the PDF reader; HEIC
pictures are plain files (the server decodes them where libheif is
installed); `BAR#`/`BART` barcode and `SIGN` signature streams are read as
UTF-8 text / not at all (formats not decoded, containers.md 2.4).

### Oracle answers (Design functions, Get selectors and layout objects)

FileMaker Server 26.0.3.309, 2026-10-29: the script engine
(`tools/fms-script-harness`, batteries `s30`..`s32`) and the calculation
engine in file context (`tools/fms-calc-harness`, `fn-get`, `fn-design`).
Files: the Contacts (en) and Tasks (en) templates, the test project (a
Contacts copy), FileMaker Server's sample, MBS Event Management and
AppleTouchBar, and `designprobe` (Tasks with `file_metadata_probe.py` and
`design_probe.py`). The rules are in 5.8 and 6.1.

| Check | Rows | Desktop host | Server script |
|---|---:|---|---|
| Design functions (`crates/om-data/tests/data/design`) | 4,199 | all (`design_oracle`) | all (`scripts::host::design::tests`) |
| `GetLayoutObjectAttribute` (`.../layout-objects`) | 324 | all | all |
| `Get` selectors in a script session, 7 steps (`crates/om-server/tests/data/get`) | 98 x 7 | not measured (6.2) | all compared (`scripts::host::get::tests`) |

How the harness answers, for whoever probes next: expressions of a run
line are evaluated after the script with no table occurrence, where
`ValueListItems` answers empty and unqualified field names resolve
nowhere; a probe script that `Evaluate`s them answers in the window. A
run line naming no script (error 104) still opens the file, runs its
OnFirstWindowOpen script and evaluates the line, which serves files whose
account is unknown. Evaluating a value list from a field builds the
field's automatic index, after which `FieldType` says `Indexed`.

Known deviations (OpenMaker):

- Server scripts: per-account privilege metadata and effective full-access
  script answers are decoded; per-table custom access remains unmeasured.
  `PersistentID` is the
  server's id where the harness, with no machine id, gives the MD5 of
  nothing; `MultiUserState` is the unhosted harness copy's 0 [U on a
  hosting server]. Focus, `ModifiedFields`, `RequestCount` and
  `SystemStorageAvailable`: see "Oracle answers (desktop Get selectors
  and s33)".
- File context: FileMaker Server answers every Design function empty but
  `DatabaseNames` there. OpenMaker's stored-calculation recompute after a
  schema change (op73) answers them the same way (`FileHost::file_context`;
  that op73's walk is this context is inferred).
- `FieldAnnotation` and `FieldDisplayNames` answer empty, which is
  FileMaker's answer only for fields without an annotation or display
  names: OpenMaker does not read `[T, 3, 5, F, 29]`, `30` and `31`
  (file-metadata.md 1.2, whose answers come from a file FMUpgradeTool
  generated, 2026-11-11; no corpus field has either).
  `GetLayoutObjectOwnerInfo` answers the not-found document for every
  UUID, as both FileMaker hosts did for every object probed (5.8, 6.3
  O5); the case where it finds an owner is unknown.
- Desktop: see the next subsection.

### Oracle answers (desktop Get selectors and s33)

The desktop host's answers follow 6.3 (`crates/om-data/tests/desktop_get.rs`
replays O1-O6 on the host, also against the Contacts and Tasks
templates; `crates/om-gui/tests/get_selectors.rs` runs scripts in the
window). The server's last Get deviations were settled with FileMaker
Server 26.0.3's script engine, 2026-10-29 (`tools/fms-script-harness`;
fixtures in `crates/om-server/tests/data/get/s33-*`):

- **Focus (`s33-get-focus`, contacts-en; `s33-get-focus-tasks`,
  tasks-en).** New Record and Enter Find Mode put the focus in the first
  field of the layout's tab order: `Title` on Contact Details, `First
  Name` on Contact List, `Task` on Task Details and Task List, with
  selection start 1, size 0, repetition 1. `ActiveLayoutObjectName` is
  that object's name (Task Details' `Task`; Contact Details' Title is
  unnamed: empty). Set Field keeps the focus, Commit Records and Revert
  Record take it away, and so does leaving Find mode. Go to Field gives
  the focus with the insertion point after the text (`4` for three
  characters); Go to Field [ ] leaves the field and commits the record.
- **Tab order.** An object's place in it is `4/5/<layout>/5/<object>/07`,
  a big-endian number, 1 first, 0 (or absent) for objects outside it
  (Contact List: First Name 1, Last Name 2, Company 3, the Details button
  4; Contact Details starts at Title 3, positions 1 and 2 unused). The
  first field is the field object with the smallest position.
- **`ModifiedFields`.** A new record lists its auto-entered fields; then
  the fields Set Field changed and the stored calculations that depend
  on them, all in field id order (`Last Name¶Initial¶PrimaryKey¶...`,
  `First Name¶Company¶Initial`). Text inserted into the focused field is
  not listed while the field has the focus (`RecordOpenState` 2).
- **Find mode.** New Request makes `RecordOpenState` 1 (the request is
  open, `RecordOpenCount` 1); `RequestCount` keeps Find mode's count
  after Enter Browse Mode without a find (2, then 1 after a second Find
  mode with one request).
- **Extended privileges (`s33-get-privileges`).** A file generated from
  contacts-en's schema with fmapp, fmxml, fmextscriptaccess and fmrest
  also granted to [Full Access] (`SCHEMA=` in `runbat.sh`, file
  `s33-privileges.fmp12` in the harness's files) answers
  `fmapp¶fmextscriptaccess¶fmreauthenticate10¶fmrest¶fmxml` for both
  selectors: the set's keywords in alphabetical order (catalog ids 3, 7,
  4, 9, 5). An extended privilege `[23, 3, 5, E]` lists the privilege
  sets it is enabled for under `05` (`[23, 3, 5, 4, 5, 1]`), its keyword
  at `10`; a set is `[23, 2, 5, P]`, its name at `10`.
- **Accounts (`s33-account-privileges`).** A file generated from
  contacts-en's schema with accounts Entry ([Data Entry Only]), Viewer
  ([Read-Only Access]) and Prober (a custom set "Probe Set": records
  created and edited, not deleted), fmapp and fmrest granted to [Data
  Entry Only] and fmxml and fmrest to Probe Set (file `s33-accounts.fmp12`
  in the harness's files), logged in as each account:

  | Account | Privilege set (Account and Current) | Extended privileges | LayoutAccess | RecordAccess |
  |---|---|---|---|---|
  | Admin | `[Full Access]` | `fmreauthenticate10` | 2 | 2 |
  | Entry | `[Data Entry Only]` | `fmapp¶fmreauthenticate10¶fmrest` | 2 | 2 |
  | Viewer | `[Read-Only Access]` | `fmreauthenticate10` | 2 | 1 |
  | Prober | `Probe Set` | `fmrest¶fmxml` | 2 | 2 |

  `AccountType` is `FileMaker File` for each. LayoutAccess is 2 even for
  read-only sets: it is the record access through the layout, not layout
  design, and the built-in sets give records via every layout modifiable
  (session-identity.md 2.4). In the file, accounts are `[23, 1, 5, A]`
  (file-format.md 4; the template's disabled `[Guest]` is account 1): the
  name at `10`, the
  privilege set at `0b` as a key string (length, then the set member's
  segment: `01 02` is set 2; Guest `01 03`), `06` and `04` hashes, `0a`
  status (`05 90 00 41 80 00` enabled, Guest's `08 90 ...` inactive;
  decoded in local-files.md 2.2). A set's
  record privileges are `[23, 2, 5, P, 02]`: `55 03` full access and data
  entry, `54 03` without delete, `10 03` read only: 2-bit fields for
  create, view, edit and delete, from the high bits down. Layout, script
  and value list privileges are `09, 04`, `09, 11` and `09, 21` (`df`/`d7`
  full access, `de`/`d6` the others). Both are decoded in
  session-identity.md 2.

Implementation decisions (OpenMaker):

- **Server.** New Record, New Request and Enter Find Mode focus the first
  field of the tab order, Go to Field its field; insert steps without a
  field go into the focused one. The privilege selectors answer the
  session account's set in the file's security catalog (matched in any
  case): its name, its extended privileges, RecordAccess from its record
  privileges (2 with any of create, edit or delete, 1 view only, 0
  otherwise [I for 0]) and LayoutAccess 2 (every set probed; layout
  entries and per-table rules, session-identity.md 2, are not applied
  yet). The session's account is always one of the file's, whose own
  password signed it in; a script runs with full access only for an
  account in [Full Access] or when it runs with full access privileges
  (script-runtime.md 11).
  `SystemStorageAvailable` is the free space of the volume holding the
  hosted file (`statvfs`).
- **Desktop window.** The desktop area is the X11 work area
  (`_NET_WORKAREA` of the current desktop; the whole screen without one),
  the frame the window system's outer rectangle, and the content area the
  window's central panel below the status toolbar and layout bar.
  Move/Resize Window moves the frame to the given point of the desktop
  area and makes the outer frame the given size; hiding the toolbar gives
  the bars' height to the content area. `WindowUUID` is new for each New
  Window.
- **Layout object bounds on the desktop.** From the content area's top
  left; the top and bottom navigation parts are not zoomed, the other
  parts are zoomed from their own top; the footer and bottom navigation
  sit at the window's bottom [I]; title parts and sub-summaries are not
  shown in Browse mode. The window's room beyond the layout moves and
  stretches anchored objects: anchored to both edges stretches, to the
  far edge alone moves, to the near edge stays, to neither keeps the
  middle [I]; a contained object moves with its container, and a portal's
  rows do not stretch. In Form view the body grows by the room left
  below. Coordinates are rounded half away from zero. In a list the body
  objects are at the current record's row and have no portal-row bounds;
  Table view and Preview have no bounds. All four O5 states and the two
  Task List rows match on the templates.
- **`isActive`** is `1` for the focused object while the window has the
  keyboard focus [I: FileMaker Pro answered `0`, in the background].
- **`LayoutObjectUUID`** is the object's UUID inside its own layout
  calculations (hide, formatting, tooltip, placeholder, label), error 10
  elsewhere, as in a script.
- **Design functions of other files.** The host answers for any other
  open file it is given (`SessionInfo::other_files`), by name in any case
  with or without `.fmp12`, and `DatabaseNames` lists the open files in
  the order they were opened. A related file imported into the schema
  (calc-values.md 13) that is also open answers its layouts and scripts
  from the open file and the rest from the imported schema, in the
  formula's context. The desktop client opens one file per window today,
  so in the app the list is that file.
- **`ModifiedFields`, open state.** The window's unsaved fields but the
  one being typed into (typing in the window, or an insert step into the
  focused field), with the stored calculations on them (through formula
  references, transitively) and a new record's auto-entered fields: those
  that hold a value when it is created and have an auto-enter option.
  `RecordOpenState` 1 is a record New Record made that no commit kept;
  Revert Record removes it even unchanged.
- **Selection.** Set Selection sets start and size; Insert Text replaces
  the selection and leaves the insertion point after the text; without a
  selection the insertion point is after the field's text.
- **Machine and paths.** `SystemNICAddress` lists `/sys/class/net`'s
  hardware addresses in interface order (loopback left out),
  `SystemIPAddress` the kernel's local IPv4 routes (loopback left out),
  `PersistentID` the MD5 of the machine id, `UserName` Settings' User
  Name (6.1.3, [desktop-client.md](../desktop-client.md#settings)): the name
  typed under Other, else the account's full name (`OPENMAKER_USER_NAME`
  overrides both). `DesktopPath`,
  `DocumentsPath` and `PreferencesPath` (the XDG configuration folder on
  Linux) are empty when the folder does not exist; `FileMakerPath` is the
  folder holding the program, `SystemDrive` `/` on Linux,
  `CacheFilePath` the temporary folder and `CacheFileName`
  `FMTEMPFM<process id>_1.tmp` though OpenMaker keeps no cache file [I];
  `DocumentsPathListing` lists every entry under Documents, folders with
  a final `/`, at most 10,000.
- **`UUIDNumber`** is a random number below 2^192 on both hosts:
  FileMaker's answers (58 digits, the s31 values) fit there [I: their
  structure is not derived].
- **Formats.** The desktop uses the system's formats
  (`UseSystemFormatsState` 1), so the system's document is `Active` and
  the file's is not (O1); the server keeps 0. The desktop's system
  formats are en-US.
- **Hosted files** (the desktop's only kind): `MultiUserState` 2,
  `ConnectionState` 2 (encrypted; OpenMaker does not verify the host's
  certificate), `HostIPAddress` the host name's IPv4 address,
  `HostApplicationVersion` the version string of the host's Connect reply
  (`Server 26.0.2` from FileMaker Server, README F14; the Claris
  reference's example is `Server 26.0.1`) in scripts and layout
  calculations [U: FileMaker Pro's hosted answers are unmeasured].
  `FileSize` of a hosted file answers empty, and is not counted as
  parity: FileMaker Pro's hosted answer is unmeasured. It presumably asks
  the host (op 83 GetFileSize), but no capture holds that exchange and
  its request and reply are not specified (network-protocol.md 5.1). The
  FileMaker Pro probe that settles it is 6.2 O7.
- **Absent state, answered as FileMaker Pro does without it.** Preview
  draws no pages, so `PageNumber` is 1 there and 0 elsewhere,
  `PageCount` empty, and a text object with `{{PageNumber}}` shows `?`;
  there is no printing, so `PrinterName` is one space; the desktop draws
  no web viewer, so its `source` and `content` answer empty (FileMaker
  Pro: `file:///` and the page's HTML once loaded); there are no
  plug-ins and no custom menus.

The desktop now transports root-file account/set metadata as described in
[session-identity.md](session-identity.md): account and effective set names,
extended keywords in catalog member order, and aggregate record flags.
Unknown access answers empty. Layout-specific and record-conditional rules
remain undecoded; elevated script steps keep account answers separate.

Still missing on the desktop: Preview pages and printing, web viewers,
the toolbar's Quick Find text (`QuickFindText` stays empty after a Quick
Find from the toolbar; FileMaker Pro answered empty after Perform Quick
Find, and what it answers after the toolbar's is unmeasured), more than one open
file, and the Shift answer of `ActiveModifierKeys` (unmeasured; the
held keys follow the Claris reference: Shift 1, Control 4, Option or Alt
8, Command 16).

## 10. Notes for other workstreams

- **calc-encoding**: the full `kDB*Op` table (495 constants, values, source
  lines in `DBOpTbl.h`/`DBCalcEngine.h`) is regenerated by
  `tools/dbopcode_table.py`; per-arity bases and `kVarArgToSolveOffset = 300`,
  `kDBMaxCalcArgs = 6` are in `DBCalcEngine.h`. Get selectors are 1000 +
  resource-131 index, including eight selectors with no DWARF constant
  (section 2). Design and Location functions are plug-in style external
  functions registered by FMEngine, so their stored form is the external
  function form, not an opcode. `kDBAddOp` and `kDBSubOp` have no constant
  value in DWARF.
- **calc-values**: function-level observations that depend on the value
  model: `FixPt::AsLongLong` truncation (3.1); trig and Random through
  double (3.8, 3.9); `Round`/`Mod` direction (U-11); date normalization
  (`SetNormalizedDate`, `NormalizeAsDate`). The Unicode category table in
  libSupport (`sCategoryPageNN`, 83 pages) also defines value breaks and
  spaces; whoever specifies text should own it.
- **script runtime**: Get selectors are host-answered (2); `ScriptParameter`,
  `ScriptResult`, `LastError*`, `ErrorCaptureState`, `AllowAbortState`,
  `TransactionOpenState`, `RevertTransactionOnErrorState` come from the
  script runtime's state.
- **Coordinator** (shared files): architecture.md 5.3's open question "how
  the newer per-arity tables map to stored tokens" is answered for the
  opcode numbers (bases 2300/2900/3200/3500/3800/4100 = table index + base);
  catalogs/README.md should list `calc-function-spec.json` and
  `calc-function-vectors.tsv` with their generator; README "Workstream
  documents" should link this file.

Authenticated password deadlines are evaluated against the current calendar date.
A date change expires retained layout calculation results even when neither the
account deadline nor provider generation changed. Desktop imported formulas read
the owning file's account facts; private server direct, nested-field and SQL
calculations share those facts. Configured hosted accounts retain unknown facts.
