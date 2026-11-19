//! ExecuteSQL over a file's data (`om_data::host::sql`): records, globals
//! and calculations read through a calculation source, calculations on the
//! query's found set, the system tables and value lists, and how a client
//! without the file fetches what a query reads: whole found sets at once,
//! `field = value` as a lookup on the host, nothing record by record.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::{CalcError, Ctx, Date, Locale, Value, ValueType};
use om_data::formula::{Formula, Key};
use om_data::host::sql::{SqlCatalog, SqlFieldInfo, SqlValueList, ValueListSource};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, CalcSource, FieldClass, FieldDef, FileCalc,
    FileHost, MemorySource, RecordValues, Recorder, SessionInfo, SummaryDef, SummaryKind,
};

const PEOPLE: u64 = 0x8001;
const PEOPLE_TO: u64 = 0xd0_0001;
const FRIENDS_TO: u64 = 0xd0_0002;

const ID: u64 = 1;
const NAME: u64 = 2;
const BORN: u64 = 3;
const LABEL: u64 = 4;

fn compile(schema: &CalcSchema, source: &str) -> Formula {
    let mut ctx = om_data::formula::Context::new(schema.names());
    ctx.occurrence = Some(PEOPLE_TO);
    let expr = om_data::formula::compile(source, &ctx)
        .unwrap_or_else(|e| panic!("{source}: {e:?}"))
        .expr;
    Formula {
        context: Some(Key::from_id(PEOPLE_TO)),
        formatted: None,
        polish: expr,
    }
}

fn schema() -> CalcSchema {
    let mut s = CalcSchema::new("Club", Locale::en_us());
    s.add_table(PEOPLE, "People");
    s.add_occurrence(PEOPLE_TO, "People", PEOPLE);
    s.add_occurrence(FRIENDS_TO, "Friends", PEOPLE);
    s.add_field(PEOPLE, FieldDef::data(ID, "id", ValueType::Number));
    s.add_field(PEOPLE, FieldDef::data(NAME, "Name", ValueType::Text));
    s.add_field(PEOPLE, FieldDef::data(BORN, "Born", ValueType::Date));
    s.add_field(
        PEOPLE,
        FieldDef::data(LABEL, "gLabel", ValueType::Text).with_global(true),
    );
    s.add_field(
        PEOPLE,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::Total, ID)),
            ..FieldDef::data(5, "sTotal", ValueType::Number)
        },
    );
    for (id, name, ty, text) in [
        (6, "Shout", ValueType::Text, "Upper ( Name ) & \"!\""),
        (
            7,
            "Results",
            ValueType::Text,
            "If ( Get ( FoundCount ) = 1 ; \"Result\" ; \"Results\" )",
        ),
        (
            8,
            "Place",
            ValueType::Text,
            "Get ( RecordNumber ) & \"/\" & Get ( FoundCount ) & \"/\" & sTotal",
        ),
        (9, "Label", ValueType::Text, "gLabel & \":\" & Name"),
        (
            10,
            "Echo",
            ValueType::Text,
            "ExecuteSQL ( \"SELECT Echo FROM People\" ; \"\" ; \"\" )",
        ),
        (11, "Tag", ValueType::Text, "\"tag:\" & Name"),
    ] {
        let formula = compile(&s, text);
        s.add_field(PEOPLE, FieldDef::calculation(id, name, ty, formula, false));
    }
    let mut catalog = SqlCatalog::default();
    catalog.fields.insert(
        (PEOPLE, NAME),
        SqlFieldInfo {
            findable: true,
            ..SqlFieldInfo::default()
        },
    );
    catalog.fields.insert(
        (PEOPLE, 11),
        SqlFieldInfo {
            always: true,
            ..SqlFieldInfo::default()
        },
    );
    catalog.value_lists = vec![
        SqlValueList {
            id: 1,
            name: "Colors".into(),
            mod_count: 2,
            source: ValueListSource::Custom(vec!["Red".into(), "-".into(), "Blue".into()]),
        },
        SqlValueList {
            id: 2,
            name: "Names".into(),
            mod_count: 1,
            source: ValueListSource::Field {
                occurrence: PEOPLE_TO,
                field: NAME,
            },
        },
        SqlValueList {
            id: 3,
            name: "Nearby".into(),
            mod_count: 1,
            source: ValueListSource::Related,
        },
    ];
    s.set_sql(catalog);
    s
}

