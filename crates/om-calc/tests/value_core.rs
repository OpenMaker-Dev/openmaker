//! The value-core follow-up batteries
//! (`tools/fms-calc-harness/batteries/vc-*.txt`, results from FileMaker
//! Server's engine in `tests/data/vc-*.tsv`; calc-values.md 13, value-core
//! answers): style run boundaries, text to number, error -1 and text
//! collation. Every row the shared reader can evaluate must match the
//! result text and error code, except the known rows listed per fixture.

mod styles;
mod support;

use om_calc::Locale;

/// A known difference: the rows of fixtures whose name starts with the
/// first field and whose expression contains every one of the second
/// (`KANA`: any kana), with the reason.
type Known = (&'static str, &'static [&'static str], &'static str);

/// Stands for any kana, bopomofo or squared katakana word.
const KANA: &str = "<kana>";

const KNOWN: &[Known] = &[
    (
        "vc-styles",
        &["TextFontRemove ( TextFont ( \"a\" ; \"Arial\" ) & TextFont ( \"b\" ; \"arial\" ) ; \"ARIAL\" )"],
        "FileMaker keeps one spelling per font name for the session, so the two \
         runs are one Arial run before the removal; om-calc keeps names as given",
    ),
    (
        "vc-styles",
        &["Replace ( TextColor ( \"a\" ; 255 ) & \"b\" & TextColor ( \"c\" ; 255 ) ; 2 ; 1 ; \"Y\" & TextColor ( \"X\" ; 255 ) )"],
        "FileMaker leaves the c after the replaced run unstyled (its blue goes); \
         om-calc keeps it blue and merges it with the blue X",
    ),
    (
        "vc-cjk.ja",
        &["SortValues", KANA],
        "Japanese orders kana their own way beyond the third-level order of \
         om-calc: small kana after longer sequences that start with the same \
         kana (㌃ before ァ), ん, ン and ヿ after bopomofo, right before the kanji",
    ),
    (
        "vc-surrogates",
        &["Encode ( "],
        "FileMaker drops lone surrogates when it converts text to bytes (a lone \
         high one alone is error 17); om-calc's encoders see their stand-ins",
    ),
    (
        "vc-surrogates",
        &["JSON"],
        "JSONSetElement drops a lone surrogate from a string; om-calc keeps its \
         stand-in",
    ),
    (
        "vc-surrogates",
        &["\"a😀b\" ; Char ( 5"],
        "FileMaker searches UTF-16 units, so a lone surrogate matches half of a \
         pair (PatternCount, Position, Substitute); om-calc's stand-ins do not",
    ),
    (
        "vc-sortnames-chars",
        &["; 1 ; \"Catalog\" )", "Ặ"],
        "Catalog's third level orders a few letters with two accents (ặ, ậ, ệ) \
         and combining Latin letters its own way; not modelled",
    ),
    (
        "vc-sortnames-chars",
        &["; 1 ; \"Japanese\" )", KANA],
        "Japanese orders kana, the voiced sound marks and bopomofo its own way \
         (see vc-cjk.ja)",
    ),
    (
        "vc-sortnames-chars",
        &["UniqueValues", "; 1 ; \"Persian\" )"],
        "Persian index keys merge the reordered alef, hamza and kaf letters with \
         the letters they are placed after; not modelled",
    ),
];

fn is_known(file: &str, failure: &str) -> bool {
    let expr = failure.split_once(": ").map_or(failure, |(e, _)| e);
    KNOWN.iter().any(|(fixture, needles, _)| {
        file.starts_with(fixture)
            && needles.iter().all(|n| {
                if *n == KANA {
                    expr.chars().any(|c| {
                        matches!(
                            c,
                            '\u{3041}'..='\u{312F}'
                                | '\u{31A0}'..='\u{31BF}'
                                | '\u{31F0}'..='\u{31FF}'
                                | '\u{3300}'..='\u{3357}'
                                | '\u{FF66}'..='\u{FF9F}'
                        )
                    })
                } else {
                    expr.contains(n)
                }
            })
    })
}

/// Checks one fixture as text (`GetAsSVG` rows included, so run boundaries
/// count) and, for `GetAsSVG ( expression )` rows, position by position.
/// Returns the number of unexpected failures.
fn check(file: &str, locale: &Locale) -> usize {
    let path = format!("{}/tests/data/{file}", env!("CARGO_MANIFEST_DIR"));
    let (rows, covered, mut failures) = support::check_harness_results(&path, locale);
    let text_passed = covered - failures.len();
    let svg = styles::check_svg_rows(&path, locale, &mut failures);
    let unexpected: Vec<&String> = failures.iter().filter(|f| !is_known(file, f)).collect();
    println!(
        "{file}: {rows} rows, {covered} covered ({text_passed} passed as text); {} GetAsSVG \
         rows, {} covered; {} known",
        svg.rows,
        svg.covered,
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    unexpected.len()
}

/// Checks a fixture in each locale, all before failing.
fn check_all(name: &str, tags: &[&str]) {
    let failed: usize = tags
        .iter()
        .map(|tag| check(&format!("{name}.{tag}.tsv"), &locale(tag)))
        .sum();
    assert_eq!(failed, 0, "{name}: {failed} rows failed");
}

/// The Contacts template locales by file suffix.
fn locale(tag: &str) -> Locale {
    match tag {
        "en" => Locale::en_us(),
        "de" => Locale::de_de(),
        "fr" => Locale::fr_fr(),
        "sv" => Locale::sv_se(),
        "ja" => Locale::ja_jp(),
        "es" => Locale::es_es(),
        "it" => Locale::it_it(),
        "nl" => Locale::nl_nl(),
        "pt" => Locale::pt_pt(),
        "ko" => Locale::ko_kr(),
        "zh" => Locale::zh_cn(),
        other => panic!("no locale {other}"),
    }
}

#[test]
fn style_runs() {
    check_all("vc-styles", &["en"]);
}

#[test]
fn text_to_number() {
    check_all("vc-numbers", &["en", "de", "fr", "sv", "ja"]);
}

#[test]
fn error_minus_one() {
    check_all("vc-errors", &["en"]);
}

#[test]
fn collation_matrices() {
    check_all("vc-collation", &["en", "de", "sv", "es", "ja"]);
    check_all("vc-collation-max", &["en", "de", "sv"]);
}

#[test]
fn collation_pairs() {
    check_all(
        "vc-collation-pairs",
        &[
            "en", "de", "fr", "sv", "ja", "es", "it", "nl", "pt", "ko", "zh",
        ],
    );
}

#[test]
fn lone_surrogates() {
    check_all("vc-surrogates", &["en", "ja"]);
}

#[test]
fn collation_cjk() {
    check_all("vc-cjk", &["en", "ja", "ko", "zh"]);
}

#[test]
fn collation_locale_names() {
    check_all("vc-sortnames", &["en"]);
    check_all("vc-sortnames-chars", &["en"]);
}
