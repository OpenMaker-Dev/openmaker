//! The evaluator's host boundary with the in-memory host: repeating
//! fields, related records and portal rows, aggregates, unstored
//! calculations, custom functions and their limits, variables, `Get`,
//! `Self`, declared result types, stored formulas, and deep recursion on a
//! small thread.

mod samples;

use om_calc::eval::{self, CalcHost, Context, FieldInfo, Join, MemoryHost, NullHost, VarScope};
use om_calc::{Locale, Value, ValueType};
use std::time::{Duration, Instant};

const T: u64 = 1;
const REL: u64 = 2;
const EMPTY: u64 = 3;

/// A table T (occurrence 1) with `r` (number, 3 repetitions), `n` (empty
/// number), `name` (text), `key`, a global `g` and two unstored
/// calculations; `Rel` (occurrence 2) related by key to three records,
/// `Empty` (3) to none; custom functions `f` (tail recursive), `g`
/// (recursive, not a tail call) and `twice`.
fn host() -> MemoryHost {
    let mut host = MemoryHost::new(Locale::en_us(), samples::today());
    samples::add_stand_ins(&mut host);
    host.add_table(1, "T");
    host.add_table(2, "Other");
    host.add_table(3, "Nothing");
    host.add_occurrence(T, "T", 1);
    host.add_occurrence(REL, "Rel", 2);
    host.add_occurrence(EMPTY, "Empty", 3);
    host.add_field(1, 1, "r", FieldInfo::stored(ValueType::Number, 3));
    host.add_field(1, 2, "n", FieldInfo::stored(ValueType::Number, 1));
    host.add_field(1, 3, "name", FieldInfo::stored(ValueType::Text, 1));
    host.add_field(1, 4, "key", FieldInfo::stored(ValueType::Number, 1));
    host.add_field(
        1,
        5,
        "g",
        FieldInfo {
            value_type: ValueType::Text,
            repetitions: 1,
            storage: eval::Storage::Global,
        },
    );
    for table in [2, 3] {
        host.add_field(table, 1, "key", FieldInfo::stored(ValueType::Number, 1));
        host.add_field(table, 2, "x", FieldInfo::stored(ValueType::Number, 1));
        host.add_field(table, 3, "tag", FieldInfo::stored(ValueType::Text, 2));
    }
    host.add_calculation(
        T,
        6,
        "doubled",
        FieldInfo::stored(ValueType::Number, 3),
        "r * 10 + Get ( CalculationRepetitionNumber )",
    )
    .unwrap();
    host.add_calculation(
        T,
        7,
        "datetext",
        FieldInfo::stored(ValueType::Date, 1),
        "\"1/2/2003\"",
    )
    .unwrap();
    for (id, name) in [(1, "first"), (2, "second")] {
        host.add_record(1, id);
        host.set(1, id, 3, 1, Value::text(name));
        host.set(1, id, 4, 1, Value::number(1));
    }
    for rep in 1..=3 {
        host.set(1, 1, 1, rep, Value::number(i64::from(rep)));
    }
    host.set(1, 1, 5, 1, Value::text("global"));
    for (id, x) in [(10, 7), (11, 1), (12, 3)] {
        host.add_record(2, id);
        host.set(2, id, 1, 1, Value::number(1));
        host.set(2, id, 2, 1, Value::number(x));
        host.set(2, id, 3, 2, Value::text(format!("t{id}")));
    }
    let by_key = Join::Equal { left: 4, right: 1 };
    host.relate(T, REL, by_key);
    host.relate(T, EMPTY, by_key);
    host.add_custom_function(1, "f", &["n"], "If ( n <= 0 ; 0 ; f ( n - 1 ) )")
        .unwrap();
    host.add_custom_function(2, "g", &["n"], "If ( n <= 0 ; 0 ; 1 + g ( n - 1 ) )")
        .unwrap();
    host.add_custom_function(3, "twice", &["x"], "Let ( y = x ; y & y )")
        .unwrap();
    host
}

fn on_record() -> Context {
    Context::record(T, 1)
}

