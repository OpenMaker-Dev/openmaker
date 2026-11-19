//! The JSON functions against FileMaker Server's engine: the battery
//! `tools/fms-calc-harness/batteries/fn-json.txt` run in the English
//! Contacts template (`tests/data/fn-json.en.tsv`). Rows that abort the
//! engine (the undocumented value functions meeting an object or array
//! element) are not in the battery (calc-functions.md 9, JSON oracle
//! answers).

mod support;

use om_calc::eval::{self, CalcHost, Context, MemoryHost, VarScope};
use om_calc::{Date, Locale, Value};

/// `JSONSetElement` with a number, date or time as its JSON argument: the
/// engine returns the number 11.11 (or `?` for dates and times), which is
/// not reproduced; OpenMaker reads the value's text like the other
/// functions do.
const NOT_TEXT: &str =
    "the engine's 11.11 / ? for a non-text JSONSetElement document is not cloned";

/// Rows that are expected to differ, with the reason.
const KNOWN: &[(&str, &str)] = &[
    ("JSONSetElement ( 5 ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    ("JSONSetElement ( Pi ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    ("JSONSetElement ( 1 = 1 ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    ("JSONSetElement ( GetAsNumber ( \"[1]\" ) ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    ("JSONSetElement ( Date ( 1 ; 2 ; 2003 ) ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    ("JSONSetElement ( Time ( 1 ; 0 ; 0 ) ; \"a\" ; 1 ; JSONNumber )", NOT_TEXT),
    (
        "JSONFormatElements ( JSONSetElement ( \"{}\" ; \"a\" ; Char ( 1 ) & Char ( 8 ) & Char ( 12 ) & Char ( 13 ) & Char ( 127 ) & Char ( 133 ) & Char ( 160 ) & Char ( 8232 ) & Char ( 8233 ) & Char ( 65279 ) ; JSONString ) )",
        "the engine formats its cached parse, whose string still holds U+2028/U+2029; OpenMaker re-reads the result text, where they are already CR",
    ),
];

#[test]
fn english() {
    let path = format!("{}/tests/data/fn-json.en.tsv", env!("CARGO_MANIFEST_DIR"));
    let (rows, covered, failures) = support::check_harness_results(&path, &Locale::en_us());
    let unexpected: Vec<&String> = failures
        .iter()
        .filter(|f| !KNOWN.iter().any(|(e, _)| f.starts_with(&format!("{e}: "))))
        .collect();
    println!(
        "fn-json.en: {rows} rows, {covered} covered, {} passed, {} known",
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
/// in the five Contacts locales: `fn-json-intl.txt`.
#[test]
fn locales() {
    support::check_fixture_set("fn-json-intl", &[]);
}

/// Hostile JSON on a thread with the 2 MiB stack of a server thread, where
/// a stack overflow or a failed allocation would abort the process. Each
/// of these aborted or took gigabytes before the JSON functions stopped
/// recursing over nesting and bounded what a call may build.
#[test]
fn hostile_json_fits_a_server_thread() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut host = MemoryHost::new(Locale::en_us(), Date::from_ymd(2026, 11, 9).unwrap());
            // Not a tail call, so each JSON call runs below the frames of
            // all the calls before it: reading, writing and dropping the
            // 999-deep document must not need more stack than a flat one.
            host.add_custom_function(
                1,
                "Walk",
                &["json", "i"],
                "If ( i > 0 ; Walk ( json ; i - 1 ) & \
                 JSONGetElementType ( JSONGetElement ( json ; \"[0]\" ) ; \"[0]\" ) ; \"\" )",
            )
            .unwrap();
            let deep = format!("{}1{}", "[".repeat(999), "]".repeat(999));
            let path = vec!["a"; 20_000].join(".");
            for (name, value) in [("$deep", deep), ("$path", path)] {
                host.variables(VarScope::Local)
                    .set(name, 1, Value::text(value));
            }
            let stack_limit = "? Exceeded stackLimit in readValue().";
            let set =
                |path: &str| format!("JSONSetElement ( \"[]\" ; \"{path}\" ; 1 ; JSONNumber )");
            for (expr, expected) in [
                ("Length ( Walk ( $deep ; 600 ) )".to_string(), "600"),
                // A path, or a value set at one, deeper than the reader's
                // 1,000 levels: the error reading such a result would give.
                (
                    "JSONSetElement ( \"{}\" ; $path ; 1 ; JSONNumber )".to_string(),
                    stack_limit,
                ),
                (
                    "JSONSetElement ( \"{}\" ; \"a\" ; $deep ; JSONRaw )".to_string(),
                    stack_limit,
                ),
                // A million elements added to arrays per call in all.
                (set("[16777216]"), "?"),
                (set("[16777216][16777216][16777216][16777216]"), "?"),
                (set("[500000][500000]"), "?"),
            ] {
                let result = eval::evaluate_text(&expr, &mut host, &Context::new());
                assert_eq!(eval::display(&result.value, &host), expected, "{expr}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
