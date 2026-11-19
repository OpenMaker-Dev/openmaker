//! Runs the financial function fixtures `tests/data/fn-financial.<locale>.tsv`
//! (FileMaker Server 26.0.3.309's engine through `tools/fms-calc-harness`:
//! battery `fn-financial.txt` in en-US, `fn-financial-intl.txt` in de-DE,
//! fr-FR, sv-SE and ja-JP) through the library with the shared literal
//! reader. Every row the reader can express must match FileMaker's result
//! text and error code, except the entries of `KNOWN`. `NPV` with literal
//! payments is the parser's error 1214, which the reader emulates; its
//! values over related records are unit tests in `number/finance.rs`.

mod support;

/// Rows FileMaker computes differently from the library: none since the
/// annuity functions follow calc-values.md 14.6.
const KNOWN: &[(&str, &str)] = &[];

#[test]
fn fixtures() {
    support::check_fixture_set("fn-financial", KNOWN);
}
