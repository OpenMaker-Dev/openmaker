//! Runs the number function fixtures `tests/data/fn-number.<locale>.tsv`
//! (FileMaker Server 26.0.3.309's engine through `tools/fms-calc-harness`:
//! battery `fn-number.txt` in en-US, `fn-number-intl.txt` in de-DE, fr-FR,
//! sv-SE and ja-JP) through the library with the shared literal reader.
//! Every row the reader can express must match FileMaker's result text and
//! error code, except the entries of `KNOWN`.

mod support;

/// Rows FileMaker computes differently from the library: none since the
/// decimal functions follow calc-values.md 14.
const KNOWN: &[(&str, &str)] = &[];

#[test]
fn fixtures() {
    support::check_fixture_set("fn-number", KNOWN);
}
