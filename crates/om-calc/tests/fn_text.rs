//! The text functions against FileMaker Server's engine: the battery
//! `tools/fms-calc-harness/batteries/fn-text.txt` run in the Contacts
//! template of each locale (`tests/data/fn-text.<locale>.tsv`).

mod support;

use om_calc::Locale;

/// Rows that are expected to differ, with the reason.
const KNOWN: &[(&str, &str)] = &[];

fn check(file: &str, locale: Locale) {
    let path = format!("{}/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
    let (rows, covered, failures) = support::check_harness_results(&path, &locale);
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| {
            !KNOWN
                .iter()
                .any(|(expr, _)| f.starts_with(&format!("{expr}: ")))
        })
        .collect();
    println!(
        "{file}: {rows} rows, {covered} covered, {} passed, {} known",
        covered - failures.len(),
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(
        unexpected.is_empty(),
        "{file}: {} rows failed",
        unexpected.len()
    );
}

#[test]
fn english() {
    check("fn-text.en.tsv", Locale::en_us());
}

#[test]
fn german() {
    check("fn-text.de.tsv", Locale::de_de());
}

#[test]
fn french() {
    check("fn-text.fr.tsv", Locale::fr_fr());
}

#[test]
fn swedish() {
    check("fn-text.sv.tsv", Locale::sv_se());
}

#[test]
fn japanese() {
    check("fn-text.ja.tsv", Locale::ja_jp());
}
