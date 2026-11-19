//! The calculation host over a file's metadata (file-metadata.md): the
//! file's locale in display, parsing and `Get ( FileLocaleElements )`;
//! relationships with every operator, several predicates and sorted sides;
//! every summary kind with break fields (`om_data::summary`), also over an
//! unstored calculation and in ExecuteSQL's columns; value lists; the
//! Design functions. The relationship, summary and value-list vectors are
//! FileMaker Server's own results on the probe file of file-metadata.md 4.3,
//! 5.3 and 6.4, the unstored calculations' and ExecuteSQL's on the same
//! records (script-runtime.md 13.19, execute-sql.md 12 S3).

use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::{Date, Locale, Value, ValueType};
use om_data::formula::{Formula, Key};
use om_data::host::{
    CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc, FileHost, LayoutEntry, LayoutField,
    MemorySource, RecordValues, RelationshipDef, SessionInfo,
};
use om_data::meta::{
    summary_flag, Relationship, ScriptEntry, SummaryDef, SummaryField, SummaryKind, ValueList,
    ValueSource,
};
use om_data::specs::{SortEntry, SortSpec, SpecField};
use om_data::ScriptKind;

const TASKS: u64 = 0x8002;
const TASKS_TO: u64 = 0xd0_0002;
/// Occurrences of Tasks the probe's relationships lead to.
const P: [(u64, &str); 6] = [
    (0xd0_0003, "P_B"),
    (0xd0_0004, "P_C"),
    (0xd0_0005, "P_D"),
    (0xd0_0006, "P_E"),
    (0xd0_0007, "P_F"),
    (0xd0_0008, "P_G"),
];

const TASK: u64 = 1;
const N: u64 = 2;
const W: u64 = 3;
const STATUS: u64 = 4;
const DUE: u64 = 5;
const CATEGORY: u64 = 6;
/// Unstored calculations: U = N * 2, UT = Task & " " & Status; summaries
/// of U: S_UTotal (total), S_UCount (count).
const U: u64 = 40;
const UT: u64 = 41;
const S_UTOTAL: u64 = 42;
const S_UCOUNT: u64 = 43;
/// Field option: evaluate even when every referenced field is empty.
const EVALUATE_ALWAYS: u32 = 0x20;

fn today() -> Date {
    Date::from_ymd(2026, 10, 29).unwrap()
}

fn field(occurrence: Option<u64>, field: u64) -> SpecField {
    SpecField {
        occurrence,
        field,
        repetition: 1,
    }
}

fn sorted_by(occurrence: u64, field_id: u64, descending: bool, blanks_last: bool) -> SortSpec {
    SortSpec {
        keep_sorted: false,
        blanks_last,
        fields: vec![SortEntry {
            field: field(Some(occurrence), field_id),
            summary: None,
            value_list: None,
            descending,
            locale: 0,
        }],
    }
}

/// A relationship as FileMaker stores it, decoded the way a host reads it.
fn relationship(
    id: u64,
    right: u64,
    predicates: &[(u8, u64, u64)],
    right_order: Option<SortSpec>,
) -> RelationshipDef {
    let key = |id: u64| {
        let segment = om_hbam::slot::uint_segment(id);
        let mut out = vec![segment.len() as u8 + 1, segment.len() as u8];
        out.extend(segment);
        out
    };
    let none = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: Vec::new(),
    };
    let mut definition = key(TASKS_TO);
    definition.extend(key(right));
    definition.extend(right_order.as_ref().unwrap_or(&none).encode());
    definition.extend(none.encode());
    let options: u32 = if right_order.is_some() { 0x40 } else { 0 };
    definition.extend(options.to_be_bytes());
    let predicates: Vec<Vec<u8>> = predicates
        .iter()
        .map(|(op, left, right)| [vec![*op], key(*left), key(*right)].concat())
        .collect();
    let bytes: Vec<&[u8]> = predicates.iter().map(Vec::as_slice).collect();
    RelationshipDef::from(&Relationship::decode(id, &definition, &bytes).unwrap())
}

fn summary(kind: SummaryKind, flags: u8, additional: Option<u64>) -> SummaryDef {
    SummaryDef {
        flags,
        additional: additional.map(|field| SummaryField {
            occurrence: Some(TASKS_TO),
            field,
            repetition: 1,
        }),
        ..SummaryDef::new(kind, N)
    }
}

/// `source` compiled in the Tasks occurrence.
fn compile(schema: &CalcSchema, source: &str) -> Formula {
    let mut ctx = om_data::formula::Context::new(schema.names());
    ctx.occurrence = Some(TASKS_TO);
    let expr = om_data::formula::compile(source, &ctx)
        .unwrap_or_else(|e| panic!("{source}: {e:?}"))
        .expr;
    Formula {
        context: Some(Key::from_id(TASKS_TO)),
        formatted: None,
        polish: expr,
    }
}