fn values(pairs: &[(u64, Value)]) -> RecordValues {
    let mut v = RecordValues::default();
    for (field, value) in pairs {
        v.set(*field, 1, value.clone());
    }
    v
}

fn date(y: i64, m: u32, d: u32) -> Value {
    Value::Date(Date::from_ymd(y, m, d).unwrap())
}

fn source() -> MemorySource {
    let mut m = MemorySource::new();
    let (n, t) = (Value::number, Value::text);
    m.put_record(
        PEOPLE,
        1,
        values(&[(ID, n(1)), (NAME, t("Ada")), (BORN, date(1815, 12, 10))]),
    );
    m.put_record(
        PEOPLE,
        2,
        values(&[(ID, n(2)), (NAME, t("ben")), (BORN, date(1990, 1, 2))]),
    );
    m.put_record(PEOPLE, 3, values(&[(ID, n(3)), (NAME, t("Ben"))]));
    m.put_record(PEOPLE, 4, values(&[(ID, n(4))]));
    m.set_globals(PEOPLE, values(&[(LABEL, t("Member"))]));
    m
}

fn today() -> Date {
    Date::from_ymd(2026, 10, 29).unwrap()
}

/// Evaluate `text` with no record, as a script's calculation would; the
/// value's text and the error code.
fn eval_with(schema: &CalcSchema, source: &dyn CalcSource, text: &str) -> (String, i32) {
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
    let result = evaluate_text(text, &mut host, &Context::new());
    (display(&result.value, &host), result.code())
}

fn sql(query: &str) -> String {
    let text = format!("ExecuteSQL ( {query:?} ; \"|\" ; \"¶\" )");
    let (value, code) = eval_with(&schema(), &source(), &text);
    assert_eq!(code, 0, "{query}: {value}");
    value
}

#[test]
fn queries_read_records_globals_and_calculations() {
    assert_eq!(
        sql("SELECT id, Name, Born FROM People ORDER BY id"),
        "1|Ada|1815-12-10\r2|ben|1990-01-02\r3|Ben|\r4||"
    );
    assert_eq!(
        sql("SELECT Shout, Label FROM People WHERE id < 3"),
        "ADA!|Member:Ada\rBEN!|Member:ben"
    );
    // Each occurrence is a table of its own; joins need no relationship.
    assert_eq!(
        sql("SELECT a.Name, b.id FROM People a JOIN Friends b ON a.id + 1 = b.id WHERE a.id = 3"),
        "Ben|4"
    );
    assert_eq!(
        sql("SELECT COUNT(*), MAX(Born) FROM People"),
        "4|1990-01-02"
    );
    let (value, code) = eval_with(
        &schema(),
        &source(),
        "ExecuteSQL ( \"SELECT Name FROM People WHERE id = ?\" ; \"\" ; \"\" ; 3 )",
    );
    assert_eq!((value.as_str(), code), ("Ben", 0));
    let (value, code) = eval_with(
        &schema(),
        &source(),
        "ExecuteSQLe ( \"SELECT Nope FROM People\" ; \"\" ; \"\" )",
    );
    assert_eq!(code, 8309);
    assert_eq!(
        value,
        "? ERROR: FQL0007/(1:7): The column named \"Nope\" does not exist in any table in the column reference's scope."
    );
}

#[test]
fn calculations_see_the_querys_found_set() {
    // execute-sql.md implementation notes: the WHERE conjuncts on a table
    // leave the found set its calculations see.
    assert_eq!(sql("SELECT Results FROM People WHERE id = 3"), "Result");
    assert_eq!(
        sql("SELECT Results FROM People WHERE id < 3"),
        "Results\rResults"
    );
    assert_eq!(
        sql("SELECT Place FROM People WHERE id > 1"),
        "1/3/9\r2/3/9\r3/3/9"
    );
    assert_eq!(sql("SELECT Place FROM People WHERE id = 2"), "1/1/2");
}

#[test]
fn calculations_over_empty_fields_are_empty_unless_evaluated_always() {
    // Record 4 has no name: Shout reads nothing; Tag is set to evaluate
    // always; Label references a global that has a value.
    assert_eq!(
        sql("SELECT Shout, Tag, Label FROM People WHERE id = 4"),
        "|tag:|Member:"
    );
}