/// Result text and error code.
fn run(host: &mut MemoryHost, expr: &str, context: &Context) -> (String, i32) {
    let r = eval::evaluate_text(expr, host, context);
    (eval::display(&r.value, host), r.code())
}

fn check(host: &mut MemoryHost, context: &Context, cases: &[(&str, &str, i32)]) {
    for (expr, want, code) in cases {
        assert_eq!(
            run(host, expr, context),
            (want.to_string(), *code),
            "{expr}"
        );
    }
}

#[test]
fn repeating_fields() {
    let mut h = host();
    check(
        &mut h,
        &on_record(),
        &[
            ("r", "1", 0),
            ("r[2]", "2", 0),
            ("r[2.9]", "2", 0),
            ("r[4]", "?", 14),
            ("r[0]", "?", 14),
            ("GetRepetition ( r ; 3 )", "3", 0),
            ("Extend ( r[3] )", "1", 0),
            ("Last ( r )", "3", 0),
            ("name[2]", "?", 14),
            ("Sum ( r )", "6", 0),
            ("Sum ( r[2] )", "6", 0),
            ("Sum ( r ; 10 )", "11", 0),
            ("Count ( r[5] )", "?", 14),
            ("List ( Extend ( r ) )", "1\r2\r3", 0),
            ("GetFieldName ( r[2] )", "T::r[2]", 0),
            ("GetFieldName ( r[1] )", "T::r", 0),
        ],
    );
    // In repetition n of a repeating calculation.
    let rep2 = on_record().with_repetition(2);
    check(
        &mut h,
        &rep2,
        &[
            ("r", "2", 0),
            ("name", "", 0),
            ("Extend ( name )", "first", 0),
            ("r[1]", "1", 0),
            ("Get ( CalculationRepetitionNumber )", "2", 0),
            ("Count ( name )", "1", 0),
            ("Count ( name ; 1 )", "1", 0),
        ],
    );
    // A validation in repetition n (`extend_fields`): a field that does
    // not repeat reads its value however it is named; repeating fields and
    // explicit repetitions keep their rules.
    let validating = Context {
        extend_fields: true,
        ..on_record().with_repetition(2)
    };
    check(
        &mut h,
        &validating,
        &[
            ("name", "first", 0),
            ("name[1]", "first", 0),
            ("GetRepetition ( name ; 1 )", "first", 0),
            ("Extend ( name )", "first", 0),
            ("name[2]", "?", 14),
            ("r", "2", 0),
            ("r[1]", "1", 0),
            ("Get ( CalculationRepetitionNumber )", "2", 0),
        ],
    );
    check(&mut h, &on_record().with_repetition(4), &[("r", "", 0)]);
    let all = eval::compile("r + 1", &h, &on_record(), false)
        .unwrap()
        .expr;
    let reps: Vec<String> = eval::evaluate_repetitions(&all, &mut h, &on_record(), 3)
        .iter()
        .map(|e| eval::display(&e.value, &h))
        .collect();
    assert_eq!(reps, ["2", "3", "4"]);
}

