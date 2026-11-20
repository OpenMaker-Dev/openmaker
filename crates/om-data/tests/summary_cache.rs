//! Summary reuse is scoped to an immutable calculation pass, with record-
//! dependent results still evaluated for the requested record and repetition.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use om_data::calc::eval::{display, evaluate_text, Context, FieldId, Variables};
use om_data::calc::{CalcError, Ctx, Date, Locale, Value, ValueType};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc,
    FileHost, MemorySource, RecordValues, Recorder, RelationshipDef, SessionInfo, SummaryCache,
    SummaryDef, SummaryKind,
};
use om_data::meta::{summary_flag, SummaryField};
use om_data::specs::{SortEntry, SortSpec, SpecField};

const TABLE: u64 = 0x8001;
const OCCURRENCE: u64 = 0xd00001;
const OTHER_OCCURRENCE: u64 = 0xd00002;

struct CountingSource {
    data: MemorySource,
    reads: Cell<usize>,
    fail_once: Cell<bool>,
}

impl CountingSource {
    fn new() -> Self {
        let mut data = MemorySource::new();
        for (id, amount, group, weight) in [(1, 10, "A", 1), (2, 20, "A", 2), (3, 60, "B", 3)] {
            let mut row = RecordValues::default();
            row.set(1, 1, Value::number(amount));
            row.set(2, 1, Value::text(group));
            row.set(3, 1, Value::number(weight));
            row.set(4, 1, Value::number(amount));
            row.set(4, 2, Value::number(amount / 10));
            data.put_record(TABLE, id, row);
        }
        data.set_found_set(OCCURRENCE, &[1, 2, 3], Some(&[2]));
        data.set_found_set(OTHER_OCCURRENCE, &[2], None);
        Self {
            data,
            reads: Cell::new(0),
            fail_once: Cell::new(false),
        }
    }
}

impl CalcSource for CountingSource {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        self.reads.set(self.reads.get() + 1);
        if self.fail_once.replace(false) {
            return Err(CalcError::from_code(101).unwrap());
        }
        self.data.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.data.records(table)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        self.data.globals(table)
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.data.found_set(occurrence, table)
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.data.found_sort(occurrence)
    }
}

fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Summary cache", Locale::en_us());
    schema.add_table(TABLE, "Rows");
    schema.add_occurrence(OCCURRENCE, "Rows", TABLE);
    schema.add_occurrence(OTHER_OCCURRENCE, "Other rows", TABLE);
    schema.add_field(TABLE, FieldDef::data(1, "Amount", ValueType::Number));
    schema.add_field(TABLE, FieldDef::data(2, "Group", ValueType::Text));
    schema.add_field(TABLE, FieldDef::data(3, "Weight", ValueType::Number));
    schema.add_field(
        TABLE,
        FieldDef::data(4, "Repetitions", ValueType::Number).with_repetitions(2),
    );
    for (id, kind, flags, field, additional) in [
        (10, SummaryKind::Total, 0, 1, None),
        (11, SummaryKind::Total, summary_flag::RUNNING, 1, None),
        (12, SummaryKind::Fraction, 0, 1, None),
        (
            13,
            SummaryKind::Total,
            summary_flag::RUNNING | summary_flag::RESTART,
            1,
            Some(2),
        ),
        (14, SummaryKind::Fraction, summary_flag::OPTION, 1, Some(2)),
        (
            15,
            SummaryKind::Total,
            summary_flag::INDIVIDUAL_REPETITIONS,
            4,
            None,
        ),
        (16, SummaryKind::Average, summary_flag::OPTION, 1, Some(3)),
    ] {
        let mut summary = SummaryDef::new(kind, field);
        summary.flags = flags;
        summary.additional = additional.map(|field| SummaryField {
            occurrence: Some(OCCURRENCE),
            field,
            repetition: 1,
        });
        schema.add_field(
            TABLE,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(summary),
                ..FieldDef::data(id, format!("Summary {id}"), ValueType::Number)
                    .with_repetitions(if id == 15 { 2 } else { 1 })
            },
        );
    }
    schema
}