/// Unstored calculation U with `options`.
fn field_u(schema: &CalcSchema, options: u32) -> FieldDef {
    FieldDef {
        options,
        ..FieldDef::calculation(U, "U", ValueType::Number, compile(schema, "N * 2"), false)
    }
}

/// The probe file: Tasks and its summaries, six relationships, value
/// lists, layouts and scripts.
fn schema(locale: Locale) -> CalcSchema {
    let mut s = CalcSchema::new("Probe", locale);
    s.add_table(TASKS, "Tasks");
    s.add_occurrence(TASKS_TO, "Tasks", TASKS);
    for (id, name) in P {
        s.add_occurrence(id, name, TASKS);
    }
    let mut task = FieldDef::data(TASK, "Task", ValueType::Text);
    task.options = 0x8000_0000;
    task.comment = "What to do".into();
    s.add_field(TASKS, task);
    s.add_field(TASKS, FieldDef::data(N, "N", ValueType::Number));
    s.add_field(TASKS, FieldDef::data(W, "W", ValueType::Number));
    s.add_field(TASKS, FieldDef::data(STATUS, "Status", ValueType::Text));
    s.add_field(TASKS, FieldDef::data(DUE, "Due Date", ValueType::Date));
    s.add_field(TASKS, FieldDef::data(CATEGORY, "Category", ValueType::Text));
    let summaries = [
        (10, "S_Total", summary(SummaryKind::Total, 0, None)),
        (
            11,
            "S_RunTotal",
            summary(
                SummaryKind::Total,
                summary_flag::RUNNING | summary_flag::RESTART,
                Some(STATUS),
            ),
        ),
        (
            12,
            "S_RunCount",
            summary(SummaryKind::Count, summary_flag::RUNNING, None),
        ),
        (13, "S_Fraction", summary(SummaryKind::Fraction, 0, None)),
        (
            14,
            "S_FracSub",
            summary(SummaryKind::Fraction, summary_flag::OPTION, Some(CATEGORY)),
        ),
        (15, "S_Average", summary(SummaryKind::Average, 0, None)),
        (
            16,
            "S_Weighted",
            summary(SummaryKind::Average, summary_flag::OPTION, Some(W)),
        ),
        (17, "S_Count", summary(SummaryKind::Count, 0, None)),
        (18, "S_Min", summary(SummaryKind::Minimum, 0, None)),
        (19, "S_Max", summary(SummaryKind::Maximum, 0, None)),
        (20, "S_StDev", summary(SummaryKind::StdDeviation, 0, None)),
        (
            21,
            "S_StDevP",
            summary(SummaryKind::StdDeviation, summary_flag::OPTION, None),
        ),
        (22, "S_List", summary(SummaryKind::List, 0, None)),
    ];
    for (id, name, def) in summaries {
        let value_type = if def.kind == SummaryKind::List {
            ValueType::Text
        } else {
            ValueType::Number
        };
        s.add_field(
            TASKS,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(def),
                ..FieldDef::data(id, name, value_type)
            },
        );
    }
    let u = field_u(&s, 0);
    s.add_field(TASKS, u);
    let ut = compile(&s, "Task & \" \" & Status");
    s.add_field(
        TASKS,
        FieldDef::calculation(UT, "UT", ValueType::Text, ut, false),
    );
    for (id, name, kind) in [
        (S_UTOTAL, "S_UTotal", SummaryKind::Total),
        (S_UCOUNT, "S_UCount", SummaryKind::Count),
    ] {
        s.add_field(
            TASKS,
            FieldDef {
                class: FieldClass::Summary,
                summary: Some(SummaryDef::new(kind, U)),
                ..FieldDef::data(id, name, ValueType::Number)
            },
        );
    }
    let [b, c, d, e, f, g] = P.map(|(id, _)| id);
    // Stored codes: 0 =, 1 ≠, 2 <, 3 ≤, 4 >, 5 ≥, 6 ×; "left op right".
    s.add_relationship(relationship(
        1,
        b,
        &[(5, N, N)],
        Some(sorted_by(b, N, true, false)),
    ));
    s.add_relationship(relationship(2, c, &[(2, N, N)], None));
    s.add_relationship(relationship(
        3,
        d,
        &[(1, N, N), (0, STATUS, STATUS), (4, DUE, DUE)],
        None,
    ));
    s.add_relationship(relationship(4, e, &[(6, TASK, TASK)], None));
    s.add_relationship(relationship(
        5,
        f,
        &[(3, N, N)],
        Some(sorted_by(f, TASK, false, true)),
    ));
    s.add_relationship(relationship(6, g, &[(4, N, N), (5, W, W)], None));
    s.add_value_list(ValueList {
        id: 2,
        name: "Letters".into(),
        source: ValueSource::Custom(vec![
            "Alpha".into(),
            "-".into(),
            "Beta".into(),
            "Gamma".into(),
        ]),
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    });
    s.add_value_list(ValueList {
        id: 1,
        name: "Numbers".into(),
        source: ValueSource::Field {
            primary: field(Some(TASKS_TO), N),
            secondary: None,
            start: None,
        },
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    });
    s.add_value_list(ValueList {
        id: 3,
        name: "By status".into(),
        source: ValueSource::Field {
            primary: field(Some(TASKS_TO), TASK),
            secondary: Some(field(Some(TASKS_TO), STATUS)),
            start: None,
        },
        options: 0x2,
        sort_locale: 0,
        separator: " ".into(),
    });
    s.add_value_list(ValueList {
        id: 0x8080,
        name: "Related".into(),
        source: ValueSource::Field {
            primary: field(Some(c), TASK),
            secondary: None,
            start: Some(TASKS_TO),
        },
        options: 0x1,
        sort_locale: 0,
        separator: " ".into(),
    });
    s.add_layout_entry(LayoutEntry {
        fields: vec![
            LayoutField::new(TASKS_TO, TASK),
            LayoutField::new(b, N),
            LayoutField {
                control: 4,
                value_list: Some(2),
                ..LayoutField::new(TASKS_TO, STATUS)
            },
        ],
        ..LayoutEntry::new(0x80, "Task Detail", Some(TASKS_TO))
    });
    s.add_layout(1, "Task List", Some(TASKS_TO));
    s.set_scripts(vec![
        ScriptEntry {
            id: 141,
            name: "Open".into(),
            kind: ScriptKind::Script,
        },
        ScriptEntry {
            id: 3,
            name: "Reports".into(),
            kind: ScriptKind::Folder,
        },
        ScriptEntry {
            id: 2,
            name: "Close".into(),
            kind: ScriptKind::Script,
        },
    ]);
    s
}

