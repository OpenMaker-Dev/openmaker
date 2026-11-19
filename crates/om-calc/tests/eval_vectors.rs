//! The evaluator against FileMaker Server's engine: every row of
//! `docs/re/catalogs/calc-value-vectors.tsv` and every line of the
//! evaluation results in `tools/fms-calc-harness/results/` goes through
//! `om_formula::compile` and `om_calc::eval`, in a copy of the file it ran
//! in (`samples`), and must give the engine's result text, error code and
//! type.
//!
//! A row is covered when nothing it evaluates is missing from OpenMaker: a
//! row that needs a function the library does not implement yet (or one
//! only a real host answers, such as `ExecuteSQL`) is counted but not
//! checked; the coverage grows as the function library fills in.

mod samples;

use std::collections::BTreeMap;

use om_calc::eval::{self, Context, FieldInfo, Join, MemoryHost};
use om_calc::{Locale, Value, ValueType};
use samples::{print_tallies, run, unescape, Outcome, Tally};

/// Rows of the vector catalog the evaluator computes differently from
/// FileMaker: none since the decimal functions follow calc-values.md 14.
const KNOWN_DIGITS: &[&str] = &[];

/// Lines of the harness results the evaluator does not reproduce, with
/// the reason.
const KNOWN_RESULTS: &[(&str, &str)] = &[
    // `Lookup` through the sample's cartesian (×) self-join returns empty
    // in the engine although every record is related; the rule behind it
    // (match fields? the lookup's own record choice?) is not settled, and
    // the evaluator returns the first related record's value.
    ("Lookup ( ${Related Tasks}::Task )", "cartesian Lookup"),
    (
        "Lookup ( ${Related Tasks}::Task ; \"fail\" )",
        "cartesian Lookup",
    ),
    (
        "LookupNext ( ${Related Tasks}::Task ; Lower )",
        "cartesian Lookup",
    ),
    // The engine keeps Let and While values on its solver stack: a Let in a
    // While condition overwrites a value the loop's logic bound in a slot
    // above its own here, but not in `While ( [ i = 0 ] ; Let ( z = i ; z <
    // 3 ) ; [ i = i + 1 ; w = i * 2 ] ; w )` (6) with the same slots; the
    // evaluator keeps each slot's value apart and gives 3.
    (
        "While ( [ i = 0 ] ; Let ( z = i * 100 ; z < 300 ) ; [ i = i + 1 ; w = i ] ; w )",
        "solver stack slots",
    ),
    // FileMaker Server's glibc 2.39 rounds sin(0x1.17cde069263eap+1), 1.8e-5
    // units below a tie, up; the library is correctly rounded
    // (calc-values.md 14.8).
    ("Sin ( 1529 )", "glibc near-tie rounding"),
    (
        "SetPrecision ( Sin ( 1529 ) ; 30 )",
        "glibc near-tie rounding",
    ),
];

fn field<'a>(evidence: &'a str, key: &str) -> Option<&'a str> {
    evidence
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key))
}

fn value_type(name: &str) -> Option<ValueType> {
    Some(match name {
        "text" | "1" => ValueType::Text,
        "number" | "2" => ValueType::Number,
        "date" | "3" => ValueType::Date,
        "time" | "4" => ValueType::Time,
        "timestamp" | "5" => ValueType::Timestamp,
        _ => return None,
    })
}

#[test]
fn value_vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/re/catalogs/calc-value-vectors.tsv"
    );
    let tsv = std::fs::read_to_string(path).expect("vector catalog");
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut known_seen = Vec::new();
    let mut not_covered: BTreeMap<String, usize> = BTreeMap::new();
    for line in tsv.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [expr, expected, error, evidence] = cols[..] else {
            continue;
        };
        if !evidence.starts_with("established:run") {
            continue;
        }
        if evidence.contains("schema:") {
            // Abstract-schema rules: `schema_rules` below.
            continue;
        }
        let battery = field(evidence, "battery=")
            .unwrap_or("inferred")
            .to_string();
        let tally = tallies.entry(battery).or_default();
        tally.rows += 1;
        let file = field(evidence, "file=").expect("file");
        let mut host = samples::host_for(file).unwrap_or_else(|| panic!("no host for {file}"));
        let mut context = Context::new();
        if let Some(key) = field(evidence, "context=") {
            context.occurrence = samples::occurrence(key);
            context.record = field(evidence, "record=").and_then(|r| r.parse().ok());
            context.repetition = field(evidence, "rep=").map_or(0, |r| r.parse().unwrap());
        }
        let declared = field(evidence, "declared-result=").and_then(value_type);
        context.result_type = declared;
        let source = unescape(expr);
        if samples::too_slow_for_debug(&source) {
            tally.slow += 1;
            continue;
        }
        let outcome = run(&mut host, &source, &context);
        if !outcome.unsupported.is_empty() {
            for name in &outcome.unsupported {
                *not_covered.entry(name.clone()).or_default() += 1;
            }
            continue;
        }
        tally.covered += 1;
        tally.stand_in += usize::from(outcome.stand_in);
        let want = unescape(expected);
        let want_type = field(evidence, "type=").unwrap_or("");
        let type_ok = declared.is_some() || outcome.type_name == want_type;
        if outcome.text == want && outcome.code.to_string() == error && type_ok {
            tally.passed += 1;
        } else if KNOWN_DIGITS.contains(&expr) {
            tally.known += 1;
            known_seen.push(expr);
        } else {
            failures.push(format!(
                "{expr} [{file}]: got {:?} error {} type {}, want {want:?} error {error} type {want_type}",
                outcome.text, outcome.code, outcome.type_name
            ));
        }
    }
    print_tallies("battery", &tallies);
    println!("not covered (missing functions): {not_covered:?}");
    println!("known deviations seen: {known_seen:?}");
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{} vectors failed", failures.len());
}