fn today(day: u32) -> Date {
    Date::from_ymd(2026, 10, day).unwrap()
}

const PARENTS: u64 = 0x8101;
const PARENT: u64 = 0xd00101;
const CHILDREN: u64 = 0x8102;
const CHILD: u64 = 0xd00102;

/// A child row: its ID, Amount and Group.
type Row = (u64, i64, &'static str);

/// Parents related to Children (1000 rows of Amount 1) sorted by Group:
/// parent 1 to rows 10 (A, 3) and 20 (B, 7), parent 2 to rows 30 and 40
/// (B, 100 and 200).
const RELATED: [(u64, &[Row]); 2] = [
    (1, &[(10, 3, "A"), (20, 7, "B")]),
    (2, &[(30, 100, "B"), (40, 200, "B")]),
];

/// Summaries of Children: a total (10), running (11), running restarted by
/// Group (12) and fraction (13) of Amount, and an unstored calculation of
/// the total (14).
fn related_schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Related summaries", Locale::en_us());
    schema.add_table(PARENTS, "Parents");
    schema.add_occurrence(PARENT, "Parents", PARENTS);
    schema.add_table(CHILDREN, "Children");
    schema.add_occurrence(CHILD, "Children", CHILDREN);
    schema.add_field(CHILDREN, FieldDef::data(1, "Amount", ValueType::Number));
    schema.add_field(CHILDREN, FieldDef::data(2, "Group", ValueType::Text));
    let group = SummaryField {
        occurrence: Some(CHILD),
        field: 2,
        repetition: 1,
    };
    for (id, name, kind, flags, additional) in [
        (10, "Total", SummaryKind::Total, 0, None),
        (
            11,
            "Running",
            SummaryKind::Total,
            summary_flag::RUNNING,
            None,
        ),
        (
            12,
            "Restarted",
            SummaryKind::Total,
            summary_flag::RUNNING | summary_flag::RESTART,
            Some(group),
        ),
        (13, "Fraction", SummaryKind::Fraction, 0, None),
    ] {
        let mut def = SummaryDef::new(kind, 1);
        def.flags = flags;
        def.additional = additional;
        schema.add_field(
            CHILDREN,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(def),
                ..FieldDef::data(id, name, ValueType::Number)
            },
        );
    }
    let mut names = om_data::formula::Context::new(schema.names());
    names.occurrence = Some(CHILD);
    let formula = om_data::formula::Formula {
        context: Some(om_data::formula::Key::from_id(CHILD)),
        formatted: None,
        polish: om_data::formula::compile("Total", &names).unwrap().expr,
    };
    schema.add_field(
        CHILDREN,
        FieldDef::calculation(14, "Calculated", ValueType::Number, formula, false),
    );
    let mut relationship = RelationshipDef::new(1, PARENT, CHILD, vec![]);
    relationship.sort_right = Some(SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(CHILD),
                field: 2,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        }],
    });
    schema.add_relationship(relationship);
    schema
}

fn child_row(amount: i64, group: &str) -> RecordValues {
    let mut row = RecordValues::default();
    row.set(1, 1, Value::number(amount));
    row.set(2, 1, Value::text(group));
    row
}

/// Every child, the window's found set of all of them sorted by Amount,
/// and the related sets an index answers. It records which children are
/// read and counts every found set and table scan.
struct Children {
    data: MemorySource,
    related: HashMap<u64, Arc<[u64]>>,
    reads: RefCell<BTreeSet<u64>>,
    scans: Cell<usize>,
}

impl Children {
    fn new() -> Self {
        let mut data = MemorySource::new();
        for id in 1..=1000 {
            data.put_record(CHILDREN, id, child_row(1, "C"));
        }
        let mut related = HashMap::new();
        for (parent, rows) in RELATED {
            for &(id, amount, group) in rows {
                data.put_record(CHILDREN, id, child_row(amount, group));
            }
            related.insert(parent, rows.iter().map(|row| row.0).collect());
        }
        let all: Vec<u64> = (1..=1000).collect();
        data.set_found_set(CHILD, &all, Some(&[1]));
        Self {
            data,
            related,
            reads: RefCell::default(),
            scans: Cell::new(0),
        }
    }

