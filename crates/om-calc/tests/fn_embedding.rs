//! The embedding vector functions against FileMaker Server's engine: the
//! battery `tools/fms-calc-harness/batteries/fn-embedding.txt` run in the
//! English and German Contacts templates (`tests/data/fn-embedding.*.tsv`).
//! Every row the shared reader can evaluate must match the result text and
//! error code, except the entries of `KNOWN`.

mod support;

use om_calc::Locale;

/// Rows that are expected to differ, with the reason.
const KNOWN: &[(&str, &str)] = &[];

fn check(file: &str, locale: Locale) {
    let path = format!("{}/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
    let (rows, covered, failures) = support::check_harness_results(&path, &locale);
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| !KNOWN.iter().any(|(e, _)| f.starts_with(&format!("{e}: "))))
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
    check("fn-embedding.en.tsv", Locale::en_us());
}

#[test]
fn german() {
    check("fn-embedding.de.tsv", Locale::de_de());
}
