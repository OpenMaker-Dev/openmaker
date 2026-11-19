//! The provider-backed calculation host over an in-memory file: every
//! field of a record, repetitions, globals, related records and
//! aggregates, the found set, unstored calculations, summaries, custom
//! functions, names for `Evaluate`, recording data for a client and
//! layout calculations.

use std::cell::RefCell;
use std::sync::Arc;

use om_data::calc::eval::{display, evaluate_text, Context, CustomFunction, Variables};
use om_data::calc::{Date, Locale, Value, ValueType};
use om_data::formula::{Formula, Key};
use om_data::host::layout::{merge_text, object_state};
use om_data::host::{
    CachedSource, CalcNeed, CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc, FileHost,
    JoinOp, JoinPredicate, MemorySource, RecordValues, Recorder, RelationshipDef, SessionInfo,
    SummaryDef, SummaryKind,
};
use om_layout::{CfCondition, ConditionalFormat, LayoutObject, ObjectCalcs};

const PEOPLE: u64 = 0x8001;
const ORDERS: u64 = 0x8002;
const PEOPLE_TO: u64 = 0xd0_0001;
const ORDERS_TO: u64 = 0xd0_0002;
const LONELY_TO: u64 = 0xd0_0003;

fn compile(schema: &CalcSchema, occurrence: Option<u64>, source: &str) -> Formula {
    let mut ctx = om_data::formula::Context::new(schema.names());
    ctx.occurrence = occurrence;
    ctx.support_self = true;
    let expr = om_data::formula::compile(source, &ctx)
        .unwrap_or_else(|e| panic!("{source}: {e:?}"))
        .expr;
    Formula {
        context: occurrence.map(Key::from_id),
        formatted: None,
        polish: expr,
    }
}

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Shop", Locale::en_us());
    s.add_table(PEOPLE, "People");
    s.add_table(ORDERS, "Orders");
    s.add_table(0x8003, "Lonely");
    s.add_occurrence(PEOPLE_TO, "People", PEOPLE);
    s.add_occurrence(ORDERS_TO, "Orders", ORDERS);
    s.add_occurrence(LONELY_TO, "Lonely", 0x8003);
    s.add_field(PEOPLE, FieldDef::data(1, "id", ValueType::Number));
    s.add_field(PEOPLE, FieldDef::data(2, "Name", ValueType::Text));
    s.add_field(
        PEOPLE,
        FieldDef::data(3, "Scores", ValueType::Number).with_repetitions(3),
    );
    s.add_field(
        PEOPLE,
        FieldDef::data(6, "gLabel", ValueType::Text).with_global(true),
    );
    s.add_field(ORDERS, FieldDef::data(1, "Person", ValueType::Number));
    s.add_field(ORDERS, FieldDef::data(2, "Amount", ValueType::Number));
    for (id, name, kind) in [
        (3, "Order count", SummaryKind::Count),
        (4, "Order total", SummaryKind::Total),
    ] {
        s.add_field(
            ORDERS,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef::new(kind, 2)),
                ..FieldDef::data(id, name, ValueType::Number)
            },
        );
    }
    s.add_relationship(RelationshipDef::new(
        1,
        PEOPLE_TO,
        ORDERS_TO,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 1,
        }],
    ));
    let double = {
        let names = s.names().clone();
        let parameters = vec!["x".to_owned()];
        let mut ctx = om_data::formula::Context::new(&names);
        ctx.parameters = &parameters;
        om_data::formula::compile("x * 2", &ctx).unwrap().expr
    };
    s.add_custom_function(
        1,
        CustomFunction {
            name: "Double".into(),
            parameters: vec!["x".into()],
            body: double,
        },
    );
    // Unstored calculations, compiled once the names they use exist.
    let total = compile(&s, Some(PEOPLE_TO), "Sum ( Orders::Amount )");
    s.add_field(
        PEOPLE,
        FieldDef::calculation(4, "Total", ValueType::Number, total, false),
    );
    let greeting = compile(&s, Some(PEOPLE_TO), "\"Hi \" & Name");
    s.add_field(
        PEOPLE,
        FieldDef::calculation(5, "Greeting", ValueType::Text, greeting, false),
    );
    s.add_layout(1, "People", Some(PEOPLE_TO));
    s.add_layout(2, "Orders", Some(ORDERS_TO));
    s
}

