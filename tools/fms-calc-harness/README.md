# fms-calc-harness: FileMaker Server's calc engine as a black-box oracle

A research tool for `docs/re/calc-values.md` and `docs/re/calc-encoding.md`.
It loads FileMaker Server 26.0.3.309's own `libDBEngine.so` and
`libSupport.so` (Ubuntu build) in a network-less amd64 Docker container,
opens a copy of an `.fmp12` file and either evaluates calculation
expressions with `Draco::DBExpressionEnv::Evaluate` (the engine entry that
parses formula text and solves it) or compiles them to the bytes FileMaker
stores. Observations it produces are runs of the real engine, like the
FileMaker Pro oracle, not code reading. No FileMaker code is copied:
`calceval.cpp` only declares the exported entry points it calls (names and
ABI from the symbol table and DWARF) and mirrors FMDeveloperTool's start-up
sequence.

## Run

```sh
docker build --platform linux/amd64 -t om-fms-calc-harness tools/fms-calc-harness
export FMS_ROOT=/path/to/re/extracted/fms/deb/opt/FileMaker   # unpacked .deb
export FILES=/path/to/scratch/files                          # .fmp12 copies
cp ".../DynamicTemplates/Contacts/en.fmp12" "$FILES/contacts-en.fmp12"
tools/fms-calc-harness/runbat.sh tools/fms-calc-harness/batteries/01-numbers.txt contacts-en.fmp12 /tmp/results
python3 -I tools/fms-calc-harness/make_vectors.py /tmp/results docs/re/catalogs/calc-value-vectors.tsv \
    tools/fms-calc-harness/schema-vectors.tsv
```

`runbat.sh [-c] [-d] <battery> <file> [results dir] [context]` runs one
battery; `-c` selects compile mode, `-d` registers the design functions,
`context` is the table occurrence the run starts in (stored form, below).
The image needs network access once (Ubuntu packages); to pull the base
image through a registry mirror, add `--build-arg
BASE_IMAGE=<mirror>/library/ubuntu:24.04` to `docker build`. Runs use
`--network none` and mount FileMaker's libraries and the files read-only. The container
works on a private copy of the file in `/tmp` and nothing is written back.
Under Rosetta/QEMU a battery of 200 expressions takes about 10 seconds, most
of it start-up. Run one container at a time on a machine with little memory:
the Docker VM kills parallel runs (exit 137).

## Input

One expression per line; `#` lines are copied through as section markers.
Escapes `\n`, `\r`, `\t`, `\\` and `\uXXXX` are decoded; any other backslash
is kept, so FileMaker's own `\"` inside text literals works (write `\\` for
a backslash FileMaker should see).

Table occurrences are written as the formula's stored context (child 4 in
calc-encoding.md 1): `03d00001` for occurrence `d0 00 01`, `0100` for
occurrence 0. Their ids are in `dump_calcs`' `to` rows.

## Evaluate (default)

Output: `expression <TAB> error <TAB> cached/native/calcNative type <TAB>
result text`. `error` is the `Error::Code` returned by `Evaluate`, the code
`EvaluationError()` reports for the same expression, except 7 (a `While` or
custom function call limit), which aborts the whole calculation. The result
text uses the input escapes, plus `\uXXXX` for controls, private-use
characters and lone surrogates (a surrogate pair prints as its character).
Prefixes:

- `!N expr` converts the result with `DBData::ConvertData(N)` (1 text,
  2 number, 3 date, 4 time, 5 timestamp), what a stored calculation's
  declared result type does to the final value (calc-values.md 6.5).
- `~ expr` appends the result text's style runs (`Text::DumpRunsToXML`).
  In practice it prints no runs for any result (observed 2026-10-28);
  check styles through `GetAsSVG ( expr )` instead, which renders each
  run as CSS.

Record context (calc-values.md 12.1):