    fn take_reads(&self) -> Vec<u64> {
        std::mem::take(&mut *self.reads.borrow_mut())
            .into_iter()
            .collect()
    }
}

impl CalcSource for Children {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        if table == CHILDREN {
            self.reads.borrow_mut().insert(record);
        }
        self.data.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.scans.set(self.scans.get() + 1);
        self.data.records(table)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        self.data.globals(table)
    }

    fn found_set(&self, occurrence: u64, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.scans.set(self.scans.get() + 1);
        self.data.found_set(occurrence, table)
    }

    fn found_sort(&self, occurrence: u64) -> Option<Arc<[u64]>> {
        self.data.found_sort(occurrence)
    }

    fn related(
        &self,
        _schema: &CalcSchema,
        from: u64,
        record: u64,
        to: u64,
        _cx: &Ctx,
    ) -> Result<Arc<[u64]>, CalcError> {
        assert_eq!((from, to), (PARENT, CHILD));
        Ok(Arc::clone(&self.related[&record]))
    }
}

/// `text` evaluated in `context` through a host over `file`, as displayed.
fn evaluated(file: FileCalc<'_>, text: &str, context: &Context) -> String {
    let session = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(file, &session, &mut locals, &mut globals);
    let result = evaluate_text(text, &mut host, context);
    assert_eq!(result.code(), 0, "{text}");
    display(&result.value, &host)
}

#[test]
fn related_summaries_read_only_the_parent_related_set() {
    let schema = related_schema();
    let source = Children::new();
    let summaries = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&summaries);
    let read = |text: &str, context: &Context| evaluated(file, text, context);
    let (first, second) = (Context::record(PARENT, 1), Context::record(PARENT, 2));

    assert_eq!(read("Children::Total", &first), "10");
    assert_eq!(source.take_reads(), [10, 20]);
    assert_eq!(read("Children::Total", &second), "300");
    assert_eq!(source.take_reads(), [30, 40]);
    // The pass's cache keeps each parent's set apart.
    assert_eq!(read("Children::Total", &first), "10");
    assert_eq!(read("Children::Calculated", &second), "300");

    // Each portal row is a position in its parent's set, which the
    // relationship sorts by Group rather than the window by Amount.
    let row = |record| first.clone().with_portal(CHILD, record);
    for (text, on_10, on_20) in [
        ("Children::Running", "3", "10"),
        ("Children::Restarted", "3", "7"),
        ("Children::Fraction", ".3", ".7"),
        ("Children::Calculated", "10", "10"),
        ("GetSummary ( Children::Total ; Children::Group )", "3", "7"),
        (
            "GetSummary ( Children::Total ; Children::Total )",
            "10",
            "10",
        ),
    ] {
        assert_eq!(read(text, &row(10)), on_10, "{text} on 10");
        assert_eq!(read(text, &row(20)), on_20, "{text} on 20");
    }
    // Without a portal row the first related record is the one read.
    assert_eq!(
        read("GetSummary ( Children::Total ; Children::Group )", &first),
        "3"
    );
    assert_eq!(read("Children::Running", &second), "100");
    assert_eq!(source.scans.get(), 0, "no found set or table is read");
    assert_eq!(source.take_reads(), [10, 20, 30, 40], "only related rows");

    // The table's own records still summarize the window's found set.
    let window = Context::record(CHILD, 1);
    assert_eq!(read("Total", &window), "1306");
    assert_eq!(read("Calculated", &window), "1306");
    assert_eq!(read("Children::Total", &first), "10");
    assert_eq!(source.scans.get(), 1);
    assert_eq!(source.take_reads().len(), 1000);
}

