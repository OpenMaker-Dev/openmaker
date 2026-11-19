//! Unstored source keys use the normal evaluator, not absent stored cells.
//! All names and records here are synthetic.
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::sync::Arc;

use om_data::calc::eval::CustomFunction;
use om_data::calc::{CalcError, Ctx, Date, Locale, Value, ValueType};
use om_data::formula::{self, Formula, Key};
use om_data::host::{
    related_by_scan, source_match_values, CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource,
    FieldDef, JoinOp, JoinPredicate, MemorySource, RecordValues, Recorder, RelationshipDef,
    SessionInfo,
};

const P: u64 = 0x8001;
const C: u64 = 0x8002;
const F: u64 = 0x8003;
const FROM: u64 = 0xd00001;
const TO: u64 = 0xd00002;
const FALLBACK: u64 = 0xd00003;

fn today() -> Date {
    Date::from_ymd(2026, 1, 2).unwrap()
}
fn compile(schema: &CalcSchema, text: &str) -> Formula {
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(FROM);
    Formula {
        context: Some(Key::from_id(FROM)),
        formatted: None,
        polish: formula::compile(text, &context).unwrap().expr,
    }
}
fn relation(id: u64, to: u64, left: u64) -> RelationshipDef {
    RelationshipDef::new(
        id,
        FROM,
        to,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left,
            right: 1,
        }],
    )
}
fn make_schema(ty: ValueType, repetitions: u32, text: &str) -> CalcSchema {
    let mut schema = CalcSchema::new("Match keys", Locale::en_us());
    for (table, occurrence, name) in [
        (P, FROM, "Parent"),
        (C, TO, "Child"),
        (F, FALLBACK, "Fallback"),
    ] {
        schema.add_table(table, name);
        schema.add_occurrence(occurrence, name, table);
        schema.add_field(
            table,
            FieldDef::data(1, "Input", ty).with_repetitions(repetitions),
        );
    }
    schema.add_field(
        P,
        FieldDef::data(3, "Override", ty).with_repetitions(repetitions),
    );
    schema.add_field(P, FieldDef::data(4, "Fallback ID", ty));
    schema.add_field(F, FieldDef::data(2, "Key", ty));
    let formula = compile(&schema, text);
    schema.add_field(
        P,
        FieldDef::calculation(2, "Match", ty, formula, false).with_repetitions(repetitions),
    );
    schema.add_relationship(relation(1, TO, 2));
    schema.add_relationship(relation(2, FALLBACK, 4));
    schema
}
fn row(values: &[(u64, u32, Value)]) -> RecordValues {
    let mut row = RecordValues::default();
    for (f, r, v) in values {
        row.set(*f, *r, v.clone());
    }
    row
}
fn related(
    source: &dyn CalcSource,
    schema: &CalcSchema,
    record: u64,
) -> Result<Arc<[u64]>, CalcError> {
    source.related(schema, FROM, record, TO, &Ctx::new(&schema.locale, today()))
}

#[test]
fn a_calculated_source_uses_an_override_or_another_related_records_key() {
    let schema = make_schema(
        ValueType::Text,
        1,
        "Case ( not IsEmpty ( Override ) ; Override ; Fallback::Key )",
    );
    let mut source = MemorySource::new();
    source.put_record(P, 1, row(&[(3, 1, Value::text("B"))]));
    source.put_record(P, 2, row(&[(4, 1, Value::text("f"))]));
    source.put_record(P, 3, RecordValues::default());
    source.put_record(
        F,
        90,
        row(&[(1, 1, Value::text("f")), (2, 1, Value::text("A"))]),
    );
    for (id, key) in [(30, "B"), (10, "A"), (20, "B"), (40, "")] {
        source.put_record(C, id, row(&[(1, 1, Value::text(key))]));
    }
    assert_eq!(&*related(&source, &schema, 1).unwrap(), &[30, 20]);
    assert_eq!(&*related(&source, &schema, 2).unwrap(), &[10]);
    assert!(related(&source, &schema, 3).unwrap().is_empty());
}

