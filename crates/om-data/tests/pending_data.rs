//! A window evaluates without waiting for its provider: what its cache
//! lacks reads as missing until fetched, and the calculation runs again
//! once it arrives ([`CachedSource::retrying`]). An evaluation that read
//! such data stops its custom function recursion and `While` loops once
//! they list nothing new, instead of running to the limit over missing
//! values; complete data, data the provider could not supply and sources
//! that wait for their data evaluate as before.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use om_data::calc::eval::{evaluate_text, Context, CustomFunction, Pending, Variables};
use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
use om_data::formula::{self, Expr};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldDef, FileCalc, FileHost,
    RecordValues, SessionInfo,
};

const ORDERS_TABLE: u64 = 0x8001;
const LINES_TABLE: u64 = 0x8002;
const ORDERS: u64 = 0xd00001;
const LINES: u64 = 0xd00002;

/// Every day from `d` up to the day before `end`, a value each: the
/// recursion only ends on reaching `end`.
const DAYS: &str = "Days ( Opened ; Lines::Due ; \"\" )";
/// The same count of days as a `While` loop.
const DAYS_WHILE: &str = "While ( [ d = Opened ; n = 0 ] ; d ≠ Lines::Due ; \
    [ $$calls = $$calls + 1 ; d = d + 1 ; n = n + 1 ] ; n )";
const DAYS_TEXT: &str = "1/1/2026\r1/2/2026\r1/3/2026\r";

fn compile(schema: &CalcSchema, text: &str, parameters: &[String]) -> Expr {
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(ORDERS);
    context.parameters = parameters;
    formula::compile(text, &context).unwrap().expr
}

/// Orders with an Opened date; Lines with a Due date and a Code; custom
/// function `Days ( d ; end ; acc )`, tail recursive, counting its calls
/// in `$$calls`.
fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Pending data", Locale::en_us());
    schema.add_table(ORDERS_TABLE, "Orders");
    schema.add_occurrence(ORDERS, "Orders", ORDERS_TABLE);
    schema.add_field(ORDERS_TABLE, FieldDef::data(1, "Opened", ValueType::Date));
    schema.add_table(LINES_TABLE, "Lines");
    schema.add_occurrence(LINES, "Lines", LINES_TABLE);
    schema.add_field(LINES_TABLE, FieldDef::data(1, "Due", ValueType::Date));
    schema.add_field(LINES_TABLE, FieldDef::data(2, "Code", ValueType::Text));
    let parameters: Vec<String> = ["d", "end", "acc"].map(String::from).to_vec();
    // Declared first so that the body's call to itself compiles.
    let mut days = CustomFunction {
        name: "Days".into(),
        parameters,
        body: Expr::Empty,
    };
    schema.add_custom_function(1, days.clone());
    days.body = compile(
        &schema,
        "Let ( $$calls = $$calls + 1 ; \
         Case ( d = end ; acc ; Days ( d + 1 ; end ; acc & d & ¶ ) ) )",
        &days.parameters,
    );
    schema.add_custom_function(1, days);
    schema
}

fn date(month: u32, day: u32) -> Value {
    Value::Date(Date::from_ymd(2026, month, day).unwrap())
}

fn record(values: &[(u64, Value)]) -> Option<Arc<RecordValues>> {
    let mut record = RecordValues::default();
    for (field, value) in values {
        record.set(*field, 1, value.clone());
    }
    Some(Arc::new(record))
}

/// What the window has before it asks for anything: order 1, opened on
/// 1/1/2026.
fn order() -> CalcData {
    let mut data = CalcData::default();
    data.records
        .insert((ORDERS_TABLE, 1), record(&[(1, date(1, 1))]));
    data
}

fn lines_of_order() -> CalcNeed {
    CalcNeed::RelatedIds {
        from: ORDERS,
        record: 1,
        to: LINES,
    }
}

/// What the provider answers for [`lines_of_order`]: two lines, the first
/// due on 1/4/2026, with their records.
fn lines() -> CalcData {
    let mut data = CalcData::default();
    data.related.insert((ORDERS, 1, LINES), Arc::from([1, 2]));
    data.records.insert(
        (LINES_TABLE, 1),
        record(&[(1, date(1, 4)), (2, Value::text("a"))]),
    );
    data.records.insert(
        (LINES_TABLE, 2),
        record(&[(1, date(2, 1)), (2, Value::text("b"))]),
    );
    data
}

/// Everything the formulas read.
fn complete() -> CalcData {
    let mut data = order();
    data.merge(lines());
    data
}

#[derive(Debug, PartialEq)]
struct Run {
    value: String,
    code: i32,
    /// Custom function calls and loop iterations, from `$$calls`.
    calls: u64,
}

/// `text` on order 1 over `source`, counting from a fresh `$$calls`.
fn run(schema: &CalcSchema, source: &dyn CalcSource, text: &str) -> Run {
    let today = Date::from_ymd(2026, 10, 30).unwrap();
    let session = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let r = evaluate_text(
        text,
        &mut FileHost::new(
            FileCalc::new(schema, source, today),
            &session,
            &mut locals,
            &mut globals,
        ),
        &Context::record(ORDERS, 1),
    );
    let cx = Ctx::new(&schema.locale, today);
    let calls = globals
        .get("$$calls", 1)
        .and_then(|v| v.to_number(&cx).to_i64())
        .unwrap_or(0);
    Run {
        value: r.value.to_text(&cx).into_string(),
        code: r.code(),
        calls: calls as u64,
    }
}

fn ran(value: &str, code: i32, calls: u64) -> Run {
    Run {
        value: value.into(),
        code,
        calls,
    }
}

