//! A portal pins intermediate relationship hops to its displayed row.
//! Aggregates over the portal itself still see the parent's entire set.
use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::{Date, Locale, Value, ValueType};
use om_data::formula::{self, Formula, Key};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc,
    FileHost, JoinOp, JoinPredicate, MemorySource, RecordValues, RelationshipDef, SessionInfo,
    SummaryDef, SummaryKind,
};
use om_data::specs::{SortEntry, SortSpec, SpecField};
use std::cell::RefCell;

const P: u64 = 101;
const C: u64 = 102;
const G: u64 = 103;
const O: u64 = 104;
const H: u64 = 105;

fn fixture() -> (CalcSchema, MemorySource) {
    let mut schema = CalcSchema::new("Portal paths", Locale::en_us());
    for (id, name) in [
        (P, "Parents"),
        (C, "Lines"),
        (G, "Matches"),
        (O, "Other"),
        (H, "Details"),
    ] {
        schema.add_table(id, name);
        schema.add_occurrence(id, name, id);
        schema.add_field(id, FieldDef::data(1, "Key", ValueType::Number));
        schema.add_field(id, FieldDef::data(2, "Parent", ValueType::Number));
        schema.add_field(id, FieldDef::data(3, "Amount", ValueType::Number));
        schema.add_field(id, FieldDef::data(4, "Label", ValueType::Text));
    }
    schema.add_field(
        G,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::Total, 3)),
            ..FieldDef::data(5, "Total", ValueType::Number)
        },
    );
    let mut names = formula::Context::new(schema.names());
    names.occurrence = Some(G);
    let calc = Formula {
        context: Some(Key::from_id(G)),
        formatted: None,
        polish: formula::compile("Total + 0", &names).unwrap().expr,
    };
    schema.add_field(
        G,
        FieldDef::calculation(6, "Nested", ValueType::Number, calc, false),
    );
    for (id, from, to, left, right) in [
        (1, P, C, 1, 2),
        (2, C, G, 1, 1),
        (3, P, O, 1, 2),
        (4, G, H, 1, 2),
    ] {
        let mut relationship = RelationshipDef::new(
            id,
            from,
            to,
            vec![JoinPredicate {
                op: JoinOp::Equal,
                left,
                right,
            }],
        );
        if id == 2 {
            relationship.sort_right = Some(SortSpec {
                keep_sorted: true,
                blanks_last: false,
                fields: vec![SortEntry {
                    field: SpecField {
                        occurrence: Some(G),
                        field: 4,
                        repetition: 1,
                    },
                    summary: None,
                    value_list: None,
                    descending: false,
                    locale: 0,
                }],
            });
        }
        schema.add_relationship(relationship);
    }
    let mut source = MemorySource::new();
    for (table, id, key, parent, amount, label) in [
        (P, 1, 10, 0, 0, "parent one"),
        (P, 2, 20, 0, 0, "parent two"),
        (C, 11, 101, 10, 0, "line one"),
        (C, 22, 202, 10, 0, "line two"),
        (C, 33, 303, 20, 0, "unrelated line"),
        (G, 101, 101, 0, 5, "alpha"),
        (G, 202, 202, 0, 20, "beta"),
        (G, 303, 303, 0, 99, "outside"),
        (O, 41, 1, 10, 1, "other one"),
        (O, 42, 2, 10, 2, "other two"),
        (H, 51, 1, 101, 1, "detail one"),
        (H, 52, 2, 202, 2, "detail two"),
        (H, 53, 3, 303, 3, "outside detail"),
    ] {
        put(&mut source, table, id, key, parent, amount, label);
    }
    (schema, source)
}
fn put(
    source: &mut MemorySource,
    table: u64,
    id: u64,
    key: i64,
    parent: i64,
    amount: i64,
    label: &str,
) {
    let mut row = RecordValues::default();
    for (field, value) in [
        (1, Value::number(key)),
        (2, Value::number(parent)),
        (3, Value::number(amount)),
        (4, Value::text(label)),
    ] {
        row.set(field, 1, value);
    }
    source.put_record(table, id, row);
}
fn read(schema: &CalcSchema, source: &dyn CalcSource, context: &Context, text: &str) -> String {
    let (mut local, mut global) = (Variables::new(), Variables::new());
    let info = SessionInfo::default();
    let mut host = FileHost::new(
        FileCalc::new(schema, source, Date::from_ymd(2026, 1, 2).unwrap()),
        &info,
        &mut local,
        &mut global,
    );
    let result = evaluate_text(text, &mut host, context);
    assert!(result.error.is_none(), "{text}: {:?}", result.error);
    display(&result.value, &host)
}
#[test]
fn intermediate_portal_hop_is_row_scoped_but_portal_aggregate_is_not() {
    let (schema, source) = fixture();
    let parent = Context::record(P, 1);
    assert_eq!(
        read(&schema, &source, &parent, "Count ( Matches::Key )"),
        "2"
    );
    for row in [11, 22] {
        let context = parent.clone().with_portal(C, row);
        assert_eq!(
            read(&schema, &source, &context, "Count ( Matches::Key )"),
            "1"
        );
        assert_eq!(
            read(&schema, &source, &context, "Count ( Lines::Key )"),
            "2"
        );
        assert_eq!(
            read(&schema, &source, &context, "Count ( Other::Key )"),
            "2"
        );
        assert_eq!(
            read(&schema, &source, &context, "Parents::Label"),
            "parent one"
        );
        assert_eq!(
            read(&schema, &source, &context, "Count ( Details::Key )"),
            "1"
        );
    }
    let second = parent.with_portal(C, 22);
    assert_eq!(read(&schema, &source, &second, "Matches::Label"), "beta");
    assert_eq!(
        read(&schema, &source, &second, "List ( Matches::Label )"),
        "beta"
    );
    assert_eq!(
        read(
            &schema,
            &source,
            &second,
            "GetNthRecord ( Matches::Label ; 1 )"
        ),
        "beta"
    );
    assert_eq!(
        read(&schema, &source, &second, "Lookup ( Matches::Label )"),
        "beta"
    );
}
#[test]
fn duplicate_warning_and_related_summaries_are_scoped_to_each_row() {
    let (schema, mut source) = fixture();
    put(&mut source, G, 102, 101, 0, 7, "alpha extra");
    let parent = Context::record(P, 1);
    let first = parent.clone().with_portal(C, 11);
    let second = parent.clone().with_portal(C, 22);
    assert_eq!(
        read(&schema, &source, &first, "Count ( Matches::Key ) > 1"),
        "1"
    );
    assert_eq!(
        read(&schema, &source, &second, "Count ( Matches::Key ) > 1"),
        "0"
    );
    for text in [
        "Sum ( Matches::Amount )",
        "Matches::Total",
        "Matches::Nested",
    ] {
        assert_eq!(read(&schema, &source, &first, text), "12", "{text}");
        assert_eq!(read(&schema, &source, &second, text), "20", "{text}");
        assert_eq!(read(&schema, &source, &parent, text), "32", "{text}");
    }
    assert_eq!(
        read(
            &schema,
            &source,
            &first,
            "GetNthRecord ( Matches::Label ; 2 )"
        ),
        "alpha extra"
    );
}
#[test]
fn missing_data_requests_only_the_portal_rows_downstream_set() {
    let (schema, _) = fixture();
    let cache = RefCell::new(CalcData::default());
    let source = CachedSource::new(&cache);
    let context = Context::record(P, 1).with_portal(C, 22);
    let (mut local, mut global) = (Variables::new(), Variables::new());
    let info = SessionInfo::default();
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, Date::from_ymd(2026, 1, 2).unwrap()),
        &info,
        &mut local,
        &mut global,
    );
    let _ = evaluate_text("Count ( Matches::Key )", &mut host, &context);
    let needs = source.missing();
    assert!(
        needs.iter().any(|need| matches!(
            need,
            CalcNeed::RelatedIds {
                from: C,
                record: 22,
                to: G
            }
        )),
        "{needs:?}"
    );
    assert!(
        !needs
            .iter()
            .any(|need| matches!(need, CalcNeed::RelatedIds { from: P, to: G, .. })),
        "{needs:?}"
    );
}