#[test]
fn system_tables_and_value_lists() {
    assert_eq!(
        sql("SELECT TableName, TableId, BaseTableName, BaseFileName FROM FileMaker_Tables"),
        "Friends|1065090|People|Club\rPeople|1065089|People|Club"
    );
    assert_eq!(
        sql("SELECT FieldName, FieldType, FieldClass FROM FileMaker_Fields WHERE TableName = 'People' AND FieldId < 6"),
        "id|decimal|Normal\rName|varchar|Normal\rBorn|date|Normal\rgLabel|global varchar|Normal\rsTotal|decimal|Summary"
    );
    assert_eq!(
        sql("SELECT BaseTableName, BaseTableId, Source FROM FileMaker_BaseTables"),
        "People|129|<Internal>"
    );
    assert_eq!(
        sql("SELECT * FROM FileMaker_ValueLists"),
        "Colors|1|Custom|2\rNames|2|Field|1\rNearby|3|Related|1"
    );
    assert_eq!(
        sql("SELECT * FROM FileMaker_ValueList_Colors"),
        "Red\r-\rBlue"
    );
    // Unique values in UTF-16 order.
    assert_eq!(
        sql("SELECT * FROM FileMaker_ValueList_Names"),
        "Ada\rBen\rben"
    );
    assert_eq!(
        sql("SELECT * FROM FileMaker_ValueList_Nearby"),
        "<Valuelist Unsupported>"
    );
}

#[test]
fn a_calculation_that_queries_itself_ends() {
    // Each level reads Echo again; the innermost fails, which reads as
    // empty, so every level returns its rows of nothing.
    let mut m = MemorySource::new();
    m.put_record(PEOPLE, 1, values(&[(ID, Value::number(1))]));
    let (value, code) = eval_with(
        &schema(),
        &m,
        "ExecuteSQL ( \"SELECT Echo FROM People\" ; \"\" ; \"\" )",
    );
    assert_eq!((value.as_str(), code), ("", 0));
}

/// A host the client fetches from: the in-memory file with a value index
/// on `Name` (case-insensitive, so it proposes more than SQL's `=` keeps),
/// counting what it is asked.
struct Host {
    file: MemorySource,
    lookups: Cell<usize>,
}

impl CalcSource for Host {
    fn record(&self, table: u64, record: u64) -> Result<Option<Arc<RecordValues>>, CalcError> {
        self.file.record(table, record)
    }

    fn records(&self, table: u64) -> Result<Arc<[u64]>, CalcError> {
        self.file.records(table)
    }

    fn globals(&self, table: u64) -> Result<Arc<RecordValues>, CalcError> {
        self.file.globals(table)
    }

    fn lookup(&self, table: u64, field: u64, text: &str) -> Result<Option<Arc<[u64]>>, CalcError> {
        self.lookups.set(self.lookups.get() + 1);
        let mut ids = Vec::new();
        for &r in self.file.records(table)?.iter() {
            let values = self.file.record(table, r)?.unwrap();
            if values
                .get(field, 1)
                .is_some_and(|v| v.to_text(&cx()).as_str().eq_ignore_ascii_case(text))
            {
                ids.push(r);
            }
        }
        Ok(Some(ids.into()))
    }
}

fn cx() -> Ctx<'static> {
    static LOCALE: std::sync::OnceLock<Locale> = std::sync::OnceLock::new();
    Ctx::new(LOCALE.get_or_init(Locale::en_us), today())
}

/// The needs of each fetch a client made.
type Fetches = RefCell<Vec<Vec<CalcNeed>>>;

fn client_sql(host: &Host, cache: &RefCell<CalcData>, fetches: &Fetches, query: &str) -> String {
    let schema = schema();
    let fetch = |needs: &[CalcNeed]| {
        fetches.borrow_mut().push(needs.to_vec());
        let recorder = Recorder::new(host);
        for need in needs {
            recorder.fetch(&schema, need, &cx()).ok()?;
        }
        Some(recorder.take())
    };
    let source = CachedSource::new(cache).with_fetch(&fetch);
    let text = format!("ExecuteSQL ( {query:?} ; \"|\" ; \"¶\" )");
    let (value, code) = eval_with(&schema, &source, &text);
    assert_eq!(code, 0, "{query}: {value}");
    value
}