#[test]
fn related_summaries_request_their_missing_rows_in_one_batch() {
    let schema = related_schema();
    let data = RefCell::new(CalcData::default());
    for (parent, rows) in RELATED {
        let ids: Arc<[u64]> = rows.iter().map(|row| row.0).collect();
        data.borrow_mut()
            .related_ids
            .insert((PARENT, parent, CHILD), ids);
    }
    let summaries = SummaryCache::default();
    let first = Context::record(PARENT, 1);
    let source = CachedSource::new(&data);
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&summaries);
    let session = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(file, &session, &mut locals, &mut globals);
    let result = evaluate_text("Children::Total", &mut host, &first);
    assert!(result.value.is_invalid());
    assert_eq!(
        source.missing(),
        [10, 20].map(|record| CalcNeed::Record {
            table: CHILDREN,
            record,
        }),
        "both related rows at once; no found set, table or unrelated row"
    );

    for (_, rows) in RELATED {
        for &(id, amount, group) in rows {
            let row = Some(Arc::new(child_row(amount, group)));
            data.borrow_mut().records.insert((CHILDREN, id), row);
        }
    }
    summaries.clear();
    let source = CachedSource::new(&data);
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&summaries);
    let read = |text: &str, context: &Context| evaluated(file, text, context);
    assert_eq!(read("Children::Total", &first), "10");
    assert_eq!(read("Children::Total", &Context::record(PARENT, 2)), "300");
    assert_eq!(
        read("Children::Restarted", &first.clone().with_portal(CHILD, 20)),
        "7"
    );
    assert!(!source.has_missing());
}

fn value(file: &FileCalc<'_>, occurrence: u64, record: u64, field: u64, rep: u32) -> String {
    file.field_value(occurrence, record, field, rep)
        .unwrap()
        .to_text(&file.cx())
        .into_string()
}

#[test]
fn repeated_totals_read_the_found_set_once_per_pass() {
    let schema = schema();
    let source = CountingSource::new();
    let cache = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    for record in [1, 2, 3, 1, 3, 999] {
        assert_eq!(value(&file, OCCURRENCE, record, 10, 1), "90");
    }
    assert_eq!(source.reads.get(), 3);

    // A different occurrence has its own found set, even over the same table.
    assert_eq!(value(&file, OTHER_OCCURRENCE, 2, 10, 1), "20");
    assert_eq!(source.reads.get(), 4);
    cache.clear();
    assert_eq!(value(&file, OCCURRENCE, 1, 10, 1), "90");
    assert_eq!(source.reads.get(), 7);
}

#[test]
fn running_fraction_weighted_and_repeating_summaries_keep_their_context() {
    let schema = schema();
    let source = CountingSource::new();
    let cache = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    let plain = FileCalc::new(&schema, &source, today(29));
    for field in [11, 12, 13, 14, 16] {
        for record in [3, 1, 2, 3, 999] {
            assert_eq!(
                value(&file, OCCURRENCE, record, field, 1),
                value(&plain, OCCURRENCE, record, field, 1),
                "field {field}, record {record}"
            );
        }
    }
    assert_eq!(value(&file, OCCURRENCE, 1, 11, 1), "10");
    assert_eq!(value(&file, OCCURRENCE, 2, 11, 1), "30");
    assert_eq!(value(&file, OCCURRENCE, 3, 11, 1), "90");
    assert_eq!(value(&file, OCCURRENCE, 3, 13, 1), "60");
    assert_eq!(value(&file, OCCURRENCE, 3, 14, 1), "1");
    assert_eq!(value(&file, OCCURRENCE, 1, 15, 1), "90");
    assert_eq!(value(&file, OCCURRENCE, 3, 15, 2), "9");
    assert_eq!(value(&file, OCCURRENCE, 2, 15, 1), "90");
    assert_eq!(value(&file, OCCURRENCE, 2, 15, 2), "9");
}

#[test]
fn running_and_fraction_inputs_are_read_once() {
    let schema = schema();
    let source = CountingSource::new();
    let cache = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    for record in [1, 2, 3, 2, 1] {
        value(&file, OCCURRENCE, record, 11, 1);
        value(&file, OCCURRENCE, record, 12, 1);
    }
    assert_eq!(source.reads.get(), 6);
}