fn values(pairs: &[(u64, u32, Value)]) -> RecordValues {
    let mut v = RecordValues::default();
    for (field, rep, value) in pairs {
        v.set(*field, *rep, value.clone());
    }
    v
}

fn source() -> MemorySource {
    let mut m = MemorySource::new();
    let n = Value::number;
    let t = Value::text;
    m.put_record(
        PEOPLE,
        1,
        values(&[(1, 1, n(1)), (2, 1, t("Ada")), (3, 1, n(10)), (3, 2, n(20))]),
    );
    m.put_record(PEOPLE, 2, values(&[(1, 1, n(2)), (2, 1, t("Ben"))]));
    m.put_record(PEOPLE, 3, values(&[(1, 1, n(3)), (2, 1, t("Cy"))]));
    for (id, person, amount) in [(10, 1, 5), (11, 1, 7), (12, 2, 100)] {
        m.put_record(ORDERS, id, values(&[(1, 1, n(person)), (2, 1, n(amount))]));
    }
    m.set_globals(PEOPLE, values(&[(6, 1, t("Shared"))]));
    m
}

fn today() -> Date {
    Date::from_ymd(2026, 10, 28).unwrap()
}

/// Evaluate `text` on `record` of occurrence `occurrence`.
fn eval_on(
    schema: &CalcSchema,
    source: &dyn CalcSource,
    occurrence: u64,
    record: u64,
    text: &str,
) -> (String, i32) {
    let session = SessionInfo {
        account_name: "Admin".into(),
        ..SessionInfo::for_this_machine()
    };
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let result = evaluate_text(text, &mut host, &Context::record(occurrence, record));
    (display(&result.value, &host), result.code())
}

fn eval(text: &str, record: u64) -> String {
    let (value, code) = eval_on(&schema(), &source(), PEOPLE_TO, record, text);
    assert_eq!(code, 0, "{text}");
    value
}

#[test]
fn every_field_of_the_record_with_types_and_repetitions() {
    assert_eq!(eval("Name", 1), "Ada");
    assert_eq!(eval("id + 1", 2), "3");
    assert_eq!(eval("Scores[2]", 1), "20");
    assert_eq!(eval("Count ( Scores )", 1), "2");
    assert_eq!(eval("Sum ( Scores )", 1), "30");
    assert_eq!(eval("Double ( Scores )", 1), "20");
    // Global storage is the same on every record.
    assert_eq!(eval("gLabel", 3), "Shared");
}

#[test]
fn related_fields_read_the_first_record_and_aggregates_all() {
    assert_eq!(eval("Orders::Amount", 1), "5");
    assert_eq!(eval("Sum ( Orders::Amount )", 1), "12");
    assert_eq!(eval("Count ( Orders::Amount )", 1), "2");
    assert_eq!(eval("List ( Orders::Amount )", 1), "5\r7");
    assert_eq!(eval("GetNthRecord ( Orders::Amount ; 2 )", 1), "7");
    assert_eq!(eval("Orders::Amount + 1", 3), "1");
    assert_eq!(eval("Count ( Orders::Amount )", 3), "0");
    // Unrelated occurrences are error 103.
    let (_, code) = eval_on(&schema(), &source(), PEOPLE_TO, 1, "Lonely::x");
    assert_ne!(code, 0);
}

#[test]
fn unstored_calculations_evaluate_when_read() {
    assert_eq!(eval("Total", 1), "12");
    assert_eq!(eval("Total", 2), "100");
    assert_eq!(eval("Greeting & \"!\"", 2), "Hi Ben!");
}

#[test]
fn the_found_set_answers_record_numbers_and_get_nth_record() {
    assert_eq!(eval("GetNthRecord ( Name ; 2 )", 1), "Ben");
    assert_eq!(eval("Get ( FoundCount )", 1), "3");
    assert_eq!(eval("Get ( RecordNumber )", 3), "3");
    assert_eq!(eval("Get ( TotalRecordCount )", 1), "3");
    assert_eq!(eval("Get ( RecordID )", 2), "2");
    assert_eq!(eval("Get ( AccountName )", 2), "Admin");
    assert_eq!(eval("Get ( LayoutCount )", 2), "2");
}

