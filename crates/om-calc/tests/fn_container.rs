//! The container encoding functions against FileMaker Server's engine: the
//! battery `tools/fms-calc-harness/batteries/fn-container.txt` run in the
//! English Contacts template (`tests/data/fn-container.en.tsv`). None of
//! these functions depends on the locale beyond the text form of numbers
//! and dates, which the value core's own fixtures cover.

mod support;

use om_calc::Locale;

/// Characters a target encoding lacks: the engine transliterates them
/// (`€` as `EUR`, `é` as `´e` or `'e`, `①` as `(1)`, `Ω` as `Ohm`, `—` as
/// `-`); OpenMaker writes their compatibility form when that encodes and
/// otherwise leaves them out.
const TRANSLIT: &str = "the engine's transliteration table is not reproduced";

/// Rows that are expected to differ, with the reason.
const KNOWN: &[(&str, &str)] = &[
    ("HexEncode ( TextEncode ( \"aé\" ; \"windows-1251\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( \"aé\" ; \"shift_jis\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( \"aé\" ; \"euc-kr\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( \"aé\" ; \"big5\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( \"€\" ; \"iso-8859-1\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 8212 ) ; \"shift_jis\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 9312 ) ; \"shift_jis\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 128 ) & Char ( 159 ) & Char ( 164 ) & Char ( 8364 ) & Char ( 1046 ) & Char ( 8486 ) ; \"iso-8859-1\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 128 ) & Char ( 159 ) & Char ( 164 ) & Char ( 8364 ) & Char ( 1046 ) & Char ( 8486 ) ; \"windows-1252\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 128 ) & Char ( 159 ) & Char ( 164 ) & Char ( 8364 ) & Char ( 1046 ) & Char ( 8486 ) ; \"windows-1251\" ; 1 ) )", TRANSLIT),
    ("HexEncode ( TextEncode ( Char ( 128 ) & Char ( 159 ) & Char ( 164 ) & Char ( 8364 ) & Char ( 1046 ) & Char ( 8486 ) ; \"macintosh\" ; 1 ) )", TRANSLIT),
    (
        "HexEncode ( TextDecode ( HexDecode ( \"\" ; \"a\" ) ; \"utf-8\" ) )",
        "error 2 (TextDecode of empty container data) ends the whole engine evaluation; a library function can only make its own result ?",
    ),
];

#[test]
fn english() {
    let path = format!(
        "{}/tests/data/fn-container.en.tsv",
        env!("CARGO_MANIFEST_DIR")
    );
    let (rows, covered, failures) = support::check_harness_results(&path, &Locale::en_us());
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| !KNOWN.iter().any(|(e, _)| f.starts_with(&format!("{e}: "))))
        .collect();
    println!(
        "fn-container.en: {rows} rows, {covered} covered, {} passed, {} known",
        covered - failures.len(),
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
    for (expr, _) in KNOWN {
        assert!(
            failures.iter().any(|f| f.starts_with(&format!("{expr}: "))),
            "{expr} now passes or is gone: remove it from KNOWN"
        );
    }
}

/// The locale-sensitive paths (number and date text, decimal separators)
/// in the five Contacts locales: `fn-container-intl.txt`.
#[test]
fn locales() {
    support::check_fixture_set("fn-container-intl", &[]);
}