/// The harness result files of evaluation batteries (not the compile-mode
/// `c` batteries), each run in one host so that `$` variables persist from
/// line to line as they do in the harness. Result files are named
/// `<battery>.<file>.tsv`; others (`t-known.tsv`, the list of deviations
/// the section 14 model reproduces) are not runs.
#[test]
fn harness_results() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/fms-calc-harness/results"
    );
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("results")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            // `<battery>.<sample file>.tsv` only, without the `c` (compile)
            // batteries; the `t` batteries (calc-values.md 14) also run in
            // `fn_transcend.rs`. `fn-get` measures the engine's session
            // (clocks, ids, the server's machine and account), which the
            // memory host does not model; calc-functions.md 6.1 records it.
            name.ends_with(".tsv")
                && name.matches('.').count() == 2
                && !name.starts_with('c')
                && !name.starts_with("fn-get.")
        })
        .collect();
    files.sort();
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut not_covered: BTreeMap<String, usize> = BTreeMap::new();
    let mut known_seen = Vec::new();
    for path in files {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let file = name.split_once('.').map_or("", |(_, f)| f).to_string();
        let mut host = samples::host_for(&file).unwrap_or_else(|| panic!("no host for {file}"));
        let tally = tallies.entry(name.clone()).or_default();
        let mut context = Context::new();
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            if let Some(directive) = line.strip_prefix("# @") {
                let words: Vec<&str> = directive.split_whitespace().collect();
                match words.as_slice() {
                    ["rep", n, ..] => context.repetition = n.parse().unwrap(),
                    [key, rest @ ..] => {
                        context = Context::new();
                        context.occurrence = samples::occurrence(key);
                        context.record = rest.first().and_then(|r| r.parse().ok());
                    }
                    [] => {}
                }
                continue;
            }
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.splitn(4, '\t').collect();
            let [expr, error, types, expected] = cols[..] else {
                continue;
            };
            tally.rows += 1;
            let mut source = unescape(expr);
            let mut row_context = context.clone();
            if let Some(rest) = source.strip_prefix('!') {
                let (n, rest) = rest.split_once(' ').unwrap();
                row_context.result_type = value_type(n);
                source = rest.to_string();
            } else if source.starts_with("~ ") {
                continue;
            }
            if samples::too_slow_for_debug(&source) {
                tally.slow += 1;
                continue;
            }
            let outcome: Outcome = run(&mut host, &source, &row_context);
            if !outcome.unsupported.is_empty() {
                for name in &outcome.unsupported {
                    *not_covered.entry(name.clone()).or_default() += 1;
                }
                continue;
            }
            tally.covered += 1;
            tally.stand_in += usize::from(outcome.stand_in);
            let want = unescape(expected);
            let native = types.split('/').nth(1).and_then(value_type);
            let type_ok = row_context.result_type.is_some()
                || native.map_or(true, |t| t == value_type(outcome.type_name).unwrap_or(t));
            if outcome.text == want && outcome.code.to_string() == error && type_ok {
                tally.passed += 1;
            } else if let Some((_, why)) = KNOWN_RESULTS.iter().find(|(e, _)| *e == source) {
                tally.known += 1;
                known_seen.push(format!("{source} ({why})"));
            } else {
                failures.push(format!(
                    "{name}: {source}: got {:?} error {} type {}, want {want:?} error {error} types {types}",
                    outcome.text, outcome.code, outcome.type_name
                ));
            }
        }
    }
    print_tallies("result file", &tallies);
    println!("not covered (missing functions): {not_covered:?}");
    println!("known deviations seen: {known_seen:?}");
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(
        failures.is_empty(),
        "{} result lines failed",
        failures.len()
    );
}

