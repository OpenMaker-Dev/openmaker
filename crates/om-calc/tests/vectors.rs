//! Runs `docs/re/catalogs/calc-value-vectors.tsv` (spec
//! `docs/re/calc-values.md` 11) through the value core.
//!
//! The shared test-only reader (`support`) evaluates the rows the core can express without
//! the real formula parser: number and text literals, `¶`, the operators,
//! `If`, `and`/`or`/`xor`/`not`, `SetPrecision`, `EvaluationError`, and the
//! number and date primitives the core implements. Rows using anything else
//! (`Let`, `Case`, variables, text functions, `Evaluate`, ...) are not
//! covered. Each covered row must match the result text, the error code
//! and the result type, except the known deviations listed below.

mod support;

use om_calc::{Ctx, Date, FirstError, Locale, Value, ValueType};
use std::collections::BTreeMap;
use support::{eval, parse, type_name, unescape, Stop};

/// Rows the value core computes differently from FileMaker: none since
/// the decimal functions follow calc-values.md 14.
const KNOWN_DEVIATIONS: &[&str] = &[];

#[derive(Default)]
struct Tally {
    rows: usize,
    covered: usize,
    passed: usize,
    known: usize,
}

#[test]
fn value_vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/re/catalogs/calc-value-vectors.tsv"
    );
    let tsv = std::fs::read_to_string(path).expect("vector catalog");
    let today = Date::from_ymd(2026, 10, 28).unwrap();
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut deviations_seen = Vec::new();
    for line in tsv.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [expr, expected, error, evidence] = cols[..] else {
            continue;
        };
        let field = |key: &str| {
            evidence
                .split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .map(str::to_string)
        };
        let battery = field("battery=").unwrap_or_else(|| "inferred".into());
        let tally = tallies.entry(battery.clone()).or_default();
        tally.rows += 1;
        if !evidence.starts_with("established:run")
            || evidence.contains("aborts-calculation")
            || evidence.contains("env=")
            || evidence.contains("schema:")
            || evidence.contains("context=")
            || evidence.contains("record=")
            || evidence.contains("rep=")
        {
            // Rows that need a table occurrence, a record or a schema wait
            // for the evaluator.
            continue;
        }
        let Some(locale) = field("file=").and_then(|f| Locale::for_tag(&f)) else {
            continue;
        };
        let cx = Ctx::new(&locale, today);
        let source = unescape(expr);
        let Ok(ast) = parse(&source, locale.decimal_separator) else {
            continue;
        };
        let mut errors = FirstError::default();
        let mut value = match eval(&ast, &cx, &mut errors) {
            Ok(value) => value,
            Err(Stop::Abort) => Value::invalid(),
            Err(Stop::Unsupported) => continue,
        };
        let declared = field("declared-result=");
        if let Some(ty) = &declared {
            let ty = match ty.as_str() {
                "text" => ValueType::Text,
                "number" => ValueType::Number,
                "date" => ValueType::Date,
                "time" => ValueType::Time,
                _ => ValueType::Timestamp,
            };
            value = value.convert_declared(ty, &cx);
        }
        tally.covered += 1;
        let got = value.to_text(&cx).into_string();
        let want = unescape(expected);
        let got_error = errors.code().to_string();
        let want_type = field("type=").unwrap();
        let type_ok = declared.is_some() || type_name(&value) == want_type;
        let ok = got == want && got_error == error && type_ok;
        if ok {
            tally.passed += 1;
        } else if KNOWN_DEVIATIONS.contains(&expr) {
            tally.known += 1;
            deviations_seen.push(expr.to_string());
        } else {
            failures.push(format!(
                "{expr} [{}]: got {got:?} error {got_error} type {}, want {want:?} error {error} type {want_type}",
                field("file=").unwrap(),
                type_name(&value),
            ));
        }
    }
    let mut total = Tally::default();
    println!("battery          rows covered passed known");
    for (battery, t) in &tallies {
        println!(
            "{battery:<16} {:>4} {:>7} {:>6} {:>5}",
            t.rows, t.covered, t.passed, t.known
        );
        total.rows += t.rows;
        total.covered += t.covered;
        total.passed += t.passed;
        total.known += t.known;
    }
    println!(
        "{:<16} {:>4} {:>7} {:>6} {:>5}",
        "total", total.rows, total.covered, total.passed, total.known
    );
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{} vectors failed", failures.len());
    for known in KNOWN_DEVIATIONS {
        assert!(
            deviations_seen.iter().any(|d| d == known),
            "{known} now passes or is gone: remove it from KNOWN_DEVIATIONS"
        );
    }
}