/// One probe record: id, Task, N, W, Status, day of the Due Date in
/// January 2020, Category.
type Row = (
    u64,
    &'static str,
    Option<i64>,
    i64,
    &'static str,
    Option<u32>,
    &'static str,
);

/// Tasks in creation order.
fn source() -> MemorySource {
    let rows: [Row; 6] = [
        (1, "e", Some(5), 1, "Pending", Some(5), "Work"),
        (2, "a", Some(1), 2, "Pending", Some(1), "Home"),
        (3, "d", Some(4), 3, "Done", Some(4), "Work"),
        (4, "b", Some(2), 1, "Done", Some(2), "Home"),
        (5, "", None, 1, "Pending", None, "Home"),
        (6, "c", Some(3), 2, "Pending", Some(3), "Work"),
    ];
    let mut m = MemorySource::new();
    for (id, task, n, w, status, due, category) in rows {
        let mut v = RecordValues::default();
        v.set(TASK, 1, Value::text(task));
        if let Some(n) = n {
            v.set(N, 1, Value::number(n));
        }
        v.set(W, 1, Value::number(w));
        v.set(STATUS, 1, Value::text(status));
        if let Some(day) = due {
            v.set(DUE, 1, Value::Date(Date::from_ymd(2020, 1, day).unwrap()));
        }
        v.set(CATEGORY, 1, Value::text(category));
        m.put_record(TASKS, id, v);
    }
    m
}

const E: u64 = 1;
const C: u64 = 6;

fn eval_on(schema: &CalcSchema, source: &dyn CalcSource, record: u64, text: &str) -> String {
    let session = SessionInfo::for_this_machine();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let result = evaluate_text(text, &mut host, &Context::record(TASKS_TO, record));
    assert_eq!(result.code(), 0, "{text}");
    display(&result.value, &host)
}

fn eval(record: u64, text: &str) -> String {
    eval_on(&schema(Locale::en_us()), &source(), record, text)
}