#[test]
fn related_records_and_portals() {
    let mut h = host();
    check(
        &mut h,
        &on_record(),
        &[
            ("Rel::x", "7", 0),
            ("Sum ( Rel::x )", "11", 0),
            ("Count ( Rel::x )", "3", 0),
            ("List ( Rel::tag )", "t10\rt11\rt12", 0),
            ("Sum ( Rel::x ; 100 )", "107", 0),
            ("Last ( Rel::x )", "3", 0),
            ("GetNthRecord ( Rel::x ; 2 )", "1", 0),
            ("GetNthRecord ( Rel::x ; 4 )", "?", 14),
            ("GetNthRecord ( name ; 2 )", "second", 0),
            ("Rel::x[2]", "?", 14),
            ("Empty::x + 1", "1", 0),
            ("Count ( Empty::x )", "0", 0),
            ("Sum ( Empty::x )", "", 0),
            ("IsValid ( Empty::x )", "0", 0),
            ("Let ( v = Empty::x ; IsValid ( v ) )", "1", 0),
            ("IsValid ( Rel::x )", "1", 0),
            ("Lookup ( Rel::x )", "7", 0),
            ("Lookup ( Empty::x ; \"none\" )", "none", 0),
            ("Lookup ( Empty::x )", "?", 401),
            ("Lookup ( name )", "?", 103),
            ("LookupNext ( Empty::x ; Higher )", "?", 401),
            ("Let ( f = Rel::x ; Count ( f ) & \"/\" & f )", "3/7", 0),
            ("Let ( $f = Rel::x ; Count ( $f ) )", "?", 102),
            ("Count ( GetField ( \"Rel::x\" ) )", "3", 0),
            ("Sum ( Evaluate ( \"Rel::x\" ) )", "11", 0),
            ("GetFieldName ( Evaluate ( \"Rel::x\" ) )", "Rel::x", 0),
            ("GetBaseTableName ( Rel::x )", "Other", 0),
            ("g", "global", 0),
            (
                "Get ( RecordNumber ) & \"/\" & Get ( FoundCount )",
                "1/2",
                0,
            ),
        ],
    );
    // A portal row reads its own record.
    let portal = on_record().with_portal(REL, 12);
    check(
        &mut h,
        &portal,
        &[("Rel::x", "3", 0), ("Sum ( Rel::x )", "11", 0)],
    );
    // Without a record every field reference ends the evaluation.
    let no_record = Context {
        occurrence: Some(T),
        ..Context::new()
    };
    check(
        &mut h,
        &no_record,
        &[
            ("name", "?", 1224),
            ("EvaluationError ( name )", "?", 1224),
            ("Get ( RecordNumber )", "0", 0),
        ],
    );
}

#[test]
fn unstored_calculations_and_result_types() {
    let mut h = host();
    check(
        &mut h,
        &on_record(),
        &[
            ("doubled", "11", 0),
            ("doubled[3]", "33", 0),
            ("List ( doubled )", "11\r22\r33", 0),
            ("datetext + 1", "1/3/2003", 0),
        ],
    );
    let text = on_record().with_result_type(ValueType::Number);
    assert_eq!(run(&mut h, "\"12abc\"", &text), ("12abc".into(), 0));
    let date = on_record().with_result_type(ValueType::Date);
    assert_eq!(run(&mut h, "\"730486\"", &date), ("?".into(), 0));
}

#[test]
fn custom_functions_and_limits() {
    let mut h = host();
    let none = Context::new();
    check(
        &mut h,
        &none,
        &[
            ("f ( 10 )", "0", 0),
            ("twice ( \"ab\" )", "abab", 0),
            ("Let ( x = 7 ; twice ( 1 ) & x )", "117", 0),
            // 49,999 calls (tail calls count too); the 50,000th aborts.
            ("f ( 49998 )", "0", 0),
            ("f ( 49999 )", "?", 7),
            ("EvaluationError ( f ( 49999 ) )", "?", 7),
            ("SetRecursion ( f ( 99 ) ; 100 )", "0", 0),
            ("SetRecursion ( f ( 100 ) ; 100 )", "?", 7),
            ("SetRecursion ( f ( 0 ) ; 0 )", "?", 7),
            // SetRecursion counts on its own; its calls do not count outside.
            (
                "f ( 30000 ) & SetRecursion ( f ( 39999 ) ; 40000 )",
                "00",
                0,
            ),
            (
                "SetRecursion ( f ( 30000 ) ; 40000 ) & f ( 30000 )",
                "00",
                0,
            ),
            // Evaluate starts its own count with the default limit.
            ("f ( 30000 ) & Evaluate ( \"f ( 30000 )\" )", "00", 0),
            ("SetRecursion ( Evaluate ( \"f ( 10 )\" ) ; 5 )", "0", 0),
            (
                "While ( [ i = 0 ] ; i < 40000 ; [ i = i + 1 ] ; i ) & f ( 40000 )",
                "400000",
                0,
            ),
            (
                "While ( [ i = 0 ] ; i < 50000 ; [ i = i + 1 ] ; i )",
                "50000",
                0,
            ),
            (
                "While ( [ i = 0 ] ; i < 50001 ; [ i = i + 1 ] ; i )",
                "?",
                7,
            ),
            ("nosuch ( 1 )", "?", 1208),
        ],
    );
}