#[test]
fn calculated_repetitions_keep_number_date_and_text_types_and_skip_empty_keys() {
    for (ty, a, b) in [
        (ValueType::Number, Value::number(4), Value::number(8)),
        (
            ValueType::Date,
            Value::Date(today()),
            Value::Date(Date::from_ymd(2026, 3, 4).unwrap()),
        ),
        (ValueType::Text, Value::text("A"), Value::text("B")),
    ] {
        let schema = make_schema(ty, 3, "Input");
        let mut source = MemorySource::new();
        source.put_record(P, 1, row(&[(1, 1, a.clone()), (1, 3, b.clone())]));
        source.put_record(C, 10, row(&[(1, 1, b.clone())]));
        source.put_record(C, 20, RecordValues::default());
        source.put_record(C, 30, row(&[(1, 2, a.clone())]));
        let cx = Ctx::new(&schema.locale, today());
        let keys = source
            .source_match_values(&schema, FROM, 1, 2, &cx)
            .unwrap();
        assert!(keys.iter().all(|v| v.value_type() == ty));
        assert_eq!(
            keys.iter()
                .map(|v| v.to_text(&cx).into_string())
                .collect::<Vec<_>>(),
            [a.to_text(&cx).into_string(), b.to_text(&cx).into_string()]
        );
        assert_eq!(&*related(&source, &schema, 1).unwrap(), &[10, 30]);
    }
}

#[test]
fn a_calculated_source_reads_the_sessions_global_field_values() {
    let mut schema = make_schema(ValueType::Text, 1, "Input");
    schema.add_field(
        P,
        FieldDef::data(1, "Input", ValueType::Text).with_global(true),
    );
    let mut source = MemorySource::new();
    source.put_record(P, 1, row(&[(1, 1, Value::text("stale"))]));
    source.set_globals(P, row(&[(1, 1, Value::text("session"))]));
    source.put_record(C, 10, row(&[(1, 1, Value::text("session"))]));
    source.put_record(C, 20, row(&[(1, 1, Value::text("stale"))]));
    assert_eq!(&*related(&source, &schema, 1).unwrap(), &[10]);
    source.set_globals(P, RecordValues::default());
    assert!(related(&source, &schema, 1).unwrap().is_empty());
}

#[test]
fn provider_session_answers_are_available_to_a_source_key() {
    let schema = make_schema(ValueType::Text, 1, "Get ( AccountName )");
    let source = MemorySource::new();
    let session = SessionInfo {
        account_name: "test-account".into(),
        ..Default::default()
    };
    let cx = Ctx::new(&schema.locale, today());
    let keys = source_match_values(&source, &schema, FROM, 1, 2, &cx, Some(&session)).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].to_text(&cx).as_str(), "test-account");
    // No connection metadata is guessed for a source that didn't supply it.
    assert_eq!(
        source
            .source_match_values(&schema, FROM, 1, 2, &cx)
            .unwrap_err()
            .code(),
        1225
    );
}

#[test]
fn normal_let_slots_and_custom_function_parameters_work() {
    let mut schema = make_schema(ValueType::Text, 1, "Input");
    let parameters = vec!["key".into()];
    let body = {
        let mut context = formula::Context::new(schema.names());
        context.parameters = &parameters;
        formula::compile("Upper ( key )", &context).unwrap().expr
    };
    schema.add_custom_function(
        1,
        CustomFunction {
            name: "Normalise".into(),
            parameters,
            body,
        },
    );
    let formula = compile(
        &schema,
        "Let ( [ key = Input ; key = key & Override ] ; Normalise ( key ) )",
    );
    schema.add_field(
        P,
        FieldDef::calculation(2, "Match", ValueType::Text, formula, false),
    );
    let mut source = MemorySource::new();
    source.put_record(
        P,
        1,
        row(&[(1, 1, Value::text("a")), (3, 1, Value::text("b"))]),
    );
    source.put_record(C, 10, row(&[(1, 1, Value::text("AB"))]));
    assert_eq!(&*related(&source, &schema, 1).unwrap(), &[10]);
}

