//! Runs the date, time and timestamp function battery
//! (`tools/fms-calc-harness/batteries/fn-datetime.txt`, results from
//! FileMaker Server's engine in each locale's Contacts file) through the
//! function library.

mod support;

use om_calc::Locale;

/// Rows that differ from FileMaker, with the reason.
const KNOWN: &[(&str, &str)] = &[
    (
        "Date ( 25 ; 1 ; -1 )",
        "a negative year whose rolled-over date is in range: FileMaker keeps the date \
         (GetAsNumber gives 1) but displays it as ?; a Date has no undisplayable state",
    ),
    ("Date ( 1 ; 800 ; -1 )", "as Date ( 25 ; 1 ; -1 )"),
    (
        "Timestamp ( \"1/2/0000\" ; 0 )",
        "date text with year 0 read field by field: FileMaker returns a timestamp \
         displayed as its time alone; om-calc reports error 500",
    ),
];

fn check(locale_file: &str, locale: Locale) {
    let path = format!(
        "{}/tests/data/fn-datetime.{locale_file}.tsv",
        env!("CARGO_MANIFEST_DIR")
    );
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
        "fn-datetime.{locale_file}: {rows} rows, {covered} covered, {} passed, {} known",
        covered - failures.len(),
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
}

#[test]
fn datetime_en() {
    check("en", Locale::en_us());
}

#[test]
fn datetime_de() {
    check("de", Locale::de_de());
}

#[test]
fn datetime_fr() {
    check("fr", Locale::fr_fr());
}

#[test]
fn datetime_sv() {
    check("sv", Locale::sv_se());
}

#[test]
fn datetime_ja() {
    check("ja", Locale::ja_jp());
}