#[test]
fn variables_live_in_the_host() {
    let mut h = host();
    let none = Context::new();
    check(
        &mut h,
        &none,
        &[
            (
                "Let ( [ $a = 1 ; $$G = 2 ; $a[3] = \"three\" ] ; 0 )",
                "0",
                0,
            ),
            ("$A & $$g & $a[3]", "12three", 0),
            ("Evaluate ( \"$a + $$g\" )", "3", 0),
            ("$unset[2] + 1", "1", 0),
            ("Let ( $a[1/0] = 5 ; $a )", "1", 15),
        ],
    );
    assert_eq!(
        h.locals.get("$a", 3).map(|v| eval::display(v, &h)),
        Some("three".into())
    );
    // A script engine sets a variable; the calculation sees it.
    h.variables(VarScope::Global)
        .set("$$fromScript", 1, Value::number(42));
    assert_eq!(run(&mut h, "$$FromScript", &none), ("42".into(), 0));
    // An empty value removes the variable.
    run(&mut h, "Let ( $a = \"\" ; 0 )", &none);
    assert!(h.locals.get("$a", 1).is_none());
}

#[test]
fn get_self_random_and_hooks() {
    let mut h = host();
    h.set_get(1, Value::text("OpenMaker test"));
    let none = Context::new();
    check(
        &mut h,
        &none,
        &[
            ("Get ( ApplicationVersion )", "OpenMaker test", 0),
            ("Get ( CalculationRepetitionNumber )", "0", 0),
            ("Self", "?", 1225),
            (
                "GetLayoutObjectAttribute ( \"x\" ; \"content\" ) & \"|\"",
                "|",
                0,
            ),
            ("ExecuteSQL ( \"SELECT 1\" ; \"\" ; \"\" )", "?", 1225),
        ],
    );
    assert_eq!(h.unsupported, ["ExecuteSQL"]);
    let with_self = Context::new().with_self(Value::number(5));
    assert_eq!(run(&mut h, "Self * 2", &with_self), ("10".into(), 0));
    h.seed_random(1);
    let a = run(&mut h, "Random", &none).0;
    h.seed_random(1);
    assert_eq!(run(&mut h, "Random", &none).0, a);
    assert!(a.starts_with('.'));
}

#[test]
fn stored_formulas_evaluate_from_their_polish_stream() {
    let mut h = host();
    let compiled = eval::compile("Sum ( Rel::x ) & name", &h, &on_record(), false).unwrap();
    let bytes = om_formula::polish::encode(&compiled.expr);
    let formula = om_formula::Formula::decode(Some(&[1, 1]), None, &bytes).unwrap();
    // The formula's own context occurrence is used when the context has none.
    let context = Context {
        record: Some(1),
        ..Context::new()
    };
    let result = eval::evaluate_formula(&formula, &mut h, &context);
    assert_eq!(eval::display(&result.value, &h), "11first");
}

#[test]
fn null_host_without_a_file() {
    let mut h = NullHost::new(Locale::de_de(), samples::today());
    let r = eval::evaluate_text(
        "Evaluate ( \"1,5\" ) * 2 & Let ( $x = 1 ; $x )",
        &mut h,
        &Context::new(),
    );
    assert_eq!((eval::display(&r.value, &h), r.code()), ("31".into(), 0));
    let r = eval::evaluate_text("x", &mut h, &Context::new());
    assert_eq!(r.code(), 106);
}

/// A host whose `ExecuteSQLe` returns a text and an error together, as
/// FileMaker's does (`? ERROR: ...` and 8309; execute-sql.md 1.2).
struct ReportingHost(NullHost);

impl CalcHost for ReportingHost {
    fn locale(&self) -> &Locale {
        &self.0.locale
    }