#[test]
fn keys_use_the_formulas_occurrence_and_handle_non_equality_hops() {
    let mut schema = make_schema(ValueType::Number, 1, "Input");
    const ALIAS: u64 = FROM + 10;
    schema.add_occurrence(ALIAS, "Parent alias", P);
    schema.add_relationship(RelationshipDef::new(
        3,
        ALIAS,
        FALLBACK,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 1,
        }],
    ));
    let formula = {
        let mut context = formula::Context::new(schema.names());
        context.occurrence = Some(ALIAS);
        Formula {
            context: Some(Key::from_id(ALIAS)),
            formatted: None,
            polish: formula::compile("Fallback::Key", &context).unwrap().expr,
        }
    };
    schema.add_field(
        P,
        FieldDef::calculation(2, "Match", ValueType::Number, formula, false),
    );
    // A second target lets this check the scan's inequality path as well.
    const GREATER: u64 = TO + 10;
    schema.add_occurrence(GREATER, "Greater child", C);
    schema.add_relationship(RelationshipDef::new(
        4,
        FROM,
        GREATER,
        vec![JoinPredicate {
            op: JoinOp::Less,
            left: 2,
            right: 1,
        }],
    ));
    let mut source = MemorySource::new();
    source.put_record(
        P,
        1,
        row(&[(1, 1, Value::number(9)), (4, 1, Value::number(8))]),
    );
    source.put_record(
        F,
        90,
        row(&[(1, 1, Value::number(9)), (2, 1, Value::number(5))]),
    );
    source.put_record(
        F,
        91,
        row(&[(1, 1, Value::number(8)), (2, 1, Value::number(7))]),
    );
    source.put_record(C, 10, row(&[(1, 1, Value::number(5))]));
    source.put_record(C, 20, row(&[(1, 1, Value::number(7))]));
    assert_eq!(&*related(&source, &schema, 1).unwrap(), &[10]);
    let cx = Ctx::new(&schema.locale, today());
    assert_eq!(
        &*source.related(&schema, FROM, 1, GREATER, &cx).unwrap(),
        &[20]
    );
}

#[test]
fn unavailable_session_or_dynamic_context_is_refused_before_evaluation() {
    let source = MemorySource::new();
    for expression in [
        "Case ( IsEmpty ( $$key ) ; \"fallback\" ; $$key )",
        "Let ( $key = \"A\" ; $key )",
        "Let ( $$key = \"A\" ; $$key )",
        "Get ( WindowMode )",
        "Get ( ScriptParameter )",
        "If ( 1 ; \"A\" ; Get ( WindowName ) )",
        "Evaluate ( \"$$key\" )",
        "GetField ( \"Input\" )",
        "ExecuteSQL ( \"SELECT Input FROM Parent\" ; \"\" ; \"\" )",
        "WindowNames ( \"\" )",
    ] {
        let schema = make_schema(ValueType::Text, 1, expression);
        let cx = Ctx::new(&schema.locale, today());
        // Supplying connection metadata does not invent a script/window.
        assert_eq!(
            source_match_values(
                &source,
                &schema,
                FROM,
                1,
                2,
                &cx,
                Some(&SessionInfo::default())
            )
            .unwrap_err()
            .code(),
            1225,
            "{expression}"
        );
    }
}

#[test]
fn unavailable_context_inside_nested_fields_and_functions_is_also_refused() {
    let mut schema = make_schema(ValueType::Text, 1, "$$key");
    let nested = compile(&schema, "Match");
    schema.add_field(
        P,
        FieldDef::calculation(5, "Nested", ValueType::Text, nested, false),
    );
    let hidden = compile(&schema, "If ( IsValid ( Nested ) ; Nested ; \"A\" )");
    schema.add_field(
        P,
        FieldDef::calculation(6, "Hidden", ValueType::Text, hidden, false),
    );
    let source = MemorySource::new();
    let locale = schema.locale.clone();
    let cx = Ctx::new(&locale, today());
    assert_eq!(
        source
            .source_match_values(&schema, FROM, 1, 6, &cx)
            .unwrap_err()
            .code(),
        1225
    );
    schema.add_custom_function(
        1,
        CustomFunction {
            name: "Hidden key".into(),
            parameters: vec![],
            body: compile(&schema, "Get ( WindowMode )").polish,
        },
    );
    let formula = compile(&schema, "Hidden key ( )");
    schema.add_field(
        P,
        FieldDef::calculation(2, "Match", ValueType::Text, formula, false),
    );
    assert_eq!(
        source
            .source_match_values(&schema, FROM, 1, 2, &cx)
            .unwrap_err()
            .code(),
        1225
    );
}