#[test]
fn get_summary_uses_each_requested_break_group() {
    let schema = schema();
    let source = CountingSource::new();
    let cache = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    let summary = FieldId {
        occurrence: OCCURRENCE,
        field: 10,
    };
    let group = FieldId {
        occurrence: OCCURRENCE,
        field: 2,
    };
    for (record, expected) in [(1, "30"), (2, "30"), (3, "60")] {
        let value = file
            .summary(&Context::record(OCCURRENCE, record), summary, group)
            .unwrap();
        assert_eq!(value.to_text(&file.cx()).into_string(), expected);
    }
    assert_eq!(value(&file, OCCURRENCE, 2, 10, 1), "90");
    assert_eq!(
        file.group_summaries(summary, group)
            .unwrap()
            .iter()
            .map(|v| v.to_text(&file.cx()).into_string())
            .collect::<Vec<_>>(),
        ["30", "30", "60"]
    );
}

#[test]
fn numeric_empty_and_zero_remain_distinct_summary_groups() {
    let schema = schema();
    let mut source = CountingSource::new();
    for (id, amount, key) in [
        (1, 10, Value::Empty(ValueType::Number)),
        (2, 20, Value::number(0)),
    ] {
        let mut row = RecordValues::default();
        row.set(1, 1, Value::number(amount));
        row.set(3, 1, key);
        source.data.put_record(TABLE, id, row);
    }
    source.data.set_found_set(OCCURRENCE, &[1, 2], Some(&[3]));
    let file = FileCalc::new(&schema, &source, today(29));
    let groups = file.found_groups(OCCURRENCE, &[3]).unwrap();
    assert_eq!(groups.breaks, [Some((0, vec![0, 1]))]);
    let totals = file
        .group_summaries(
            FieldId {
                occurrence: OCCURRENCE,
                field: 10,
            },
            FieldId {
                occurrence: OCCURRENCE,
                field: 3,
            },
        )
        .unwrap();
    assert_eq!(
        totals
            .iter()
            .map(|v| v.to_text(&file.cx()).into_string())
            .collect::<Vec<_>>(),
        ["10", "20"]
    );
}

#[test]
fn a_new_pass_observes_changed_data_found_set_and_sort() {
    let schema = schema();
    let mut source = CountingSource::new();
    let cache = SummaryCache::default();
    {
        let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
        assert_eq!(value(&file, OCCURRENCE, 2, 10, 1), "90");
        assert_eq!(value(&file, OCCURRENCE, 3, 13, 1), "60");
    }
    let mut row = RecordValues::default();
    row.set(1, 1, Value::number(100));
    row.set(2, 1, Value::text("B"));
    source.data.put_record(TABLE, 3, row);
    source.data.set_found_set(OCCURRENCE, &[3, 2], None);
    cache.clear();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    assert_eq!(value(&file, OCCURRENCE, 3, 10, 1), "120");
    assert_eq!(value(&file, OCCURRENCE, 2, 13, 1), "120");
}

#[test]
fn one_cache_does_not_mix_distinct_sources() {
    let schema = schema();
    let a = CountingSource::new();
    let mut b = CountingSource::new();
    b.data.set_found_set(OCCURRENCE, &[3], None);
    let cache = SummaryCache::default();
    let fa = FileCalc::new(&schema, &a, today(29)).with_summary_cache(&cache);
    let fb = FileCalc::new(&schema, &b, today(29)).with_summary_cache(&cache);
    assert_eq!(value(&fa, OCCURRENCE, 1, 10, 1), "90");
    assert_eq!(value(&fb, OCCURRENCE, 3, 10, 1), "60");
    assert_eq!(value(&fa, OCCURRENCE, 2, 10, 1), "90");
    assert_eq!(a.reads.get(), 3);
    assert_eq!(b.reads.get(), 1);
}

#[test]
fn failed_reads_are_not_cached_as_completed_summaries() {
    let schema = schema();
    let source = CountingSource::new();
    source.fail_once.set(true);
    let cache = SummaryCache::default();
    let file = FileCalc::new(&schema, &source, today(29)).with_summary_cache(&cache);
    assert!(file.field_value(OCCURRENCE, 1, 10, 1).is_err());
    assert_eq!(value(&file, OCCURRENCE, 1, 10, 1), "90");
    assert_eq!(value(&file, OCCURRENCE, 3, 10, 1), "90");
    assert_eq!(source.reads.get(), 4);
}