    fn today(&self) -> om_calc::Date {
        self.0.today
    }

    fn variables(&mut self, scope: VarScope) -> &mut om_calc::eval::Variables {
        self.0.variables(scope)
    }

    fn call_reporting(
        &mut self,
        _function: eval::HostFunction<'_>,
        _args: &[Value],
        _context: &Context,
    ) -> (Value, Option<om_calc::CalcError>) {
        let error = om_calc::CalcError::from_code(8309);
        (Value::text("? ERROR: x"), error)
    }
}

#[test]
fn host_results_keep_a_value_beside_their_error() {
    let mut h = ReportingHost(NullHost::new(Locale::en_us(), samples::today()));
    let call = "ExecuteSQLe ( \"x\" ; \"\" ; \"\" )";
    let r = eval::evaluate_text(call, &mut h, &Context::new());
    assert_eq!(
        (eval::display(&r.value, &h), r.code()),
        ("? ERROR: x".into(), 8309)
    );
    let r = eval::evaluate_text(&format!("Length ( {call} )"), &mut h, &Context::new());
    assert_eq!((eval::display(&r.value, &h), r.code()), ("10".into(), 8309));
    let r = eval::evaluate_text(
        &format!("EvaluationError ( {call} )"),
        &mut h,
        &Context::new(),
    );
    assert_eq!(eval::display(&r.value, &h), "8309");
}

/// A stored read: its context's occurrence, record and related set, and
/// the record read.
type ScopedRead = (Option<u64>, Option<u64>, Option<(u64, u64)>, u64);

/// The in-memory host, recording the context of every stored read.
struct ScopeHost {
    host: MemoryHost,
    reads: std::cell::RefCell<Vec<ScopedRead>>,
}

impl CalcHost for ScopeHost {
    fn locale(&self) -> &Locale {
        self.host.locale()
    }

    fn today(&self) -> om_calc::Date {
        self.host.today()
    }

    fn names(&self) -> &dyn om_formula::Names {
        self.host.names()
    }

    fn field(&self, occurrence: u64, field: u64) -> Option<FieldInfo> {
        self.host.field(occurrence, field)
    }