#[test]
fn pending_and_failed_data_never_become_match_values() {
    let schema = make_schema(
        ValueType::Text,
        1,
        "While ( [ n = 0 ] ; IsEmpty ( Fallback::Key ) ; [ n = n + 1 ] ; Input )",
    );
    let mut data = CalcData::default();
    data.records
        .insert((P, 1), Some(Arc::new(row(&[(1, 1, Value::text("A"))]))));
    let cache = RefCell::new(data);
    let need = CalcNeed::RelatedIds {
        from: FROM,
        record: 1,
        to: FALLBACK,
    };
    let failed = BTreeSet::new();
    let source = CachedSource::new(&cache).retrying(&failed);
    let cx = Ctx::new(&schema.locale, today());
    assert_eq!(
        source
            .source_match_values(&schema, FROM, 1, 2, &cx)
            .unwrap_err()
            .code(),
        1224
    );
    assert_eq!(source.missing(), [need]);
    let object: &dyn CalcSource = &source;
    let recorder = Recorder::new(object);
    assert_eq!(
        recorder
            .source_match_values(&schema, FROM, 1, 2, &cx)
            .unwrap_err()
            .code(),
        1224
    );
    assert!(recorder.pending().reads > 0);
    // A failed read still is an error, and cannot turn into a literal '?'
    // key that matches a target record. Use a direct read so it needn't loop.
    let failed_schema = make_schema(ValueType::Text, 1, "Fallback::Key");
    let failed = source.missing().into_iter().collect::<BTreeSet<_>>();
    let source = CachedSource::new(&cache).retrying(&failed);
    assert!(source
        .source_match_values(&failed_schema, FROM, 1, 2, &cx)
        .is_err());
    assert_eq!(source.pending(), Default::default());
    // Once the requested related set and row arrive, the same loop
    // completes normally; the pending pass did not poison the key.
    cache
        .borrow_mut()
        .related
        .insert((FROM, 1, FALLBACK), Arc::from([90]));
    cache.borrow_mut().records.insert(
        (F, 90),
        Some(Arc::new(row(&[(2, 1, Value::text("ready"))]))),
    );
    let complete = CachedSource::new(&cache).retrying(&failed);
    let keys = complete
        .source_match_values(&schema, FROM, 1, 2, &cx)
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].to_text(&cx).as_str(), "A");
    assert!(complete.missing().is_empty());
}

struct Counted {
    rows: MemorySource,
    reads: Cell<usize>,
}
impl CalcSource for Counted {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        self.reads.set(self.reads.get() + 1);
        assert!(
            self.reads.get() < 200,
            "a circular relationship key must be bounded"
        );
        self.rows.record(table, record)
    }
    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.rows.records(table)
    }
    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        self.rows.globals(table)
    }
}

#[test]
fn a_circular_key_is_bounded_instead_of_overflowing_the_stack() {
    let schema = make_schema(ValueType::Text, 1, "Child::Input");
    let mut rows = MemorySource::new();
    rows.put_record(P, 1, RecordValues::default());
    rows.put_record(C, 10, row(&[(1, 1, Value::text("A"))]));
    let source = Counted {
        rows,
        reads: Cell::new(0),
    };
    assert!(related_by_scan(
        &source,
        &schema,
        FROM,
        1,
        TO,
        &Ctx::new(&schema.locale, today())
    )
    .is_err());
    assert!(source.reads.get() < 100);
}

#[test]
fn unread_catalogs_suspend_calculated_keys_and_sort_levels() {
    use om_data::specs::{SortEntry, SortSpec, SpecField};
    let full = make_schema(
        ValueType::Text,
        1,
        "If ( IsValid ( Fallback::Key ) ; Fallback::Key ; Input )",
    );
    let mut schema = CalcSchema::new("Pending fields", Locale::en_us());
    schema.add_table(P, "Parent");
    schema.add_occurrence(FROM, "Parent", P);
    schema.add_field(P, full.field(FROM, 1).unwrap().clone());
    schema.add_field(P, full.field(FROM, 2).unwrap().clone());
    schema.add_table(F, "Fallback");
    schema.add_occurrence(FALLBACK, "Fallback", F);
    schema.set_fields_pending(F);
    let data = RefCell::new(CalcData::default());
    let source = CachedSource::new(&data);
    let cx = Ctx::new(&schema.locale, today());
    assert_eq!(
        source_match_values(&source, &schema, FROM, 1, 2, &cx, None)
            .unwrap_err()
            .code(),
        1224
    );
    assert_eq!(
        source.missing(),
        [CalcNeed::Occurrence {
            occurrence: FALLBACK
        }]
    );
    let source = CachedSource::new(&data);
    let spec = SortSpec {
        keep_sorted: false,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(FALLBACK),
                field: 2,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        }],
    };
    assert_eq!(
        om_data::host::sort_records(&source, &schema, FROM, vec![2, 1], &spec, &cx)
            .unwrap_err()
            .code(),
        1224
    );
    assert_eq!(
        source.missing(),
        [CalcNeed::Occurrence {
            occurrence: FALLBACK
        }]
    );
}