/// A summary of an unstored calculation totals its inputs as the host's
/// own calculations read them, with the host's variables (#615): read as
/// a field and through `GetSummary`. Without variables, as before, the
/// inputs read none.
#[test]
fn summaries_of_unstored_fields_read_the_hosts_variables() {
    let mut schema = schema();
    let mut context = om_data::formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    let compiled =
        om_data::formula::compile("If ( IsEmpty ( $$rate ) ; -1 ; $$rate )", &context).unwrap();
    schema.add_field(
        TABLE,
        FieldDef::calculation(
            7,
            "Rated",
            ValueType::Number,
            om_data::formula::Formula {
                context: Some(om_data::formula::Key::from_id(OCCURRENCE)),
                formatted: None,
                polish: compiled.expr,
            },
            false,
        ),
    );
    schema.add_field(
        TABLE,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::Total, 7)),
            ..FieldDef::data(19, "Rated total", ValueType::Number)
        },
    );
    let source = CountingSource::new();
    let session = SessionInfo::default();
    let read = |rate: Option<i64>, text: &str| {
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        if let Some(rate) = rate {
            globals.set("$$rate", 1, Value::number(rate));
        }
        let file = FileCalc::new(&schema, &source, today(29));
        let mut host = FileHost::new(file, &session, &mut locals, &mut globals);
        let result = evaluate_text(text, &mut host, &Context::record(OCCURRENCE, 1));
        assert_eq!(result.code(), 0, "{text}");
        display(&result.value, &host)
    };
    for text in ["Rated total", "GetSummary ( Rated total ; Rated total )"] {
        assert_eq!(read(Some(10), text), "30", "{text}");
        assert_eq!(read(None, text), "-3", "{text}");
    }
    // The input itself reads it too, on its record.
    assert_eq!(read(Some(10), "Rated"), "10");
}

#[test]
fn summaries_of_unstored_fields_do_not_mix_sessions_or_dates() {
    let mut schema = schema();
    let mut context = om_data::formula::Context::new(schema.names());
    context.occurrence = Some(OCCURRENCE);
    let expressions = [
        (5, "Account", ValueType::Text, "Get ( AccountName )"),
        (
            6,
            "Current year",
            ValueType::Number,
            "Year ( GetAsDate ( \"10/29\" ) )",
        ),
    ];
    let fields: Vec<_> = expressions
        .into_iter()
        .map(|(id, name, ty, expr)| {
            let compiled = om_data::formula::compile(expr, &context).unwrap();
            FieldDef::calculation(
                id,
                name,
                ty,
                om_data::formula::Formula {
                    context: Some(om_data::formula::Key::from_id(OCCURRENCE)),
                    formatted: None,
                    polish: compiled.expr,
                },
                false,
            )
        })
        .collect();
    for field in fields {
        schema.add_field(TABLE, field);
    }
    for (id, kind, field, ty) in [
        (17, SummaryKind::List, 5, ValueType::Text),
        (18, SummaryKind::Total, 6, ValueType::Number),
    ] {
        schema.add_field(
            TABLE,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef::new(kind, field)),
                ..FieldDef::data(id, format!("Summary {id}"), ty)
            },
        );
    }
    let source = CountingSource::new();
    let cache = SummaryCache::default();
    let alice = SessionInfo {
        account_name: "Alice".into(),
        ..SessionInfo::default()
    };
    let bob = SessionInfo {
        account_name: "Bob".into(),
        ..SessionInfo::default()
    };
    let a = FileCalc::new(&schema, &source, today(29))
        .with_session(&alice)
        .with_summary_cache(&cache);
    let b = FileCalc::new(&schema, &source, today(29))
        .with_session(&bob)
        .with_summary_cache(&cache);
    assert_eq!(value(&a, OCCURRENCE, 1, 17, 1), "Alice\rAlice\rAlice");
    assert_eq!(value(&b, OCCURRENCE, 1, 17, 1), "Bob\rBob\rBob");
    assert_eq!(value(&a, OCCURRENCE, 2, 17, 1), "Alice\rAlice\rAlice");
    assert_eq!(value(&a, OCCURRENCE, 1, 18, 1), "6078");
    let previous_year = FileCalc::new(&schema, &source, Date::from_ymd(2025, 10, 29).unwrap())
        .with_session(&alice)
        .with_summary_cache(&cache);
    assert_eq!(value(&previous_year, OCCURRENCE, 1, 18, 1), "6075");
}