    fn field_value(
        &self,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, om_calc::CalcError> {
        self.host.field_value(occurrence, record, field, repetition)
    }

    fn field_value_in_context(
        &self,
        context: &Context,
        occurrence: u64,
        record: u64,
        field: u64,
        repetition: u32,
    ) -> Result<Value, om_calc::CalcError> {
        self.reads.borrow_mut().push((
            context.occurrence,
            context.record,
            context.related_from,
            record,
        ));
        self.host.field_value(occurrence, record, field, repetition)
    }

    fn related_records(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
    ) -> Result<Vec<u64>, om_calc::CalcError> {
        self.host.related_records(occurrence, record, related)
    }

    fn related_record(
        &self,
        occurrence: u64,
        record: u64,
        related: u64,
        index: usize,
    ) -> Result<Option<u64>, om_calc::CalcError> {
        self.host.related_record(occurrence, record, related, index)
    }

    fn variables(&mut self, scope: VarScope) -> &mut om_calc::eval::Variables {
        self.host.variables(scope)
    }
}

#[test]
fn nested_calculations_keep_the_related_set_they_were_read_in() {
    const ANOTHER: u64 = 4;
    let mut host = host();
    host.add_occurrence(ANOTHER, "Another", 2);
    host.add_calculation(
        REL,
        4,
        "viaRel",
        FieldInfo::stored(ValueType::Number, 1),
        "x",
    )
    .unwrap();
    host.add_calculation(
        ANOTHER,
        5,
        "fromAnother",
        FieldInfo::stored(ValueType::Number, 1),
        "x",
    )
    .unwrap();
    let mut h = ScopeHost {
        host,
        reads: Default::default(),
    };
    let mut read = |expr: &str, context: &Context| {
        let r = eval::evaluate_text(expr, &mut h, context);
        assert_eq!(r.code(), 0, "{expr}");
        std::mem::take(&mut *h.reads.borrow_mut())
    };
    let parent = Some((T, 1));
    // A related read is asked for from the parent's context, its nested
    // calculation in the related record's context and the parent's set.
    assert_eq!(read("Rel::x", &on_record()), [(Some(T), Some(1), None, 10)]);
    assert_eq!(
        read("Rel::viaRel", &on_record()),
        [(Some(REL), Some(10), parent, 10)]
    );
    // A portal row is a record of the parent's related set too.
    assert_eq!(
        read("Rel::viaRel", &on_record().with_portal(REL, 11)),
        [(Some(REL), Some(11), parent, 11)]
    );
    // Outside a relationship nothing is carried; nor into a formula that
    // evaluates from another occurrence than the set was read in.
    assert_eq!(
        read("viaRel", &Context::record(REL, 12)),
        [(Some(REL), Some(12), None, 12)]
    );
    assert_eq!(
        read("Rel::fromAnother", &on_record()),
        [(Some(ANOTHER), Some(10), None, 10)]
    );
}

/// Runs `f` on a thread with a 2 MB stack, the size of a test thread.
fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_recursion_fits_a_small_stack() {
    on_small_stack(|| {
        let mut h = host();
        let none = Context::new();
        // Recursion that is not a tail call: every call nests. 10,000 deep
        // in a debug build (about 100 MB of heap stack segments); release
        // builds go to the 49,999 FileMaker allows.
        let depth = if cfg!(debug_assertions) {
            10_000
        } else {
            49_998
        };
        let expr = format!("g ( {depth} )");
        assert_eq!(run(&mut h, &expr, &none), (depth.to_string(), 0));
        // A tree 150 levels deep (the deepest stored formula is 54).
        let mut deep = "1".to_string();
        for _ in 0..150 {
            deep = format!("( {deep} + 1 )");
        }
        assert_eq!(run(&mut h, &deep, &none), ("151".into(), 0));
        // Evaluate inside Evaluate, 100 levels.
        h.add_custom_function(
            4,
            "nest",
            &["n"],
            "If ( n <= 0 ; 0 ; 1 + Evaluate ( \"nest ( \" & n - 1 & \" )\" ) )",
        )
        .unwrap();
        assert_eq!(run(&mut h, "nest ( 100 )", &none), ("100".into(), 0));
    });
}

/// 2,001 numbers 999 arrays deep: 6 KB that `JSONFormatElements` lays out
/// in 2 MB, a line of 999 tabs for each.
fn deep_array() -> String {
    format!(
        "{}{}0{}",
        "[".repeat(998),
        "0,".repeat(2_000),
        "]".repeat(998)
    )
}

#[test]
fn an_evaluation_stops_at_its_memory_limits() {
    on_small_stack(|| {
        let mut h = host();
        let none = Context::new();
        h.add_custom_function(
            4,
            "nest",
            &["n"],
            "If ( n <= 0 ; 0 ; 1 + Evaluate ( \"nest ( \" & n - 1 & \" )\" ) )",
        )
        .unwrap();
        h.variables(VarScope::Local)
            .set("$p", 1, Value::text("a".repeat(100_000)));
        h.variables(VarScope::Local)
            .set("$deep", 1, Value::text(deep_array()));
        // With FileMaker's 2 GB, 10 GB of text is refused before it is
        // built; error 7 ends the evaluation as the call limit does.
        let started = Instant::now();
        assert_eq!(
            run(&mut h, "Length ( Substitute ( $p ; \"a\" ; $p ) )", &none),
            ("?".into(), 7)
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // Smaller limits, as a server's.
        h.limits = eval::CalcLimits {
            text: 1 << 20,
            nesting: 1_000,
            ..eval::CalcLimits::DEFAULT
        };
        // Replace checks the result, not input plus replacement (#347).
        h.variables(VarScope::Local)
            .set("$replace", 1, Value::text("a".repeat(600_000)));
        assert_eq!(
            run(
                &mut h,
                "Length ( Replace ( $replace ; 1 ; Length ( $replace ) ; $replace ) )",
                &none,
            ),
            ("600000".into(), 0)
        );
        for formula in [
            // Evaluate nesting itself, and custom functions through it,
            // each Evaluate starting a call count of its own.
            "Let ( $x = \"Evaluate ( $x )\" ; Evaluate ( $x ) )",
            "EvaluationError ( nest ( 1000000 ) )",
            "SetRecursion ( g ( 10000000 ) ; 20000000 )",
            // Text doubled until past the limit, and layouts many times
            // their input.
            "While ( [ t = $p ] ; 1 ; [ t = t & t ] ; 0 )",
            "Length ( JSONFormatElements ( $deep ) )",
            "Length ( Evaluate ( $p & \" + 1\" ) )",
        ] {
            let started = Instant::now();
            assert_eq!(run(&mut h, formula, &none), ("?".into(), 7), "{formula}");
            let took = started.elapsed();
            assert!(took < Duration::from_secs(10), "{formula}: {took:?}");
        }
        // Within them, as before: each level of `nest` is a call and an
        // Evaluate.
        assert_eq!(run(&mut h, "nest ( 450 )", &none), ("450".into(), 0));
        assert_eq!(
            run(&mut h, "Length ( $p & $p & $p )", &none),
            ("300000".into(), 0)
        );
    });
}

#[test]
fn deeply_nested_text_compiles_on_a_small_stack() {
    on_small_stack(|| {
        let mut h = host();
        let none = Context::new();
        // Past the parser's 151 levels: refused as any text that does not
        // parse is, without a stack sized by the text (400,000 parentheses
        // used to ask for 26 GB). Inside a text constant they are text.
        h.variables(VarScope::Local)
            .set("$p", 1, Value::text("(".repeat(400_000)));
        check(
            &mut h,
            &none,
            &[
                ("IsValidExpression ( $p )", "0", 0),
                ("Evaluate ( $p )", "?", 1200),
                ("IsValidExpression ( Quote ( $p ) )", "1", 0),
                ("Length ( Evaluate ( Quote ( $p ) ) )", "400000", 0),
            ],
        );
    });
}

#[test]
fn an_interrupted_evaluation_ends_promptly() {
    on_small_stack(|| {
        let mut h = host();
        let none = Context::new();
        let hay = "a".repeat(200_000);
        let needle = format!("{}b", "a".repeat(100_000));
        h.variables(VarScope::Local)
            .set("$hay", 1, Value::text(hay));
        h.variables(VarScope::Local)
            .set("$needle", 1, Value::text(needle));
        // Each would run for an hour or more. The host stops them (on
        // OpenMaker Server, the script's abort or time limit) with error 1,
        // which EvaluationError does not catch.
        for formula in [
            "While ( [ i = 0 ] ; i < 50000 ; [ i = i + 1 ; j = While ( [ k = 0 ] ; \
             k < 50000 ; [ k = k + 1 ] ; k ) ] ; i )",
            "EvaluationError ( SetRecursion ( f ( 2000000000 ) ; 3000000000 ) )",
            "PatternCount ( $hay ; $needle )",
        ] {
            h.deadline = Some(Instant::now() + Duration::from_millis(50));
            let started = Instant::now();
            assert_eq!(run(&mut h, formula, &none), ("?".into(), 1), "{formula}");
            let took = started.elapsed();
            assert!(took < Duration::from_secs(10), "{formula}: {took:?}");
        }
    });
}

#[test]
fn every_repetition_of_a_match_field_relates() {
    // T::r holds 1, 2 and 3 on record 1; related by r to Other::x (7, 1, 3),
    // repetitions 1 and 3 each match a record.
    let mut host = host();
    host.add_occurrence(4, "ByRep", 2);
    host.relate(T, 4, Join::Equal { left: 1, right: 2 });
    check(
        &mut host,
        &on_record(),
        &[
            ("Count ( ByRep::x )", "2", 0),
            ("List ( ByRep::x )", "1\r3", 0),
        ],
    );
}