#[test]
fn relationships_follow_every_operator_and_their_sort() {
    let cases = [
        // N ≥ N, the right side sorted by N descending.
        ("List ( P_B::N )", "3\r2\r1", "5\r4\r3\r2\r1"),
        // N < N, unsorted: creation order.
        ("List ( P_C::N )", "5\r4", ""),
        // N ≠ N and Status = Status and Due Date > Due Date.
        ("List ( P_D::N )", "1", "1\r3"),
        // Task × Task relates every record; List skips the empty N.
        ("List ( P_E::N )", "5\r1\r4\r2\r3", "5\r1\r4\r2\r3"),
        // N ≤ N, sorted by Task ascending, blanks last.
        ("List ( P_F::Task )", "c\rd\re", "e"),
        // N > N and W ≥ W.
        ("List ( P_G::N )", "1\r2", "2"),
    ];
    for (text, from_c, from_e) in cases {
        assert_eq!(eval(C, text), from_c, "{text} from c");
        assert_eq!(eval(E, text), from_e, "{text} from e");
    }
    // The first related record is the first in the sort.
    assert_eq!(eval(C, "P_B::N"), "3");
    assert_eq!(eval(C, "GetNthRecord ( P_B::N ; 2 )"), "2");
    // Six records, the one with an empty Task not counted.
    assert_eq!(eval(C, "Count ( P_E::Task )"), "5");
}

/// One row of the probe's summary table: Task, N, running total
/// restarting by Status, running count, fraction, fraction subtotaled by
/// Category, GetSummary by Category, by Status, by itself, and the running
/// total's GetSummary by Status.
fn summary_row(source: &MemorySource, record: u64) -> Vec<String> {
    let schema = schema(Locale::en_us());
    [
        "Task",
        "N",
        "S_RunTotal",
        "S_RunCount",
        "Round ( S_Fraction ; 4 )",
        "Round ( S_FracSub ; 4 )",
        "GetSummary ( S_Total ; Category )",
        "GetSummary ( S_Total ; Status )",
        "GetSummary ( S_Total ; S_Total )",
        "GetSummary ( S_RunTotal ; Status )",
    ]
    .iter()
    .map(|text| eval_on(&schema, source, record, text))
    .collect()
}

fn table(source: &MemorySource, order: &[u64]) -> Vec<String> {
    order
        .iter()
        .map(|r| summary_row(source, *r).join(" "))
        .collect()
}

#[test]
fn summaries_run_restart_and_break_by_the_sort() {
    let unsorted = source();
    assert_eq!(
        table(&unsorted, &[1, 2, 3, 4, 5, 6]),
        [
            "e 5 5 1 .3333    15 ",
            "a 1 6 2 .0667    15 ",
            "d 4 10 3 .2667    15 ",
            "b 2 12 4 .1333    15 ",
            "  12 4     15 ",
            "c 3 15 5 .2    15 ",
        ]
    );
    // Sorted by Category, Task: Home (∅, a, b), Work (c, d, e).
    let mut by_category = source();
    by_category.set_found_set(TASKS_TO, &[5, 2, 4, 6, 3, 1], Some(&[CATEGORY, TASK]));
    assert_eq!(
        table(&by_category, &[5, 2, 4, 6, 3, 1]),
        [
            "   0 0 0 3  15 ",
            "a 1 1 1 .0667 .3333 3  15 ",
            "b 2 3 2 .1333 .6667 3  15 ",
            "c 3 6 3 .2 .25 12  15 ",
            "d 4 10 4 .2667 .3333 12  15 ",
            "e 5 15 5 .3333 .4167 12  15 ",
        ]
    );
    // Sorted by Status, N: Done (b, d), Pending (∅, a, c, e).
    let mut by_status = source();
    by_status.set_found_set(TASKS_TO, &[4, 3, 5, 2, 6, 1], Some(&[STATUS, N]));
    assert_eq!(
        table(&by_status, &[4, 3, 5, 2, 6, 1]),
        [
            "b 2 2 1 .1333   6 15 6",
            "d 4 6 2 .2667   6 15 6",
            "   2    9 15 9",
            "a 1 1 3 .0667   9 15 9",
            "c 3 4 4 .2   9 15 9",
            "e 5 9 5 .3333   9 15 9",
        ]
    );
}

#[test]
fn every_summary_kind_over_the_found_set() {
    // Unsorted, on the last record (file-metadata.md 5.3).
    let cases = [
        ("S_Total", "15"),
        ("S_Average", "3"),
        ("S_Weighted", "3"),
        ("S_Count", "5"),
        ("S_Min", "1"),
        ("S_Max", "5"),
        ("S_StDev", "1.5811388300841897"),
        ("S_StDevP", "1.414213562373095"),
        ("S_Fraction", ".2"),
        ("S_List", "5\r1\r4\r2\r3"),
        ("S_RunTotal", "15"),
        ("S_RunCount", "5"),
    ];
    for (text, expected) in cases {
        assert_eq!(eval(C, text), expected, "{text}");
    }
    // A List of Task skips the record without one.
    let mut list = schema(Locale::en_us());
    list.add_field(
        TASKS,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::List, TASK)),
            ..FieldDef::data(30, "S_Tasks", ValueType::Text)
        },
    );
    assert_eq!(eval_on(&list, &source(), C, "S_Tasks"), "e\ra\rd\rb\rc");
}

