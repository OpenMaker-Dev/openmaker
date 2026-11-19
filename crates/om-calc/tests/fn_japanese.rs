//! Runs the Japanese function battery
//! (`tools/fms-calc-harness/batteries/fn-japanese.txt`, results from
//! FileMaker Server's engine in each locale's Contacts file) through the
//! function library: rows the shared reader can evaluate as text, and
//! `GetAsSVG ( expression )` rows style by style (`styles`).

mod styles;
mod support;

use om_calc::Locale;

/// Rows that differ from FileMaker, with the reason.
const KNOWN: &[(&str, &str)] = &[];

fn check(locale_file: &str, locale: Locale) {
    let path = format!(
        "{}/tests/data/fn-japanese.{locale_file}.tsv",
        env!("CARGO_MANIFEST_DIR")
    );
    let (rows, covered, mut failures) = support::check_harness_results(&path, &locale);
    let text_passed = covered - failures.len();
    let svg = styles::check_svg_rows(&path, &locale, &mut failures);
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| {
            !KNOWN
                .iter()
                .any(|(expr, _)| f.starts_with(&format!("{expr}: ")))
        })
        .collect();
    println!(
        "fn-japanese.{locale_file}: {rows} rows, {covered} covered as text ({text_passed} \
         passed); {} GetAsSVG rows, {} covered ({} styled); {} known",
        svg.rows,
        svg.covered,
        svg.styled,
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
}

#[test]
fn japanese_en() {
    check("en", Locale::en_us());
}

#[test]
fn japanese_ja() {
    check("ja", Locale::ja_jp());
}

#[test]
fn japanese_de() {
    check("de", Locale::de_de());
}
