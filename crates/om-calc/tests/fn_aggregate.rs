//! Runs the aggregate function fixtures `tests/data/fn-aggregate.<locale>.tsv`
//! (FileMaker Server 26.0.3.309's engine through `tools/fms-calc-harness`:
//! battery `fn-aggregate.txt` in en-US, `fn-aggregate-intl.txt` in de-DE,
//! fr-FR, sv-SE and ja-JP) through the library with the shared literal
//! reader, which passes each argument as a one-value list. Every row the
//! reader can express must match FileMaker's result text and error code,
//! except the entries of `KNOWN`. A single literal argument is the parser's
//! error 1214, which the reader emulates; lists of many values from related
//! records are unit tests in `functions/aggregate.rs`.

mod support;

/// Rows FileMaker computes differently from the library: none since the
/// square root follows calc-values.md 14 and collation the engine's.
const KNOWN: &[(&str, &str)] = &[];

#[test]
fn fixtures() {
    support::check_fixture_set("fn-aggregate", KNOWN);
}