/// U = N * 2 is empty on the record without N, as FileMaker's "do not
/// evaluate if all referenced fields are empty" leaves it (finds and
/// ExecuteSQL on FileMaker Server, script-runtime.md 13.19); summaries read
/// unstored calculations evaluated.
#[test]
fn unstored_calculations_and_summaries_of_them() {
    const EMPTY_N: u64 = 5;
    assert_eq!(eval(C, "U"), "6");
    assert_eq!(eval(EMPTY_N, "U"), "");
    assert_eq!(eval(EMPTY_N, "UT"), " Pending");
    assert_eq!(eval(C, "S_UTotal"), "30");
    assert_eq!(eval(C, "S_UCount"), "5");
    assert_eq!(eval(C, "List ( P_E::U )"), "10\r2\r8\r4\r6");
    // Set to evaluate always: 0, and counted.
    let mut always = schema(Locale::en_us());
    let u = field_u(&always, EVALUATE_ALWAYS);
    always.add_field(TASKS, u);
    assert_eq!(eval_on(&always, &source(), EMPTY_N, "U"), "0");
    assert_eq!(eval_on(&always, &source(), C, "S_UCount"), "6");
}

/// `ExecuteSQL ( query ; "|" ; "¶" )` on record c.
fn sql(query: &str) -> String {
    eval(C, &format!("ExecuteSQL ( \"{query}\" ; \"|\" ; \"¶\" )"))
}