#[test]
fn summaries_compute_over_the_found_set() {
    let (count, _) = eval_on(&schema(), &source(), ORDERS_TO, 10, "Order count");
    let (total, _) = eval_on(&schema(), &source(), ORDERS_TO, 10, "Order total");
    assert_eq!((count.as_str(), total.as_str()), ("3", "112"));
    let (grand, _) = eval_on(
        &schema(),
        &source(),
        ORDERS_TO,
        10,
        "GetSummary ( Order total ; Order total )",
    );
    assert_eq!(grand, "112");
}

#[test]
fn names_compile_evaluate_and_get_field_text() {
    assert_eq!(eval("Evaluate ( \"Name & \\\"!\\\"\" )", 1), "Ada!");
    assert_eq!(eval("GetField ( \"Orders::Amount\" )", 1), "5");
    assert_eq!(eval("GetFieldName ( Orders::Amount )", 1), "Orders::Amount");
    // By name, as FileMaker Server lists occurrences.
    assert_eq!(eval("TableNames ( \"\" )", 1), "Lonely\rOrders\rPeople");
}

#[test]
fn missing_records_fail_rather_than_read_empty() {
    let (value, code) = eval_on(&schema(), &source(), PEOPLE_TO, 99, "Name");
    assert_eq!((value.as_str(), code), ("?", 101));
}

#[test]
fn recorded_data_lets_a_client_evaluate_without_the_file() {
    let schema = schema();
    let file = source();
    let recorder = Recorder::new(&file);
    let formulas = [
        "Name & Sum ( Orders::Amount ) & Get ( FoundCount )",
        "Total",
    ];
    let direct: Vec<_> = formulas
        .iter()
        .map(|f| eval_on(&schema, &recorder, PEOPLE_TO, 1, f))
        .collect();
    let cache = RefCell::new(recorder.take());
    let cached = CachedSource::new(&cache);
    let replayed: Vec<_> = formulas
        .iter()
        .map(|f| eval_on(&schema, &cached, PEOPLE_TO, 1, f))
        .collect();
    assert_eq!(direct, replayed);
    assert_eq!(replayed[0].0, "Ada123");
    assert!(!cached.has_missing());

    // A record the cache lacks is listed for fetching and reads as missing.
    let (_, code) = eval_on(&schema, &cached, PEOPLE_TO, 2, "Name");
    assert_ne!(code, 0);
    assert_eq!(
        cached.missing(),
        [CalcNeed::Record {
            table: PEOPLE,
            record: 2
        }]
    );

    // With a fetcher the cache fills itself, as a script waits for data.
    let fetch = |needs: &[CalcNeed]| {
        let recorder = Recorder::new(&file);
        for need in needs {
            recorder
                .fetch(
                    &schema,
                    need,
                    &om_data::calc::Ctx::new(&schema.locale, today()),
                )
                .ok()?;
        }
        Some(recorder.take())
    };
    let fetching = CachedSource::new(&cache).with_fetch(&fetch);
    assert_eq!(eval_on(&schema, &fetching, PEOPLE_TO, 2, "Total").0, "100");
    assert!(!fetching.has_missing());

    // A record being edited overlays the cached one.
    let draft = Arc::new(values(&[
        (1, 1, Value::number(1)),
        (2, 1, Value::text("Ava")),
    ]));
    let editing = CachedSource::new(&cache).with_overlay(PEOPLE, 1, draft);
    assert_eq!(
        eval_on(&schema, &editing, PEOPLE_TO, 1, "Greeting").0,
        "Hi Ava"
    );
    // Its related records stay the stored set while no match field
    // changes (#1308): no scan of Orders, whose record list is not cached.
    assert_eq!(eval_on(&schema, &editing, PEOPLE_TO, 1, "Total").0, "12");
    assert!(!editing.has_missing(), "{:?}", editing.missing());
    let amount = Arc::new(values(&[
        (1, 1, Value::number(1)),
        (2, 1, Value::number(50)),
    ]));
    let editing = CachedSource::new(&cache).with_overlay(ORDERS, 10, amount);
    assert_eq!(eval_on(&schema, &editing, PEOPLE_TO, 1, "Total").0, "57");
    assert!(!editing.has_missing(), "{:?}", editing.missing());
    // A changed match field relates other records: the source's key, or a
    // stored target record's. Those sets are scanned from the records.
    let scanned = |table, record, pairs: &[(u64, u32, Value)]| {
        let source = CachedSource::new(&cache).with_fetch(&fetch).with_overlay(
            table,
            record,
            Arc::new(values(pairs)),
        );
        eval_on(&schema, &source, PEOPLE_TO, 1, "Total").0
    };
    assert_eq!(scanned(PEOPLE, 1, &[(1, 1, Value::number(2))]), "100");
    assert_eq!(
        scanned(
            ORDERS,
            10,
            &[(1, 1, Value::number(2)), (2, 1, Value::number(5))]
        ),
        "7"
    );
    assert!(cache.borrow().tables.contains_key(&ORDERS));
    // A new record the transaction holds (#892) has no related set of its
    // own on the host: under a draft it is scanned from its held key.
    {
        let mut cache = cache.borrow_mut();
        let mut held = values(&[(1, 1, Value::number(1)), (2, 1, Value::text("New"))]);
        held.held_new = true;
        cache.records.insert((PEOPLE, 9), Some(Arc::new(held)));
        cache
            .related
            .insert((PEOPLE_TO, 9, ORDERS_TO), Arc::from([]));
    }
    let typed = Arc::new(values(&[
        (1, 1, Value::number(1)),
        (2, 1, Value::text("Newer")),
    ]));
    let held = CachedSource::new(&cache)
        .with_fetch(&fetch)
        .with_overlay(PEOPLE, 9, typed);
    assert_eq!(eval_on(&schema, &held, PEOPLE_TO, 9, "Total").0, "12");
    // An emptied key relates nothing, read from no record of Orders.
    cache.borrow_mut().tables.remove(&ORDERS);
    let emptied = CachedSource::new(&cache).with_overlay(
        PEOPLE,
        1,
        Arc::new(values(&[(2, 1, Value::text("Ada"))])),
    );
    assert_eq!(eval_on(&schema, &emptied, PEOPLE_TO, 1, "Total").0, "");
    assert!(!emptied.has_missing(), "{:?}", emptied.missing());
}