#[test]
fn recursion_over_pending_data_returns_with_its_request() {
    let schema = schema();
    let failed = BTreeSet::new();
    let cache = RefCell::new(order());
    let source = CachedSource::new(&cache).retrying(&failed);
    // Without stopping, `d = end` never holds over the missing due date:
    // 49,999 calls and error 7, only for the result to be discarded.
    let first = run(&schema, &source, DAYS);
    assert!(first.calls <= 2, "{first:?}");
    assert_eq!((first.value.as_str(), first.code), ("?", 1224));
    assert_eq!(source.missing(), [lines_of_order()]);
    assert_eq!(source.pending(), Pending { reads: 1, needs: 1 });

    // The provider's answer arrives: the same value as with every record
    // at hand, and as a source that waits for its data.
    cache.borrow_mut().merge(lines());
    let source = CachedSource::new(&cache).retrying(&failed);
    let done = run(&schema, &source, DAYS);
    assert_eq!(done, ran(DAYS_TEXT, 0, 4));
    assert!(!source.has_missing());
    assert_eq!(source.pending(), Pending::default());
    let full = RefCell::new(complete());
    assert_eq!(run(&schema, &CachedSource::new(&full), DAYS), done);
    let empty = RefCell::new(order());
    let fetch = |_: &[CalcNeed]| Some(lines());
    let waiting = CachedSource::new(&empty)
        .with_fetch(&fetch)
        .retrying(&failed);
    assert_eq!(run(&schema, &waiting, DAYS), done);
}

#[test]
fn while_over_pending_data_returns_with_its_request() {
    let schema = schema();
    let failed = BTreeSet::new();
    let cache = RefCell::new(order());
    let source = CachedSource::new(&cache).retrying(&failed);
    // A `?` condition is true: 50,000 iterations and error 7 otherwise.
    let first = run(&schema, &source, DAYS_WHILE);
    assert!(first.calls <= 2, "{first:?}");
    assert_eq!((first.value.as_str(), first.code), ("?", 1224));
    assert_eq!(source.missing(), [lines_of_order()]);

    cache.borrow_mut().merge(lines());
    let source = CachedSource::new(&cache).retrying(&failed);
    let done = run(&schema, &source, DAYS_WHILE);
    assert_eq!(done, ran("3", 0, 3));
    let full = RefCell::new(complete());
    assert_eq!(run(&schema, &CachedSource::new(&full), DAYS_WHILE), done);
}

/// A loop that lists a need on every iteration runs on, so that one pass
/// asks for every record it reads rather than one per round trip.
#[test]
fn loops_listing_new_needs_ask_for_them_together() {
    let schema = schema();
    let failed = BTreeSet::new();
    let mut data = order();
    data.related.insert((ORDERS, 1, LINES), Arc::from([1, 2]));
    let cache = RefCell::new(data);
    let source = CachedSource::new(&cache).retrying(&failed);
    let codes = "While ( [ i = 0 ; out = \"\" ] ; i < 2 ; \
        [ $$calls = $$calls + 1 ; i = i + 1 ; out = out & GetNthRecord ( Lines::Code ; i ) ] ; \
        out )";
    let first = run(&schema, &source, codes);
    assert_eq!(first.calls, 2, "{first:?}");
    assert_eq!(
        source.missing(),
        [1, 2].map(|record| CalcNeed::Record {
            table: LINES_TABLE,
            record
        })
    );

    cache.borrow_mut().merge(lines());
    let source = CachedSource::new(&cache).retrying(&failed);
    assert_eq!(run(&schema, &source, codes), ran("ab", 0, 2));
}

/// The pending state is the source's, shared by every evaluation over it:
/// one that reads none of it still runs to its end, and one that reads a
/// need already listed stops too.
#[test]
fn only_evaluations_that_read_pending_data_stop() {
    let schema = schema();
    let failed = BTreeSet::new();
    let cache = RefCell::new(order());
    let source = CachedSource::new(&cache).retrying(&failed);
    assert!(run(&schema, &source, DAYS).calls <= 2);
    let unrelated = run(&schema, &source, "Days ( Opened ; Opened + 3 ; \"\" )");
    assert_eq!(unrelated, ran(DAYS_TEXT, 0, 4));
    let again = run(&schema, &source, DAYS);
    assert!(again.calls <= 1, "{again:?}");
    assert_eq!(again.code, 1224);
    assert_eq!(source.missing(), [lines_of_order()]);
}

/// What the provider could not supply stays missing: the evaluation runs
/// as it always did, to its result, rather than waiting for it again.
/// (`SetRecursion` keeps the full run short.)
#[test]
fn failed_needs_and_waiting_sources_are_not_pending() {
    let schema = schema();
    let bounded = format!("SetRecursion ( {DAYS} ; 300 )");
    let full_run = ran("?", 7, 300);
    let failed = BTreeSet::from([lines_of_order()]);
    let cache = RefCell::new(order());

    let source = CachedSource::new(&cache).retrying(&failed);
    assert_eq!(run(&schema, &source, &bounded), full_run);
    assert_eq!(source.missing(), [lines_of_order()]);
    assert_eq!(source.pending(), Pending::default());

    // By default what is missing stays missing, as it does after a fetch
    // that could not supply it.
    let source = CachedSource::new(&cache);
    assert_eq!(run(&schema, &source, &bounded), full_run);
    assert_eq!(source.pending(), Pending::default());
    let none = BTreeSet::new();
    let fetch = |_: &[CalcNeed]| -> Option<CalcData> { None };
    let source = CachedSource::new(&cache).with_fetch(&fetch).retrying(&none);
    assert_eq!(run(&schema, &source, &bounded), full_run);
    assert_eq!(source.pending(), Pending::default());
}
