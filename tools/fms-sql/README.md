# fms-sql: FileMaker's SQL engine (FQL) as a black-box oracle

Research tools for `docs/re/execute-sql.md`. Two parts:

1. **Probe batteries** for `ExecuteSQL` / `ExecuteSQLe`, run by the
   calculation harness (`tools/fms-calc-harness`, FileMaker Server
   26.0.3.309's own `libDBEngine.so` in a network-less amd64 container), and
   the scripts that turn their results into catalogs.
2. **`fqlgrammar`**, a small program for the same container that loads
   `libDBEngine.so` and the ANTLR 4.13.1 runtime FileMaker ships
   (`lib/libantlr4-runtime.so.4.13.1`) and calls exported accessors of
   FileMaker's generated SQL recognizers. No FileMaker code is copied: it
   only declares exported entry points (names and ABI from the dynamic
   symbol table).

## Batteries

Sources are `batteries/*.sql`; `make_battery.py` turns each into the
calceval battery `batteries/*.txt` (one calculation per line):

```
# ...              section marker, copied
@ <ctx> [<id>]     calceval record context line, copied
= <calculation>    copied as is
<sql>              ExecuteSQLe ( "<sql>" ; "|" ; "¶" )
<sql> ;; a ;; b    the same with arguments a, b (calculation text) for ? parameters
E <sql>            ExecuteSQL instead of ExecuteSQLe
%sep <f> <r>       field and row separators (FileMaker text literals) from here on
```

The SQL is written as FileMaker should receive it; `make_battery.py` escapes
`"` and `\` for the text literal. calceval's own escapes (`\n \r \t \uXXXX`)
are decoded before FileMaker sees the formula, so a CR written `\r` becomes a
CR *in the formula text*, which the calculation parser turns into a space
(calc-values.md 12.1); write `¶` for a CR in the SQL text.

```sh
export FMS_ROOT=.../re/extracted/fms/deb/opt/FileMaker FILES=.../re/work/fms-harness/files
tools/fms-sql/runq.sh q04-predicates fms-sample-en.fmp12
tools/fms-sql/view.sh re/work/fms-sql/q04-predicates.fms-sample-en.tsv
```

`runq.sh <name> <file> [context] [results dir]` writes to the git-ignored
`re/work/fms-sql/` by default. Inspect a new result before copying it into
`tools/fms-sql/results/` as a fixture. Build the catalogs from the checked-in
fixtures:

```sh
python3 -I tools/fms-sql/make_sql_vectors.py tools/fms-sql/results docs/re/catalogs/execute-sql-vectors.tsv
python3 -I tools/fms-sql/make_error_catalog.py tools/fms-sql/results/errors-dump.txt \
    docs/re/catalogs/execute-sql-vectors.tsv docs/re/catalogs/execute-sql-errors.tsv
```

`runq.sh` takes the harness lock through `runbat.sh` (one container at a
time). Files (copies in `$FILES`; the MBS examples are from the MBS
FileMaker plug-in 16.3 example folder, `re/work/calc-encoding/corpus-mbs`):

| file in `$FILES` | source | used by |
|---|---|---|
| `fms-sample-en.fmp12` | FileMaker Server `en_FMServer_Sample.fmp12` | q01-q05, q11, q12, q14-q16 |
| `contacts-{en,de,sv,ja}.fmp12` | FileMaker Pro template Contacts | q13, q13b, q13c |
| `mbs-audit.fmp12` | `Examples/Audit/Audit.fmp12` | q06 |
| `mbs-email.fmp12` | `Examples/CURL/Email/Build and send Email.fmp12` | q07 |
| `mbs-atb.fmp12` | `Examples/Third Party/Apple TouchBar/AppleTouchBar.fmp12` | q08 |
| `mbs-swissqr.fmp12` | `Examples/Barcode/Swiss QR-Code for invoices/...ISO 20022.fmp12` | q09 |
| `mbs-events.fmp12` | `Examples/Audit/Event Management with Audit/Event Management with Audit.fmp12` | q10 |
| `sumprobe.fmp12` | the run copy of `tools/fms-script-harness` s16 (`findprobe` with the six probe records of file-metadata.md 4.3) | q19, q20 |

| battery | content |
|---|---|
| `q01-lexical` | keywords, identifiers, quoting, comments, white space, literals, syntax errors and their positions |
| `q02-output` | separators, text form of each type, CR in SQL text, `SELECT *`, `ROWID`/`ROWMODID` |
| `q03-functions`, `q03b-functions` | scalar functions, `CAST`, `CASE`, `COALESCE`, `NULLIF` (q03b: one item per probe) |
| `q04-predicates` | comparison, case, `LIKE`, `IN`, `BETWEEN`, `NULL`, quantified, `EXISTS`, type mixing |
| `q05-query` | aggregates, `GROUP BY`/`HAVING`, `DISTINCT`, `ORDER BY`, `OFFSET`/`FETCH`, `UNION`, joins, subqueries |
| `q06-audit` | date, time, timestamp, number and empty container fields |
| `q07-containers` | container fields with data, `GETAS`, globals |
| `q08-repetitions` | repeating fields and `field[n]` |
| `q09-summary` | a summary field, global text |
| `q10-external` | an occurrence of a file that is not open; calculated fields |
| `q11-parameters` | `?` parameters from arguments of every type; named parameters |
| `q12-statements` | `INSERT`, `UPDATE`, `DELETE`, DDL, cursors through `ExecuteSQL` |
| `q13-collation` | text order and comparison in en, de, sv, ja files; locale-dependent results |
| `q13b-keys`, `q13c-keys` | the sort and duplicate key of text values |
| `q14-context` | context independence, `ExecuteSQL` against `ExecuteSQLe`, system tables |
| `q15-numbers`, `q15b`, `q15c` | number text form, exponent notation |
| `q16-calc-compare` | SQL functions beside the calculation functions of the same name |
| `q19-summary-data` | summary fields over data: every kind per row, with and without `WHERE`, `ORDER BY`, joins, unstored calculations |
| `q20-summary-found-sets` | the found set a summary column sees: joins, `FETCH`/`OFFSET`, `OR`/`NOT`, `DISTINCT`, subqueries |

Checked-in results (`results/<battery>.<file>.tsv`) are calceval's output:
expression, error (`EvaluationError`), cached/native/calc types, and result
text with escapes `\r \n \t \\ \uXXXX`.

## fqlgrammar

```sh
tools/fms-sql/run.sh atn > grammar-dump.txt       # rule names, vocabulary, serialized ATNs, FunctionTable
python3 -I tools/fms-sql/atn_grammar.py grammar-dump.txt FQLSQLSemanticAnalyzer FQLSQLLexer > grammar.txt
tools/fms-sql/run.sh tree < statements.txt          # tokens and parse tree of each line
tools/fms-sql/run.sh errors > errors-dump.txt     # FQLnnnn texts and engine error texts
tools/fms-sql/run.sh keys 2 < strings.txt         # International::MakeKey of Locale::Type 2 (Unicode raw)
tools/fms-sql/run.sh locale                       # FQL::FQLLocale and the International it holds
```

- `atn`: constructs `FQLSQLLexer`, `FQLSQLParser` and `FQLSQLSemanticAnalyzer`
  the way `FQL::FQLParserDriver::CreateTokenStream` does (ANTLRInputStream,
  lexer object 0x110 bytes with its `TokenSource` base at +0x78,
  CommonTokenStream) and prints `getRuleNames()`, the vocabulary
  (`Vocabulary::getLiteralName`/`getSymbolicName` up to the ATN's maximum
  token type) and `getSerializedATN()` of each, then the 20 entries of the
  global `FunctionTable` (name, function code).
- `atn_grammar.py` deserializes the ATNs (format version 4, as the public
  ANTLR 4 runtime's `ATNDeserializer` reads it) and walks each rule's states
  back into EBNF. `results/fql-grammar.txt` is its output for the lexer and
  the semantic analyzer (the grammar ExecuteSQL uses; `FQLSQLParser` is only
  constructed by its own delegating constructor and is dead code).
- `tree`: lexes each line and parses it with
  `FQLSQLSemanticAnalyzer::statement()`, the rule
  `FQLParserDriver::ParseQuery` calls; prints the tokens
  (`CommonToken::toString`), the syntax error count and
  `Trees::toStringTree`. ANTLR's console listener prints the syntax errors.
- `errors`: `FQL::NonLocalizedStrings(n)` for n = 0..56 and
  `FQL::ErrorToString(code)` for -1..40000; `results/errors-dump.txt` keeps
  the FQL lines and the codes whose text is not the default
  `FileMaker error`.
- `keys`, `locale`: used to rule out `International::MakeKey` of the Unicode
  raw collation as the FQL sort key (it is plain UTF-16BE) and to identify
  `FQL::FQLLocale`'s collation (Unicode raw).

`run.sh` uses the `om-fms-calc-harness` image and takes the same machine-wide
lock as `runbat.sh`.