#[test]
fn get_summary_selects_its_break_group_in_the_portal_rows_downstream_set() {
    let (mut schema, mut source) = fixture();
    let mut running = SummaryDef::new(SummaryKind::Total, 3);
    running.flags = om_data::meta::summary_flag::RUNNING;
    schema.add_field(
        G,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(running),
            ..FieldDef::data(7, "Running", ValueType::Number)
        },
    );
    put(&mut source, G, 204, 202, 0, 30, "beta");
    let parent = Context::record(P, 1);
    for text in [
        "GetSummary ( Matches::Total ; Matches::Label )",
        "GetSummary ( Matches::Running ; Matches::Label )",
    ] {
        assert_eq!(read(&schema, &source, &parent, text), "5", "{text}");
        assert_eq!(
            read(&schema, &source, &parent.clone().with_portal(C, 11), text),
            "5",
            "{text} on row 1"
        );
        assert_eq!(
            read(&schema, &source, &parent.clone().with_portal(C, 22), text),
            "50",
            "{text} on row 2"
        );
    }
}

#[test]
fn a_downstream_occurrence_of_the_same_table_uses_the_displayed_row_key() {
    let (mut schema, mut source) = fixture();
    const MATCHING_LINES: u64 = 106;
    schema.add_occurrence(MATCHING_LINES, "Matching lines", C);
    schema.add_relationship(RelationshipDef::new(
        5,
        C,
        MATCHING_LINES,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: 1,
            right: 1,
        }],
    ));
    put(
        &mut source,
        C,
        44,
        101,
        20,
        0,
        "duplicate on another parent",
    );
    let parent = Context::record(P, 1);
    let first = parent.clone().with_portal(C, 11);
    let second = parent.clone().with_portal(C, 22);
    assert_eq!(
        read(&schema, &source, &first, "Count ( Matching lines::Key )"),
        "2"
    );
    assert_eq!(
        read(&schema, &source, &second, "Count ( Matching lines::Key )"),
        "1"
    );
    assert_eq!(
        read(&schema, &source, &second, "Matching lines::Label"),
        "line two"
    );
    assert_eq!(read(&schema, &source, &second, "Count ( Lines::Key )"), "2");
}