- `@ <context> <record ID>` evaluates the following lines in that table
  occurrence on that record, through `DBRecordCursor(DBFileUser const&,
  HBSerialKey<5> const&, HBSerialKey<9> const&)` and
  `DBExpressionEnv::Setup`; the record key is one segment holding the ID.
  Without an ID the occurrence is set but there is no record (field
  references are then error 1224). Record IDs come from `ExecuteSQL (
  "SELECT ROWID FROM T" ; "" ; "," )`; `FileMaker_Tables` and
  `FileMaker_Fields` describe the schema.
- `@rep <n>` sets `DBExpressionEnv::mRepetition`, the repetition of a
  repeating calculation being computed (`Get ( CalculationRepetitionNumber
  )`); 0 is "no repetition".

## Compile (`-c`)

Each line is compiled the way `DBCalcFormula::Set` does it, minus the
database writes: `DBCalcParse::DoParse` with the file's locale and table
occurrence catalog, `Optimize(1)` and `WriteAsPolish` of the result
(DBCalc vtable slots 15 and 16), then `DBCalcScan::Tokenize` of the same
text. Output: `expression <TAB> error <TAB> begin:end <TAB> context <TAB>
formatted <TAB> polish`, the streams in hex as stored in children 5 and 6;
`begin:end` is the error's UTF-16 span (`DBScanError::Retrieve`). Prefixes:
`@<context> ` compiles that line in another table occurrence, `%N ` passes
`DBCalcFormula::CalcOptions` N (16 `kSupportSelf` for `Self`; 1, 2, 4
auto-quoting). `decode_results.py <results> [names.tsv]` prints the trees
and checks them against `tools/calc_polish.py`'s compiler.

## Round trip (`-r`) and `roundtrip.py`

Input lines `id <TAB> context <TAB> formatted <TAB> polish` (hex, from
`dump_calcs`): the formatted stream is displayed with
`DBCalcScan::UnTokenize`, compiled again (retrying with `kSupportSelf` after
error 1225) and compared. `roundtrip.py <calcs.tsv> <file dir> <out dir>
[jobs]` runs a whole `dump_calcs` corpus, one container per file.

## Design functions (`-d`)

libDBEngine alone does not know the design functions (`FieldNames`,
`LayoutNames`, ...). `-d` dlopens libFMEngine.so and calls its exported
`Draco::InstallDesignFunction()`, which registers them as built-in plug-in
functions (`FMDR`). The mobile group (`FMMB`: `Location`, ...) is registered
by nothing in the server libraries.

## Batteries and results

| battery | mode, file | content |
|---|---|---|
| `01`..`15` | eval, Contacts en/de/fr/sv/ja | values, conversions, errors (calc-values.md) |
| `16-literals` | eval, contacts-en | text literal values |
| `c01-needed` | `-c -d`, calc-oracle, `03d00001` | the needed-expressions of calc-encoding.md section 10 plus error spans |
| `c02-unused` | `-c -d`, calc-oracle, `03d00001` | one call per function, constant and Get selector never seen in a file (`make_unused_battery.py`) |
| `c03-custom` | `-c`, MBS FileMaker_Snippet_Storage, `03d00001` | custom function calls |
| `c04-summary` | `-c`, MBS Swiss QR-Code ISO 20022, `03d00001` | `GetSummary` with a real summary field |
| `c05-locale` | `-c`, contacts-de | decimal-comma literals |
| `x01`..`x04` | eval with `@` lines, MBS AppleTouchBar, MBS Phidget Digital Outputs, FileMaker Server sample en, MBS FileMaker_Snippet_Storage | repetitions, empty number fields, related records, custom function limits |
| `e01`..`e09` | eval (`e02`..`e05`, `e07`, `e09` with `@` lines), Contacts en, FileMaker Server sample en, MBS FileMaker_Snippet_Storage, MBS AppleTouchBar; `e08` with `-d` | evaluator probes: Let and While slots, variables, `Evaluate`, `IsValid`, `GetField`, field operands, limits, argument order (calc-values.md 13, evaluator answers) |
| `c06-commas` | `-c`, contacts-en | `,` between arguments in a decimal-point file |
| `t01`..`t09`, `t11`..`t13` | eval, contacts-en | transcendental functions, powers, financial functions, trigonometry and arithmetic probes at p = 16..400 (calc-values.md 14), generated by `tools/fixpt_battery.py gen` |
| `t10-npv` | eval with `@` lines, FileMaker Server sample en | `NPV` over the 12 related values |
| `vc-cjk-order` | eval, Contacts zh, ko (ja, en) | FileMaker's order of every BMP character, the input of `cjk_tables.py` (om-calc's Chinese and Korean tables) |
| `fn-get` | eval, contacts-en | every Get selector in file context, without and with a table occurrence (calc-functions.md 6) |
| `fn-design` | eval `-d`, contacts-en | the Design functions in file context: all empty but `DatabaseNames` (calc-functions.md 5.8) |
| `s14-probes` | eval, contacts-en | guard digits after `Int`, `Floor`, `SetPrecision`..., operand limits of `*` and `/`, `Mod` and `Div` with wide divisors (calc-values.md 14, implementation notes) |