#[test]
fn layout_calculations_hide_format_and_label_objects() {
    let schema = schema();
    let file = source();
    let field = |source: &str| Some(compile(&schema, None, source));
    let object = LayoutObject {
        id: 5,
        calcs: ObjectCalcs {
            hide: field("People::Name = \"Ben\""),
            conditions: vec![
                ConditionalFormat {
                    formula: field("Self > 15"),
                    condition: CfCondition::default(),
                    kind: None,
                },
                ConditionalFormat {
                    formula: field("Self < 15"),
                    condition: CfCondition::default(),
                    kind: None,
                },
            ],
            tooltip: field("\"Total: \" & People::Total"),
            placeholder: field("$$hint"),
            ..ObjectCalcs::default()
        },
        ..LayoutObject::default()
    };
    let session = SessionInfo::for_this_machine();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    globals.set("$$hint", 1, Value::text("Type a name"));
    globals.set("$$who", 1, Value::text("Ada"));
    let mut host = FileHost::new(
        FileCalc::new(&schema, &file, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let on = |record| Context::record(PEOPLE_TO, record);
    let state = object_state(&mut host, &object, &on(1), Value::number(20));
    assert!(!state.hidden);
    assert_eq!(state.conditions, [0]);
    assert_eq!(state.tooltip.as_deref(), Some("Total: 12"));
    assert_eq!(state.placeholder.as_deref(), Some("Type a name"));
    assert!(object_state(&mut host, &object, &on(2), Value::number(20)).hidden);
    assert_eq!(
        merge_text(
            &mut host,
            "Hello <<$$who>>, record {{RecordNumber}} <<Name>>",
            &on(3)
        ),
        "Hello Ada, record 3 <<Name>>"
    );
    // A symbol held as its selector's number; another number stays as typed.
    assert_eq!(
        merge_text(&mut host, "Record {{1020}}, {{9999}}, {{+1020}}", &on(3)),
        "Record 3, {{9999}}, {{+1020}}"
    );
}