/// The abstract-schema rules of the vector catalog (`schema:` rows,
/// `tools/fms-calc-harness/schema-vectors.tsv`), in the schema each row
/// describes: a table with `r` (number, 3 repetitions 1;2;3), `n` and `m`
/// (empty numbers), `x`; a relationship `Rel` to three records with x = 7,
/// 1, 3 and one, `Empty`, to none; the custom function
/// `f ( n ) = If ( n <= 0 ; 0 ; f ( n - 1 ) )`.
fn schema_host() -> MemoryHost {
    let mut host = MemoryHost::new(Locale::en_us(), samples::today());
    samples::add_stand_ins(&mut host);
    host.add_table(1, "T");
    host.add_table(2, "Other");
    host.add_table(3, "Nothing");
    host.add_occurrence(1, "T", 1);
    host.add_occurrence(2, "Rel", 2);
    host.add_occurrence(3, "Empty", 3);
    host.add_field(1, 1, "r", FieldInfo::stored(ValueType::Number, 3));
    host.add_field(1, 2, "n", FieldInfo::stored(ValueType::Number, 1));
    host.add_field(1, 3, "m", FieldInfo::stored(ValueType::Number, 1));
    host.add_field(1, 4, "key", FieldInfo::stored(ValueType::Number, 1));
    for table in [2, 3] {
        host.add_field(table, 1, "key", FieldInfo::stored(ValueType::Number, 1));
        host.add_field(table, 2, "x", FieldInfo::stored(ValueType::Number, 1));
    }
    host.add_record(1, 1);
    for rep in 1..=3 {
        host.set(1, 1, 1, rep, Value::number(i64::from(rep)));
    }
    host.set(1, 1, 4, 1, Value::number(1));
    for (id, x) in [(1, 7), (2, 1), (3, 3)] {
        host.add_record(2, id);
        host.set(2, id, 1, 1, Value::number(1));
        host.set(2, id, 2, 1, Value::number(x));
    }
    let by_key = Join::Equal { left: 4, right: 1 };
    host.relate(1, 2, by_key);
    host.relate(1, 3, by_key);
    host.add_custom_function(1, "f", &["n"], "If ( n <= 0 ; 0 ; f ( n - 1 ) )")
        .unwrap();
    host
}

#[test]
fn schema_rules() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/re/catalogs/calc-value-vectors.tsv"
    );
    let tsv = std::fs::read_to_string(path).unwrap();
    let rows: Vec<(&str, &str, &str, &str)> = tsv
        .lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c.len() == 4 && c[3].contains("schema:")).then(|| (c[0], c[1], c[2], c[3]))
        })
        .collect();
    assert_eq!(rows.len(), 17);
    let (mut covered, mut passed) = (0, 0);
    let mut failures = Vec::new();
    for (expr, expected, error, evidence) in rows {
        let mut host = schema_host();
        let context = Context::record(1, 1);
        // Descriptions stand in for some results; they are concrete in this
        // schema.
        let want = match expected {
            "(first related record's x)" => "7".to_string(),
            "(sum over all related records)" => "11".to_string(),
            "(first related record's x) + 100" => "107".to_string(),
            other => other.to_string(),
        };
        let got = if evidence.contains("calc with 3 repetitions") {
            let expr = om_calc::eval::compile(expr, &host, &context, false)
                .unwrap()
                .expr;
            let results = eval::evaluate_repetitions(&expr, &mut host, &context, 3);
            let texts: Vec<String> = results
                .iter()
                .map(|r| eval::display(&r.value, &host))
                .collect();
            Outcome {
                text: format!("{} (per repetition)", texts.join("¶")),
                code: results.iter().map(|r| r.code()).max().unwrap_or(0),
                type_name: "number",
                unsupported: Vec::new(),
                stand_in: false,
            }
        } else if evidence.contains("non-repeating calc field") {
            // Repetition 1 of a calculation field that does not repeat.
            run(&mut host, expr, &context.clone().with_repetition(1))
        } else {
            run(&mut host, expr, &context)
        };
        if !got.unsupported.is_empty() {
            continue;
        }
        covered += 1;
        if got.text == want && got.code.to_string() == error {
            passed += 1;
        } else {
            failures.push(format!(
                "{expr}: got {:?} error {}, want {want:?} error {error}",
                got.text, got.code
            ));
        }
    }
    println!("schema rules: 17 rows, {covered} covered, {passed} passed");
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty());
}