/// A summary column is the summary over the query's found set in record
/// order: the table without a WHERE, the records the WHERE leaves
/// otherwise (FileMaker Server, execute-sql.md 12 S3, batteries q19 and q20
/// on the same records; P_B stands for their P_A). FETCH and OFFSET cut it
/// to the result's records, and with a WHERE and an ORDER BY running
/// summaries follow the ORDER BY (q20). DISTINCT, which cuts FileMaker's
/// found set too, is a known deviation (execute-sql.md implementation
/// notes).
#[test]
fn execute_sql_summary_columns_summarize_the_querys_found_set() {
    let cases = [
        (
            "SELECT Task, N, S_Total FROM Tasks",
            "e|5|15\ra|1|15\rd|4|15\rb|2|15\r||15\rc|3|15",
        ),
        ("SELECT Task, S_Total FROM Tasks WHERE Status = 'Done'", "d|6\rb|6"),
        ("SELECT Task, S_Total FROM Tasks WHERE N > 2", "e|12\rd|12\rc|12"),
        (
            "SELECT Task, S_RunTotal, S_RunCount FROM Tasks",
            "e|5|1\ra|6|2\rd|10|3\rb|12|4\r|12|4\rc|15|5",
        ),
        // Running summaries follow record order, not the ORDER BY.
        (
            "SELECT Task, S_RunTotal, S_RunCount FROM Tasks ORDER BY N DESC",
            "e|5|1\rd|10|3\rc|15|5\rb|12|4\ra|6|2\r|12|4",
        ),
        (
            "SELECT Task, S_RunTotal, S_RunCount FROM Tasks WHERE Category = 'Work'",
            "e|5|1\rd|9|2\rc|12|3",
        ),
        // Unsorted: the subtotaled fraction is empty.
        (
            "SELECT Task, S_Fraction, S_FracSub FROM Tasks",
            "e|.3333333333333333|\ra|.0666666666666667|\rd|.2666666666666667|\rb|.1333333333333333|\r||\rc|.2|",
        ),
        (
            "SELECT Task, S_Fraction FROM Tasks WHERE Status = 'Done'",
            "d|.6666666666666667\rb|.3333333333333333",
        ),
        ("SELECT COUNT(S_Total), MAX(S_Total), SUM(S_Total) FROM Tasks", "6|15|90"),
        // A WHERE on a summary reads it over the whole table.
        ("SELECT Task FROM Tasks WHERE S_Total = 15", "e\ra\rd\rb\r\rc"),
        ("SELECT Task FROM Tasks WHERE S_Total IS NULL", ""),
        ("SELECT Task, S_Total FROM Tasks WHERE S_Total = 6", ""),
        (
            "SELECT a.Task, b.S_Total FROM Tasks a JOIN P_B b ON a.N = b.N",
            "e|15\ra|15\rd|15\rb|15\rc|15",
        ),
        ("SELECT Status, MAX(S_Total) FROM Tasks GROUP BY Status", "Done|15\rPending|15"),
        (
            "SELECT Task, S_Total FROM Tasks WHERE Task IN ('a', 'b') OR N = 5",
            "e|8\ra|8\rb|8",
        ),
        (
            "SELECT Task, U, UT FROM Tasks",
            "e|10|e Pending\ra|2|a Pending\rd|8|d Done\rb|4|b Done\r|| Pending\rc|6|c Pending",
        ),
        ("SELECT Task FROM Tasks WHERE U > 5", "e\rd\rc"),
        // Any condition on the one table narrows its found set (q20).
        ("SELECT Task, S_Total FROM Tasks WHERE NOT (Status = 'Pending')", "d|6\rb|6"),
        (
            "SELECT Task, S_Total FROM Tasks WHERE Status = 'Done' OR Category = 'Work'",
            "e|14\rd|14\rb|14\rc|14",
        ),
        (
            "SELECT Task, S_Total FROM Tasks WHERE N > 1 AND (W = 1 OR Category = 'Home')",
            "e|7\rb|7",
        ),
        ("SELECT Task, S_Total FROM Tasks WHERE N + W > 5", "e|9\rd|9"),
        ("SELECT Task, S_Total FROM Tasks WHERE UPPER(Task) = 'A' OR N = 5", "e|6\ra|6"),
        ("SELECT COUNT(*), MAX(S_Total) FROM Tasks WHERE Status = 'Done'", "2|6"),
        // In a join only the first table's conditions narrow its found
        // set; the other table keeps every record.
        (
            "SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a JOIN P_B b ON a.N = b.N WHERE a.Status = 'Done'",
            "d|6|15\rb|6|15",
        ),
        (
            "SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a, P_B b WHERE a.N = b.N AND a.Status = 'Done' AND b.Category = 'Home'",
            "b|6|15",
        ),
        (
            "SELECT b.Task, b.S_Total, a.S_Total FROM P_B b JOIN Tasks a ON a.N = b.N WHERE b.Status = 'Done'",
            "d|6|15\rb|6|15",
        ),
        (
            "SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a, P_B b WHERE a.Status = 'Done' AND b.Status = 'Done'",
            "d|6|15\rd|6|15\rb|6|15\rb|6|15",
        ),
        (
            "SELECT a.Task, a.S_Total, b.Task FROM Tasks a LEFT JOIN P_B b ON a.N = b.W WHERE a.Category = 'Home'",
            "a|3|e\ra|3|b\ra|3|\rb|3|a\rb|3|c\r|3|",
        ),
        // A subquery has a found set of its own.
        (
            "SELECT Task, (SELECT MAX(S_Total) FROM P_B) FROM Tasks WHERE Status = 'Done'",
            "d|15\rb|15",
        ),
        (
            "SELECT Task, (SELECT MAX(p.S_Total) FROM P_B p WHERE p.Category = 'Home') FROM Tasks WHERE Status = 'Done'",
            "d|3\rb|3",
        ),
        // FETCH and OFFSET cut the found set to the result's records (q20).
        ("SELECT Task, S_Total FROM Tasks FETCH FIRST 2 ROWS ONLY", "e|6\ra|6"),
        (
            "SELECT Task, S_Total FROM Tasks ORDER BY Task FETCH FIRST 2 ROWS ONLY",
            "|1\ra|1",
        ),
        (
            "SELECT Task, S_Total FROM Tasks OFFSET 2 ROWS FETCH FIRST 2 ROWS ONLY",
            "d|6\rb|6",
        ),
        ("SELECT Task, S_Total FROM Tasks OFFSET 4 ROWS", "|3\rc|3"),
        (
            "SELECT Task, S_Total FROM Tasks WHERE Status = 'Pending' FETCH FIRST 2 ROWS ONLY",
            "e|6\ra|6",
        ),
        (
            "SELECT Task, S_RunTotal FROM Tasks FETCH FIRST 3 ROWS ONLY",
            "e|5\ra|6\rd|10",
        ),
        // The other columns keep their values.
        ("SELECT Task, N, S_Total FROM Tasks FETCH FIRST 2 ROWS ONLY", "e|5|6\ra|1|6"),
        // With a WHERE, running summaries follow the ORDER BY (q20).
        (
            "SELECT Task, S_RunTotal FROM Tasks WHERE N > 1 ORDER BY Task",
            "b|2\rc|5\rd|9\re|14",
        ),
    ];
    for (query, expected) in cases {
        assert_eq!(sql(query), expected, "{query}");
    }
}

