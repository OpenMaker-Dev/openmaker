# Interoperability catalogs (FileMaker Pro 26.0.3)

Data tables extracted from the shipped binaries. Sources, record layouts,
counts, and caveats (which indexes are believed to be internal ids) are in
[../architecture.md](../architecture.md), section 5.

| File | Source |
|---|---|
| `calc-functions.csv` | DBEngine string resources 130, 134, 137-145, 156 |
| `get-selectors.csv` | DBEngine string resource 131 |
| `script-steps.csv` | DBEngine string resource 132 (index = step id) |
| `error-codes.csv` | DBEngine string resource 159 |
| `script-triggers.csv` | DBEngine string resource 129, indices 1221-1276 |
| `layout-object-types.csv` | DBEngine resource 147, FMEngine XML type table (VA `0x901f40`), class names |
| `enumerations.csv` | DBEngine string resources 133, 135, 136, 146-149, 158, 161, 162 |
| `calc-tokens.csv` | every function, operator, keyword and Get selector with its stored Polish encoding and argument counts ([calc-encoding.md](../calc-encoding.md) section 9; `tools/calc_polish.py`) |
| `calc-function-spec.json` | every calculation function and Get selector: parameters, result, compatibility, host needs, documented example expressions and results, without the Claris pages' prose ([calc-functions.md](../calc-functions.md); `tools/calc_function_spec.py`) |
| `calc-function-vectors.tsv` | function test vectors from the Claris docs and the engine; tab-separated, no quoting |
| `script-step-params.csv` | every script step's parameters: names, kinds, storage ([script-steps.md](../script-steps.md); verified by `om-scripts` example `step_params`) |
| `script-step-compat.csv` | which clients can run each step (Pro, Go, WebDirect, Server, Data API...) ([script-runtime.md](../script-runtime.md)) |
| `hbkey-constants.csv` | 229 named key constants of the file's catalogs (`HBKey` enumerators) |
| `locale-types.csv` | every file language id with its name and ISO code ([file-metadata.md](../file-metadata.md)) |
| `index-key-vectors.tsv` | value- and word-index keys of 140 text values in 74 index languages, of number, time, timestamp and date values, and every kana's Japanese weights, from a file FileMaker Server's engine wrote (`tools/fms-script-harness`; [query-program.md](../query-program.md) 7.1-7.3) |
| `execute-sql-vectors.tsv` | 1,633 ExecuteSQL/ExecuteSQLe results from FileMaker Server's engine over 10 sample files ([execute-sql.md](../execute-sql.md); `tools/fms-sql`); `depends=` flags clock- and session-dependent rows |
| `execute-sql-errors.tsv` | the 56 FQL error messages and 176 engine error texts ExecuteSQLe reports |
| `password-hash-decompose.tsv` | `Unicode::HashDecompose`'s table, measured by calling FileMaker Pro 26.0.3's own Support library on non-surrogate BMP code units ([network-protocol.md](../network-protocol.md) 4.4): each code unit that decomposes, with its full decomposition, and each combining mark (`mark` 1: a leading one gets a space). Code units not listed stay as they are |
| `password-hash-vectors.tsv` | 50 `ComputePasswordHash` vectors from the same library: text and its decomposition as UTF-16 code units (`-` empty), salt, iterations, selector, output length, hash. Inputs are synthetic and non-secret; every row uses the fixed 30-byte pepper of [network-protocol.md](../network-protocol.md) 4.3 |
| `calc-value-vectors.tsv` | 2,196 value and operator vectors from FileMaker Server's engine ([calc-values.md](../calc-values.md) section 11; `tools/fms-calc-harness`); escapes `\r \n \t \\` in both expression and result. Evidence `file=` names the template's locale; rows with `context=`, `record=` or `rep=` ran in a table occurrence on a record (`schema:` rows describe the schema in words) and need an evaluator with fields |

Regenerate (Mac x86_64 slice of DBEngine, SHA-256 `b8bf96a634ddb7833c8ac68f4cc7f3203bc41c43684f7feeebda4c4770958fb1`):

```
python3 -I tools/dbengine_strres.py re/work/thin/DBEngine 2556500 re/work/arch/dbengine-strres.tsv
python3 -I tools/make_catalogs.py re/work/arch/dbengine-strres.tsv docs/re/catalogs
```

The raw dump of all 37 string resources (`re/work/arch/dbengine-strres.tsv`)
stays in the git-ignored `re/work/`: the catalogs above keep only what
OpenMaker needs.

`layout-object-types.csv` is assembled by hand from the evidence listed in it.