#[test]
fn clients_fetch_found_sets_at_once_and_look_values_up_on_the_host() {
    let host = Host {
        file: source(),
        lookups: Cell::new(0),
    };
    let fetches = Fetches::default();
    let cache = RefCell::new(CalcData::default());
    // `Name = 'Ben'`: the host proposes records 2 and 3 (its index folds
    // case), SQL keeps 3; only those two records are fetched, in one
    // request.
    assert_eq!(
        client_sql(
            &host,
            &cache,
            &fetches,
            "SELECT id FROM People WHERE Name = 'Ben'"
        ),
        "3"
    );
    assert_eq!(host.lookups.get(), 1);
    let needs = fetches.take();
    assert_eq!(needs.len(), 3, "{needs:?}");
    assert!(matches!(
        needs[0][..],
        [CalcNeed::Records { table: PEOPLE }]
    ));
    assert!(matches!(
        &needs[1][..],
        [CalcNeed::Lookup { table: PEOPLE, field: NAME, text }] if &**text == "Ben"
    ));
    assert_eq!(
        needs[2],
        [
            CalcNeed::Record {
                table: PEOPLE,
                record: 2
            },
            CalcNeed::Record {
                table: PEOPLE,
                record: 3
            }
        ]
    );
    // A scan reads the found set ahead: one request for the records the
    // cache lacks, none for the cached lookup.
    assert_eq!(
        client_sql(
            &host,
            &cache,
            &fetches,
            "SELECT id FROM People WHERE Name = 'Ben' OR id = 1"
        ),
        "1\r3"
    );
    let needs = fetches.take();
    assert_eq!(
        needs,
        [vec![
            CalcNeed::Record {
                table: PEOPLE,
                record: 1
            },
            CalcNeed::Record {
                table: PEOPLE,
                record: 4
            }
        ]]
    );
    // Texts a find cannot match whole, and number fields, are scanned.
    assert_eq!(
        client_sql(
            &host,
            &cache,
            &fetches,
            "SELECT id FROM People WHERE Name = ' Ben'"
        ),
        ""
    );
    assert_eq!(
        client_sql(
            &host,
            &cache,
            &fetches,
            "SELECT Name FROM People WHERE id = 3"
        ),
        "Ben"
    );
    assert_eq!(host.lookups.get(), 1);
    assert!(fetches.take().is_empty());
}

#[test]
fn a_record_being_edited_is_a_lookup_candidate() {
    let host = Host {
        file: source(),
        lookups: Cell::new(0),
    };
    let fetches = Fetches::default();
    let cache = RefCell::new(CalcData::default());
    let schema = schema();
    let fetch = |needs: &[CalcNeed]| {
        fetches.borrow_mut().push(needs.to_vec());
        let recorder = Recorder::new(&host);
        for need in needs {
            recorder.fetch(&schema, need, &cx()).ok()?;
        }
        Some(recorder.take())
    };
    // Record 4 is being renamed Ben in the window: the host's index does
    // not know it yet.
    let draft = Arc::new(values(&[
        (ID, Value::number(4)),
        (NAME, Value::text("Ben")),
    ]));
    let source = CachedSource::new(&cache)
        .with_fetch(&fetch)
        .with_overlay(PEOPLE, 4, draft);
    let (value, code) = eval_with(
        &schema,
        &source,
        "ExecuteSQL ( \"SELECT id FROM People WHERE Name = 'Ben'\" ; \"\" ; \",\" )",
    );
    assert_eq!((value.as_str(), code), ("3,4", 0));
}

#[test]
fn layouts_ask_for_a_found_set_in_one_request() {
    // Without a fetch, what is missing is listed for the next request:
    // the record list, then every record of the found set at once.
    let host = Host {
        file: source(),
        lookups: Cell::new(0),
    };
    let schema = schema();
    let cache = RefCell::new(CalcData::default());
    let text = "ExecuteSQL ( \"SELECT Shout FROM People WHERE id > 2\" ; \"\" ; \",\" )";
    let mut rounds = Vec::new();
    for _ in 0..4 {
        let source = CachedSource::new(&cache);
        let (value, _) = eval_with(&schema, &source, text);
        let missing = source.missing();
        if missing.is_empty() {
            assert_eq!(value, "BEN!,");
            break;
        }
        let recorder = Recorder::new(&host);
        for need in &missing {
            recorder.fetch(&schema, need, &cx()).unwrap();
        }
        cache.borrow_mut().merge(recorder.take());
        rounds.push(missing);
    }
    assert_eq!(rounds.len(), 2, "{rounds:?}");
    assert_eq!(rounds[0], [CalcNeed::Records { table: PEOPLE }]);
    assert_eq!(rounds[1].len(), 4);
}