#[test]
fn value_lists_list_custom_field_and_related_values() {
    assert_eq!(
        eval(C, "ValueListItems ( \"Probe\" ; \"Letters\" )"),
        "Alpha\r-\rBeta\rGamma"
    );
    assert_eq!(
        eval(C, "ValueListItems ( \"Probe\" ; \"Numbers\" )"),
        "1\r2\r3\r4\r5"
    );
    // One per distinct Status, the first record winning, by Status.
    assert_eq!(
        eval(C, "ValueListItems ( Get ( FileName ) ; \"By status\" )"),
        "d\re"
    );
    // Only the values related to this record through Tasks to P_C.
    assert_eq!(
        eval(C, "ValueListItems ( \"Probe\" ; \"Related\" )"),
        "d\re"
    );
    assert_eq!(eval(E, "ValueListItems ( \"Probe\" ; \"Related\" )"), "");
    assert_eq!(
        eval(C, "ValueListNames ( \"Probe\" )"),
        "Letters\rNumbers\rBy status\rRelated"
    );
    // Display order, ids as FileMaker shows them: key `80 80` is 256.
    assert_eq!(eval(C, "ValueListIDs ( \"Probe\" )"), "2\r1\r3\r256");
    assert_eq!(eval(C, "ValueListItems ( \"Other\" ; \"Letters\" )"), "");
}

/// A custom set that gives value list `hidden` no access and every other
/// list view-only access (session-identity.md 2.2): its items are read
/// nowhere, its name and id still are.
#[test]
fn a_value_list_without_access_has_no_items() {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, ItemAccess, Quad, TableRules,
    };
    use om_data::host::security::SetAccess;
    let mut schema = schema(Locale::en_us());
    let letters = schema.value_list_named("Letters").unwrap().id;
    let read = TableRules {
        actions: Actions {
            create: Allow::No,
            view: Allow::Yes,
            edit: Allow::No,
            delete: Allow::No,
        },
        fields: AccessLevel::Modifiable,
        field_levels: Default::default(),
    };
    let mut value_lists = ItemAccess {
        default: Some(Quad(0xd6)),
        ..ItemAccess::default()
    };
    // `09, 21 L` = `d5`: no access.
    value_lists.items.insert(
        letters
            .to_be_bytes()
            .into_iter()
            .skip_while(|b| *b == 0)
            .collect(),
        Quad(0xd5),
    );
    schema.authenticated_account.set_access = Some(SetAccess::Custom(Box::new(CustomRules {
        default: read,
        tables: Default::default(),
        layouts: ItemAccess::default(),
        scripts: ItemAccess::default(),
        value_lists,
        prints: true,
        exports: true,
        menus: om_data::host::privilege_set::MenuCommands::All,
    })));
    let source = source();
    let eval = |text| eval_on(&schema, &source, C, text);
    assert_eq!(eval("ValueListItems ( \"Probe\" ; \"Letters\" )"), "");
    assert_eq!(
        eval("ValueListItems ( \"Probe\" ; \"Numbers\" )"),
        "1\r2\r3\r4\r5"
    );
    assert_eq!(
        eval("ValueListNames ( \"Probe\" )"),
        "Letters\rNumbers\rBy status\rRelated"
    );
}