#[test]
fn recorded_found_sets_keep_sort_context_for_deferred_summaries() {
    let schema = schema();
    let source = CountingSource::new();
    let recorder = Recorder::new(&source);
    recorder
        .fetch(
            &schema,
            &CalcNeed::FoundSet {
                occurrence: OCCURRENCE,
                table: TABLE,
            },
            &FileCalc::new(&schema, &source, today(29)).cx(),
        )
        .unwrap();
    let mut data = recorder.take();
    assert_eq!(data.sorts[&OCCURRENCE].as_ref(), &[2]);
    for record in [1, 2, 3] {
        recorder.record(TABLE, record).unwrap();
    }
    data.merge(recorder.take());
    let cache = RefCell::new(data);
    let cached = CachedSource::new(&cache);
    let file = FileCalc::new(&schema, &cached, today(29));
    assert_eq!(value(&file, OCCURRENCE, 2, 13, 1), "30");
    assert_eq!(value(&file, OCCURRENCE, 3, 13, 1), "60");
    assert_eq!(value(&file, OCCURRENCE, 3, 14, 1), "1");
    let subtotal = file
        .summary(
            &Context::record(OCCURRENCE, 3),
            FieldId {
                occurrence: OCCURRENCE,
                field: 10,
            },
            FieldId {
                occurrence: OCCURRENCE,
                field: 2,
            },
        )
        .unwrap();
    assert_eq!(subtotal.to_text(&file.cx()).into_string(), "60");
    assert!(!cached.has_missing());
}

#[test]
fn recorded_unsort_replaces_older_sort_and_manual_overrides_take_priority() {
    let mut source = CountingSource::new();
    let recorder = Recorder::new(&source);
    // Reading only the sort also records it, without a found-set read.
    assert_eq!(recorder.found_sort(OCCURRENCE).unwrap().as_ref(), &[2]);
    let mut data = recorder.take();
    assert!(!data.is_empty());
    source.data.set_found_set(OCCURRENCE, &[1, 2, 3], None);
    let recorder = Recorder::new(&source);
    recorder.found_set(OCCURRENCE, TABLE).unwrap();
    let unsorted = recorder.take();
    assert!(unsorted.sorts[&OCCURRENCE].is_empty());
    data.merge(unsorted);
    assert!(recorder.found_sort(OCCURRENCE).is_none());
    assert!(recorder.take().sorts[&OCCURRENCE].is_empty());
    let cache = RefCell::new(data);
    assert!(CachedSource::new(&cache)
        .found_sort(OCCURRENCE)
        .unwrap()
        .is_empty());
    let cached = CachedSource::new(&cache).with_found_sort(OCCURRENCE, Arc::from([1]));
    assert_eq!(cached.found_sort(OCCURRENCE).unwrap().as_ref(), &[1]);

    cache.borrow_mut().sorts.insert(OCCURRENCE, Arc::from([2]));
    let unsorted_override = CachedSource::new(&cache).with_found_sort(OCCURRENCE, Arc::from([]));
    assert!(unsorted_override.found_sort(OCCURRENCE).unwrap().is_empty());
    // A replacement found set without sort metadata is unknown, so an
    // earlier set's sort cannot survive it.
    let mut unknown = CalcData::default();
    unknown.found.insert(OCCURRENCE, Arc::from([3]));
    cache.borrow_mut().merge(unknown);
    assert!(CachedSource::new(&cache).found_sort(OCCURRENCE).is_none());
    cache.borrow_mut().sorts.insert(OCCURRENCE, Arc::from([2]));
    cache.borrow_mut().invalidate_record(TABLE, 3);
    assert!(cache.borrow().sorts.is_empty());
}
