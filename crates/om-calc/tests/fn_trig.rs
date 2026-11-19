//! Runs the trigonometric function fixtures `tests/data/fn-trig.<locale>.tsv`
//! (FileMaker Server 26.0.3.309's engine through `tools/fms-calc-harness`:
//! battery `fn-trig.txt` in en-US, `fn-trig-intl.txt` in de-DE, fr-FR, sv-SE
//! and ja-JP) through the library with the shared literal reader. Every row
//! the reader can express must match FileMaker's result text and error code,
//! except the entries of `KNOWN`.

mod support;

/// FileMaker Server's glibc 2.39 is not correctly rounded near a rounding
/// tie, and the library is (calc-values.md 14.8).
const KNOWN: &[(&str, &str)] = &[(
    "Sin(1529)",
    "glibc rounds sin(0x1.17cde069263eap+1), 1.8e-5 units below a tie, up",
)];

#[test]
fn fixtures() {
    support::check_fixture_set("fn-trig", KNOWN);
}