The `t` batteries are checked against the executable model of
calc-values.md 14 with `python3 -I tools/fixpt_battery.py check
results/t*.tsv --libm results/t11-t12-glibc.txt` (`results/t-summary.txt`
is its last output; `results/t-known.tsv` holds the known deviations of
`om-calc`'s tests). The trigonometric results need the C library of the
image: `python3 -I tools/fixpt_battery.py libm-calls results/t11-*.tsv
results/t12-*.tsv` lists the doubles the model passes to it, and

```sh
docker run -i --rm --platform linux/amd64 --network none -v "$PWD/tools/fms-calc-harness":/h:ro \
    om-fms-calc-harness sh -c 'gcc -O0 -o /tmp/libm /h/libm.c -lm && /tmp/libm' < calls.txt
```

prints glibc 2.39's results for them (take the run lock around it).

`results/` holds the outputs of the `16`, `c`, `e`, `x`, `t` and `s`
batteries (`crates/om-calc/tests/eval_vectors.rs` runs the evaluation ones,
`tests/fn_transcend.rs` the `t` ones once more), decoded
compile results, and `roundtrip.txt` (the engine round trip of the template
corpus). The sample files: `calc-oracle.fmp12` is the FileMaker Pro oracle's
file (`tools/calc-oracle`), the MBS examples are from the MBS FileMaker
plug-in 16.3 example folder, the server sample is `en_FMServer_Sample.fmp12`
from the FileMaker Server package.

## Context and limits

- Without `@` lines the context is the opened file with no table occurrence,
  no record, no layout or window, at "script level 0" of a server-type
  session (`Get(ApplicationVersion)` reports FMDeveloperTool). Field names
  then resolve as missing tables (error 106), and `$`/`$$` variables
  persist from line to line within one run.
- `Self` cannot be evaluated: `Evaluate` parses without `kSupportSelf` (1225).
- Text formats, decimal separators, collation and day names come from the
  locale stored in the file (no "system formats" exist on the server). The
  shipped Contacts templates give en-US, de-DE, fr-FR, sv-SE and ja-JP.
- Creating schema, records or custom functions through the engine is not
  implemented; batteries use sample files that already have them.
- gdb cannot attach under emulation; a crash prints a backtrace to stderr.
  The harness leaves with `_exit` because a libSupport timer thread can
  crash during static destructors.

`runbat.sh` takes a machine-wide lock (`$HARNESS_LOCK`, default
`.run.lock` next to the `$FILES` directory) so concurrent callers queue instead of
running containers side by side, which gets them killed for memory (exit
137, truncated result files). A run that is killed leaves the lock
directory behind; remove it by hand if no harness container is running.