#[test]
fn design_functions_answer_from_the_schema() {
    // Formats as FileMaker Server answers them (calc-functions.md 5.8,
    // s30-design): occurrences and base tables by name, the rest in
    // display order.
    let tables = "P_B\rP_C\rP_D\rP_E\rP_F\rP_G\rTasks";
    assert_eq!(eval(C, "TableNames ( \"Probe\" )"), tables);
    assert_eq!(eval(C, "TableNames ( \"probe.FMP12\" )"), tables);
    assert_eq!(eval(C, "TableNames ( \" Probe\" )"), "");
    // Occurrence key d0 00 02 is id 1065090; table 80 02 is 130.
    assert!(eval(C, "TableIDs ( \"Probe\" )").ends_with("\r1065090"));
    assert_eq!(eval(C, "BaseTableNames ( \"Probe\" )"), "Tasks");
    assert_eq!(eval(C, "BaseTableIDs ( \"Probe\" )"), "130");
    assert_eq!(
        eval(C, "LayoutNames ( \"Probe\" )"),
        "Task Detail\rTask List"
    );
    assert_eq!(eval(C, "ScriptNames ( \"Probe\" )"), "Open\rClose");
    assert_eq!(eval(C, "ScriptIDs ( \"Probe\" )"), "141\r2");
    assert_eq!(
        eval(C, "FieldNames ( \"Probe\" ; \"Task Detail\" )"),
        "Task\rP_B::N\rStatus"
    );
    assert!(eval(C, "FieldNames ( \"Probe\" ; \"P_C\" )").starts_with("Task\rN\rW\rStatus"));
    // A related field's id is the occurrence's and the field's.
    assert_eq!(
        eval(C, "FieldIDs ( \"Probe\" ; \"Task Detail\" )"),
        format!("1\r{}::2\r4", 1065091)
    );
    assert_eq!(
        eval(C, "FieldType ( \"Probe\" ; \"Tasks::Task\" )"),
        "Standard Text Indexed 1"
    );
    assert_eq!(
        eval(C, "FieldType ( \"Probe\" ; \"Due Date\" )"),
        "Standard Date Unindexed 1"
    );
    assert_eq!(
        eval(C, "FieldType ( \"Probe\" ; \"S_Total\" )"),
        "Summary Number Unindexed 1"
    );
    // The control style of the field's object, with its value list.
    assert_eq!(
        eval(C, "FieldStyle ( \"Probe\" ; \"Task Detail\" ; \"Status\" )"),
        "RadioButton Letters"
    );
    assert_eq!(
        eval(C, "FieldStyle ( \"Probe\" ; \"Task Detail\" ; \"P_B::N\" )"),
        "Standard"
    );
    assert_eq!(
        eval(
            C,
            "FieldRepetitions ( \"Probe\" ; \"Task Detail\" ; \"Task\" )"
        ),
        "1 vertical"
    );
    assert_eq!(
        eval(C, "FieldComment ( \"Probe\" ; \"Task\" )"),
        "What to do"
    );
    // The related side first, its operator read from it.
    assert_eq!(
        eval(C, "RelationInfo ( \"Probe\" ; \"P_D\" )"),
        "Source:Probe\rTable:Tasks\rOptions:\r\
         Tasks::N <> P_D::N\rTasks::Status = P_D::Status\rTasks::Due Date > P_D::Due Date"
    );
    let info = eval(C, "RelationInfo ( \"Probe\" ; \"Tasks\" )");
    assert!(
        info.starts_with("Source:Probe\rTable:P_B\rOptions:Sorted\rP_B::N <= Tasks::N\r\r"),
        "{info:?}"
    );
    assert_eq!(eval(C, "DatabaseNames"), "Probe");
}

#[test]
fn the_files_locale_formats_display_parsing_and_its_elements() {
    let source = source();
    let de = schema(Locale::de_de());
    let de_cases = [
        ("GetAsText ( Date ( 2 ; 17 ; 2014 ) )", "17.02.2014"),
        (
            "GetAsText ( Timestamp ( Date ( 2 ; 17 ; 2014 ) ; Time ( 13 ; 5 ; 9 ) ) )",
            "17.02.2014 13:05:09",
        ),
        ("GetAsNumber ( \"1.234,5\" )", "1234,5"),
        ("DayName ( Date ( 2 ; 17 ; 2014 ) )", "Montag"),
        ("MonthName ( Date ( 2 ; 17 ; 2014 ) )", "Februar"),
        ("GetAsDate ( \"17.02.2014\" )", "17.02.2014"),
        ("P_B::Due Date", "03.01.2020"),
    ];
    for (text, expected) in de_cases {
        assert_eq!(eval_on(&de, &source, C, text), expected, "de {text}");
    }
    let ja = schema(Locale::ja_jp());
    let ja_cases = [
        ("GetAsText ( Date ( 2 ; 17 ; 2014 ) )", "2014/02/17"),
        ("GetAsNumber ( \"1.234,5\" )", "1.2345"),
        ("DayName ( Date ( 2 ; 17 ; 2014 ) )", "月曜日"),
        ("MonthName ( Date ( 2 ; 17 ; 2014 ) )", "2月"),
        ("GetAsText ( 1234567.5 / 10 )", "123456.75"),
        ("Due Date", "2020/01/03"),
    ];
    for (text, expected) in ja_cases {
        assert_eq!(eval_on(&ja, &source, C, text), expected, "ja {text}");
    }
    // FileMaker Pro uses the system's formats (`UseSystemFormatsState` 1,
    // calc-functions.md 6.3 O1): `Active` marks the system's document.
    for (schema, locale) in [(&de, Locale::de_de()), (&ja, Locale::ja_jp())] {
        assert_eq!(
            eval_on(schema, &source, C, "Get ( FileLocaleElements )"),
            locale.elements_json(false)
        );
    }
    let system = eval_on(&de, &source, C, "Get ( SystemLocaleElements )");
    assert!(system.contains("\"IDStr\" : \"English\"") && system.contains("\"Active\" : true"));
}
