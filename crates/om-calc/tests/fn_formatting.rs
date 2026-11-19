//! Runs the text formatting battery
//! (`tools/fms-calc-harness/batteries/fn-formatting.txt`, results from
//! FileMaker Server's engine) through the function library.
//!
//! Rows the shared reader can evaluate are checked as text, and
//! `GetAsSVG ( expression )` rows style by style (`styles`).

mod styles;
mod support;

use om_calc::Locale;

/// Rows that differ from FileMaker, with the reason.
const KNOWN: &[(&str, &str)] = &[(
    "GetAsSVG ( TextFont ( \"ab\" ; \"ARIAL\" ) )",
    "FileMaker keeps one spelling per font name, ignoring case and width, for \
         the whole session (the battery used \"Arial\" first); om-calc keeps the \
         name as given",
)];

#[test]
fn formatting_en() {
    let locale = Locale::en_us();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/fn-formatting.en.tsv"
    );
    let (rows, covered, mut failures) = support::check_harness_results(path, &locale);
    let text_passed = covered - failures.len();
    let svg = styles::check_svg_rows(path, &locale, &mut failures);
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| {
            !KNOWN
                .iter()
                .any(|(expr, _)| f.starts_with(&format!("{expr}: ")))
        })
        .collect();
    println!(
        "fn-formatting.en: {rows} rows, {covered} covered as text ({text_passed} passed); \
         {} GetAsSVG rows, {} covered ({} styled); {} known",
        svg.rows,
        svg.covered,
        svg.styled,
        failures.len() - unexpected.len()
    );
    assert!(
        svg.styled > 250,
        "too few styled GetAsSVG rows: {}",
        svg.styled
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
}
