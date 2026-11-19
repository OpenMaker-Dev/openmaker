//! The desktop provider reads related hosted files' tables (external.rs):
//! a Root file whose occurrence "Remote Tasks" shows the Tasks table of a
//! Related file, both synthetic Tasks files with deliberately identical
//! table, field, record and custom-function ids, served by one `om-server`
//! on verified loopback TLS.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;
#[path = "../../om-server/tests/fixtures/relay.rs"]
mod relay;
#[path = "common/privileges.rs"]
mod security;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use fixture::{ACCOUNT, COUNT, DUE, NAME, OCCURRENCE, SETTING, TABLE};
use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::Date;
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, FileCalc, FileHost, SessionInfo,
};
use om_data::DataProvider;
use om_formula::NameTable;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_provider::NativeProvider;
use related::{reference, reference_with, Host};

const ROOT: &str = "Root.fmp12";
const RELATED: &str = "Related.fmp12";
/// Root's occurrence of Related's Tasks.
const REMOTE: u64 = 0xd0_0040;
/// Related's occurrence of Root's Tasks: a cycle back to the root file.
const BACK: u64 = 0xd0_0041;
/// Root's occurrence of a file the host does not have.
const MISSING: u64 = 0xd0_0042;
/// Related's calculation field reading its own file's names.
const INFO: u64 = 18;

/// `field` of record `record` of Tasks set to `text`.
fn put(root: &mut OwnedNode, record: u64, field: u64, text: &str) {
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(record),
            &uint_segment(field),
        ],
        om_scripts::text_node(&[], text),
    );
}

/// Global Tasks::Setting stored as `text`.
fn global(root: &mut OwnedNode, text: &str) {
    root.put(
        &[&uint_segment(TABLE), &[8], &uint_segment(SETTING)],
        om_scripts::text_node(&[], text),
    );
}

/// Custom function 1, `Marker ()`, answering `result`.
fn function(root: &mut OwnedNode, result: &str) {
    root.put(
        &[&[16], &[5], &[1], &[16]],
        om_scripts::text_node(&[], "Marker"),
    );
    root.put(&[&[16], &[5], &[1], &[33]], om_scripts::text_node(&[], ""));
    let compiled = om_formula::compile(
        &format!("{result:?}"),
        &om_formula::Context::new(&NameTable::default()),
    )
    .unwrap();
    root.put(
        &[&[16], &[5], &[1], &[35], &[6]],
        OwnedNode::leaf(Vec::new(), om_formula::polish::encode(&compiled.expr)),
    );
}

/// Mark field `field` of Tasks value-indexed (schema-path.md 5.5).
fn indexed(root: &mut OwnedNode, field: u64) {
    fixture::set_field_options(root, TABLE, field, 0x8000_0000);
}

/// Root and Related: Root's Tasks::Name relates to Remote Tasks::Name and
/// Related's Tasks::Name to Back Tasks::Name. Related's record 1 counts 37,
/// its Setting, `Marker ()` and `$$marker` are its own, and its File Info
/// reads its file's name, fields, functions, ExecuteSQL and Back Tasks.
fn files() -> Vec<(&'static str, OwnedNode)> {
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference(&mut root, (MISSING, "Unavailable Tasks"), 2, "Not hosted");
    // A source key above 127 and a cycle back to the root file must not
    // open another session for the root file.
    reference(&mut child, (BACK, "Back Tasks"), 0x8081, ROOT);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, NAME), 0, (BACK, NAME));
    put(&mut child, 1, COUNT, "37");
    indexed(&mut child, NAME);
    global(&mut root, "root-global");
    global(&mut child, "child-global");
    function(&mut root, "root-function");
    function(&mut child, "child-function");
    let mut names = fixture::names();
    names.add_custom_function(1, "Marker", 0);
    names.add_occurrence(BACK, "Back Tasks", TABLE);
    fixture::add_field(&mut child, TABLE, INFO, "File Info", 2, 1, 0, 0x2_0020, 0);
    fixture::add_formula(
        &mut child,
        &names,
        (TABLE, OCCURRENCE),
        INFO,
        5,
        "Get ( FileName ) & \"|\" & GetField ( \"Count\" ) & \"|\" & Evaluate ( \"Marker()\" ) \
         & \"|\" & ExecuteSQL ( \"SELECT SUM(\\\"Count\\\") FROM Tasks\" ; \"\" ; \"\" ) & \"|\" \
         & Back Tasks::Count & \"|\" & Let ( $$marker = \"child\" ; $$marker )",
    );
    vec![(ROOT, root), (RELATED, child)]
}

/// Evaluate formulas in Tasks record 1 of `provider`'s file, as the window
/// does: reading what is missing from the provider.
struct Window<'p> {
    provider: RefCell<&'p mut NativeProvider>,
    schema: std::sync::Arc<CalcSchema>,
    cache: RefCell<CalcData>,
}

impl<'p> Window<'p> {
    fn new(provider: &'p mut NativeProvider) -> Self {
        let schema = provider.calc_schema().unwrap().unwrap();
        Self {
            provider: RefCell::new(provider),
            schema,
            cache: RefCell::default(),
        }
    }

    /// Each of `formulas`' result: its code and display.
    fn read(&self, formulas: &[&str]) -> Vec<(i32, String)> {
        let fetch = |needs: &[CalcNeed]| self.provider.borrow_mut().calc_data(needs).ok();
        let source = CachedSource::new(&self.cache).with_fetch(&fetch);
        let session = SessionInfo {
            file_name: "Root".into(),
            account_name: ACCOUNT.into(),
            ..SessionInfo::for_this_machine()
        };
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let mut host = FileHost::new(
            FileCalc::new(&self.schema, &source, Date::from_ymd(2026, 10, 29).unwrap()),
            &session,
            &mut locals,
            &mut globals,
        );
        formulas
            .iter()
            .map(|text| {
                let result = evaluate_text(text, &mut host, &Context::record(OCCURRENCE, 1));
                (result.code(), display(&result.value, &host))
            })
            .collect()
    }
}

#[test]
fn related_files_keep_their_records_globals_functions_and_names() {
    let host = Host::new("separate", files());
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let mut catalog_lists = Vec::new();
    for (file, transaction) in host.captures.transactions() {
        if file != RELATED {
            continue;
        }
        match transaction.first().copied() {
            Some(92) => {
                let list = om_protocol::parse_download_list_request(&transaction).unwrap();
                if list.key_path.is_empty() {
                    assert_eq!(list.flag, 0);
                    catalog_lists.push(om_protocol::key_list_keys(&list.payload).unwrap());
                }
            }
            Some(90) => {
                let download = om_protocol::parse_download_request(&transaction).unwrap();
                assert!(
                    ![3, 16, 32, 33]
                        .iter()
                        .any(|key| download.key_path == [vec![*key]]),
                    "root catalogs are read in one list: {:?}",
                    download.key_path
                );
            }
            _ => {}
        }
    }
    assert_eq!(catalog_lists, [vec![vec![3], vec![16], vec![32], vec![33]]]);
    let remote = schema
        .table_of(REMOTE)
        .expect("Remote Tasks is resolved")
        .id;
    assert_ne!(remote, TABLE, "the files' Tasks tables have different ids");
    assert!(
        schema.table_of(MISSING).is_none(),
        "an occurrence of a file that is not open never shows Root's table with the same id"
    );
    assert_eq!(
        schema.tables().count(),
        4,
        "Related's Back Tasks shows Root's own Tasks"
    );
    let formula = schema
        .field(REMOTE, INFO)
        .unwrap()
        .formula
        .as_ref()
        .unwrap();
    let context = formula.occurrence().unwrap();
    assert_ne!(context, OCCURRENCE);
    assert_eq!(schema.table_of(context).unwrap().id, remote);
    assert!(
        provider
            .related_file_warnings()
            .iter()
            .any(|w| w.contains("“Not hosted”")),
        "{:?}",
        provider.related_file_warnings()
    );
    let initial = provider.calc_data(&[]).unwrap();
    assert!(initial.records.is_empty() && initial.tables.is_empty());
    assert!(
        host.captures.row_downloads().is_empty(),
        "opening the related files reads none of their records"
    );

    let window = Window::new(&mut provider);
    let results = window.read(&[
        "Count",
        "Remote Tasks::Count",
        "Setting & \"|\" & Remote Tasks::Setting",
        "Marker()",
        "Let ( $$marker = \"root\" ; $$marker )",
        "Remote Tasks::File Info",
        "$$marker",
        "Count & \"|\" & Get ( FileName ) & \"|\" & Marker()",
        // Root's Design functions and ExecuteSQL name its own occurrences,
        // never Related's under their `Related::` names.
        "TableNames ( Get ( FileName ) )",
        "FieldNames ( Get ( FileName ) ; \"Remote Tasks\" )",
        "GetFieldName ( Remote Tasks::Count )",
        "ExecuteSQL ( \"SELECT SUM(\\\"Count\\\") FROM \\\"Remote Tasks\\\"\" ; \"\" ; \"\" )",
    ]);
    let values: Vec<&str> = results.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(
        values,
        [
            "3",
            "37",
            "root-global|child-global",
            "root-function",
            "root",
            // Related's own name, record, function, tables (37 + 5) and
            // Back Tasks (Root's record 1), and its own $$marker.
            "Related|37|child-function|42|3|child",
            "root",
            "3|Root|root-function",
            "Notes\rRemote Tasks\rTasks\rUnavailable Tasks",
            "Name\rCount\rDue\rSetting\rDoubled\rCreator\rCode\rFile Info",
            "Remote Tasks::Count",
            "42",
        ]
    );
    assert!(results.iter().all(|(code, _)| *code == 0), "{results:?}");
    let cache = window.cache.borrow();
    assert!(cache.records.contains_key(&(TABLE, 1)));
    assert!(cache.records.contains_key(&(remote, 1)));
    drop(cache);

    // Related's records were read on its own connection, by its own ids.
    let rows = host.captures.row_downloads();
    assert!(rows
        .iter()
        .any(|(file, table, _)| file == RELATED && *table == TABLE));
    assert!(
        host.captures
            .downloaded_tables()
            .iter()
            .all(|(_, table)| *table < 0x1_0000_0000),
        "no client-only id is sent: {:?}",
        host.captures.downloaded_tables()
    );
}

#[test]
fn an_unavailable_related_file_reads_as_missing_not_empty() {
    let host = Host::new("missing-values", files());
    let mut provider = host.open(ROOT);
    let window = Window::new(&mut provider);
    let results = window.read(&[
        "Unavailable Tasks::Name",
        "IsEmpty ( Unavailable Tasks::Name )",
        "Count ( Unavailable Tasks::Name )",
    ]);
    for (text, (code, display)) in ["Unavailable Tasks::Name", "IsEmpty", "Count"]
        .iter()
        .zip(&results)
    {
        assert_ne!(*code, 0, "{text}: {display:?}");
        assert_eq!(display, "?", "{text}");
    }
}

/// Dates written in each file's own formats (Root month/day, Related
/// day/month) relate by the day they name, through either file's index.
#[test]
fn indexed_date_relationships_find_in_the_related_files_formats() {
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference(&mut child, (BACK, "Back Tasks"), 1, "Root");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, DUE), 0, (REMOTE, DUE));
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, DUE), 0, (BACK, DUE));
    let mut locale = om_data::calc::Locale::en_us();
    locale.date_order = om_collate::DateOrder::DayMonthYear;
    child.put(
        &[&[2], &[0x0d]],
        OwnedNode::leaf(Vec::new(), locale.to_record()),
    );
    indexed(&mut root, DUE);
    indexed(&mut child, DUE);
    // March 4 in Root; March 4 and April 3 in Related.
    put(&mut root, 1, DUE, "03/04/2026");
    put(&mut child, 1, DUE, "04/03/2026");
    put(&mut child, 2, DUE, "03/04/2026");
    let host = Host::new("dates", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    // The first find on a missing index builds it (query-program.md 7.1).
    for answered in [false, true] {
        let data = provider
            .calc_data(&[
                CalcNeed::Lookup {
                    table: TABLE,
                    field: DUE,
                    text: "1/1/2000".into(),
                },
                CalcNeed::Lookup {
                    table: remote,
                    field: DUE,
                    text: "1/1/2000".into(),
                },
            ])
            .unwrap();
        assert!(data.lookups.values().all(|ids| ids.is_some() == answered));
    }
    let downloads = host.captures.row_downloads().len();
    let data = provider
        .calc_data(&[CalcNeed::Related {
            from: OCCURRENCE,
            record: 1,
            to: REMOTE,
        }])
        .unwrap();
    assert_eq!(&*data.related[&(OCCURRENCE, 1, REMOTE)], &[1]);
    let from = schema.occurrence_named("Related::Tasks").unwrap().id;
    let to = schema.occurrence_named("Related::Back Tasks").unwrap().id;
    let data = provider
        .calc_data(&[CalcNeed::Related {
            from,
            record: 1,
            to,
        }])
        .unwrap();
    assert_eq!(&*data.related[&(from, 1, to)], &[1]);
    let rows = host.captures.row_downloads();
    assert!(
        rows[downloads..].iter().all(|(_, _, count)| *count == 1),
        "only the parents and the matching rows are read: {rows:?}"
    );
    // ExecuteSQL's `=` on the related occurrence finds March 4 too.
    let window = Window::new(&mut provider);
    assert_eq!(
        window.read(&[
            "ExecuteSQL ( \"SELECT Name FROM \\\"Remote Tasks\\\" WHERE Due = ?\" ; \"\" ; \"\" ; \
             Date ( 3 ; 4 ; 2026 ) )"
        ]),
        [(0, "Write tests".to_string())]
    );
}

/// A relationship of Related's graph compares text in Related's language,
/// as its own calculations do (`FileCalc::cx_for`): in a Swedish file
/// "Wall" is "Vall" (Swedish keys `w` as `v`), in Root's English it is not.
#[test]
fn a_related_files_relationships_compare_in_its_own_language() {
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference(&mut child, (BACK, "Back Tasks"), 1, "Root");
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, NAME), 0, (BACK, NAME));
    let mut swedish = om_data::calc::Locale::en_us();
    swedish.language = om_data::calc::Language::Swedish;
    child.put(
        &[&[2], &[0x0d]],
        OwnedNode::leaf(Vec::new(), swedish.to_record()),
    );
    put(&mut child, 1, NAME, "Wall");
    put(&mut root, 1, NAME, "Vall");
    let host = Host::new("language", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let from = schema.occurrence_named("Related::Tasks").unwrap().id;
    let to = schema.occurrence_named("Related::Back Tasks").unwrap().id;
    let data = provider
        .calc_data(&[CalcNeed::Related {
            from,
            record: 1,
            to,
        }])
        .unwrap();
    assert_eq!(&*data.related[&(from, 1, to)], &[1]);
}

#[test]
fn numeric_relationships_find_on_the_related_files_index() {
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Numeric Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, COUNT), 0, (REMOTE, COUNT));
    indexed(&mut child, COUNT);
    fixture::add_tasks(&mut child, 3..=400);
    put(&mut child, 1, NAME, "Related first");
    for id in 3..=400 {
        let number = match id {
            3 => "3.0",
            4 => "2.999999999999999999",
            5 => "4",
            _ => "1000",
        };
        put(&mut child, id, COUNT, number);
    }
    let host = Host::new("numeric", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let need = CalcNeed::Related {
        from: OCCURRENCE,
        record: 1,
        to: REMOTE,
    };
    // The first find builds Related's index; the scan answers it.
    let first = provider.calc_data(std::slice::from_ref(&need)).unwrap();
    assert_eq!(&*first.related[&(OCCURRENCE, 1, REMOTE)], &[1, 3]);
    let before = host.captures.row_downloads().len();
    let data = provider.calc_data(std::slice::from_ref(&need)).unwrap();
    assert_eq!(&*data.related[&(OCCURRENCE, 1, REMOTE)], &[1, 3]);
    let locale = om_data::calc::Locale::en_us();
    let cx = om_data::calc::Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
    let names: Vec<String> = [1, 3]
        .iter()
        .map(|id| {
            data.records[&(table, *id)]
                .as_ref()
                .unwrap()
                .get(NAME, 1)
                .unwrap()
                .to_text(&cx)
                .into_string()
        })
        .collect();
    assert_eq!(names, ["Related first", "Task 3"]);
    // Candidates the index proposed but the comparison refused.
    assert!(!data.records.contains_key(&(table, 4)));
    assert!(!data.records.contains_key(&(table, 5)));
    let rows = host.captures.row_downloads();
    assert!(
        rows[before..].iter().map(|(_, _, n)| n).sum::<usize>() < 10,
        "the index, not a scan, answered: {rows:?}"
    );
    let queries = host.captures.with_opcode(om_protocol::Opcode::QueryProgram);
    assert!(!queries.is_empty());
    for (file, transaction) in queries {
        assert_eq!(file, RELATED, "Root's connection is not asked");
        let query = om_protocol::parse_query_request(&transaction).unwrap();
        let program = om_query::Program::parse(&query.program).unwrap();
        for instruction in program.instructions {
            if let Ok(table) = instruction.table() {
                assert_eq!(
                    om_hbam::slot::be_uint(&table),
                    Some(TABLE),
                    "the find names Related's own table id"
                );
            }
        }
    }
}

#[test]
fn finds_through_related_files_are_refused_before_anything_is_sent() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    let mut root = fixture::tree(&[], true);
    let child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::add_relationship(
        &mut root,
        10,
        (REMOTE, NAME),
        0,
        (fixture::NOTES_OCCURRENCE, fixture::BODY),
    );
    let host = Host::new("find-hop", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let queries = host
        .captures
        .with_opcode(om_protocol::Opcode::QueryProgram)
        .len();
    // A field of the related file's table, one of Root's own table
    // reached through it, and the latter beside a criterion of the
    // layout's own table, in the same request or the next.
    let cases: [&[&[(&str, &str)]]; 4] = [
        &[&[("Remote Tasks::Name", "Hi")]],
        &[&[("Notes::Body", "Hi")]],
        &[&[("Name", "Write tests"), ("Notes::Body", "Hi")]],
        &[&[("Name", "Write tests")], &[("Notes::Body", "Hi")]],
    ];
    for criteria in cases {
        let requests = criteria
            .iter()
            .map(|criteria| {
                let mut request = FindRequest::default();
                for (name, text) in *criteria {
                    request.set(name, *text);
                }
                request
            })
            .collect();
        let error = provider
            .change_found_set(
                u32::try_from(fixture::LAYOUT).unwrap(),
                FoundSetChange::Find {
                    requests,
                    kind: FindKind::Replace,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, Some(3), "{criteria:?}: {error}");
        assert!(error.message.contains("related files"), "{error}");
    }
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::QueryProgram)
            .len(),
        queries,
        "no find was sent"
    );
}

/// `LayoutDef` of a layout showing `occurrence` (layout-model.md 4.1).
fn layout_def(occurrence: u64) -> Vec<u8> {
    let mut alias = serial::decode(&uint_segment(occurrence)).unwrap();
    let mut out = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        if alias == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

/// The caller's Notes layout shows the peer's Tasks, with colliding native
/// table/field/record ids but different values and stored date order.
fn find_files() -> Vec<(&'static str, OwnedNode)> {
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    root.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    let mut peer = fixture::tree(&[], true);
    put(&mut peer, 1, NAME, "Other");
    put(&mut peer, 2, NAME, "Write tests");
    put(&mut peer, 2, COUNT, "37");
    put(&mut peer, 2, DUE, "17/02/2014");
    fixture::add_field(&mut peer, TABLE, 90, "At", 1, 4, 0, 0, 0);
    put(&mut peer, 2, 90, "21:26:57");
    for field in [NAME, COUNT, DUE, 90] {
        indexed(&mut peer, field);
    }
    let mut locale = om_data::calc::Locale::en_us();
    locale.date_order = om_collate::DateOrder::DayMonthYear;
    peer.put(&[&[2], &[13]], OwnedNode::leaf([], locale.to_record()));
    fixture::add_field(&mut peer, TABLE, 91, "Found context", 2, 1, 0, 0x2_0020, 0);
    fixture::add_formula(
        &mut peer,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        91,
        5,
        "Get ( FoundCount ) & \"|\" & Get ( RecordNumber ) & \"|\" & GetNthRecord ( Name ; 1 )",
    );
    vec![(ROOT, root), (RELATED, peer)]
}

fn find_request(criteria: &[(&str, &str)], omit: bool) -> om_data::found::FindRequest {
    om_data::found::FindRequest {
        omit,
        criteria: criteria
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    }
}

#[test]
fn imported_layout_find_uses_owner_indexes_fallback_and_typed_values() {
    use om_data::found::{FindKind, FoundSetChange};
    let host = Host::configured(
        "imported-find-typed",
        find_files(),
        om_server::Server::with_late_index_builds,
    );
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let downloads = || {
        host.captures
            .row_downloads()
            .iter()
            .filter(|(file, table, _)| file == RELATED && *table == TABLE)
            .map(|(_, _, n)| n)
            .sum::<usize>()
    };
    // The first request builds four indexes and streams the peer's values;
    // the second uses those indexes without downloading any record bodies.
    for expected_downloads in [2, 0] {
        let before = downloads();
        let query_start = host
            .captures
            .with_opcode(om_protocol::Opcode::QueryProgram)
            .len();
        let result = provider
            .change_found_set(
                2,
                FoundSetChange::Find {
                    requests: vec![find_request(
                        &[
                            ("Remote Tasks::Name", "==Write tests"),
                            ("Count", ">=30"),
                            ("Due", "2/17/2014"),
                            ("At", "21:26:57"),
                        ],
                        false,
                    )],
                    kind: FindKind::Replace,
                },
            )
            .unwrap();
        assert_eq!((result.found, result.total), (1, 2));
        assert_eq!(downloads() - before, expected_downloads);
        let queries = host.captures.with_opcode(om_protocol::Opcode::QueryProgram);
        assert!(queries.len() > query_start);
        for (file, transaction) in &queries[query_start..] {
            assert_eq!(file, RELATED);
            let query = om_protocol::parse_query_request(transaction).unwrap();
            let program = om_query::Program::parse(&query.program).unwrap();
            for instruction in program.instructions {
                if let Ok(table) = instruction.table() {
                    assert_eq!(om_hbam::slot::be_uint(&table), Some(TABLE));
                }
            }
        }
        let page = provider.records(2, 0, 10).unwrap();
        assert_eq!(page.records.iter().map(|r| r.id).collect::<Vec<_>>(), [2]);
        let context = page
            .fields
            .iter()
            .position(|f| f.name == "Found context")
            .unwrap();
        assert_eq!(
            page.records[0].values[context].as_deref(),
            Some("1|1|Write tests")
        );
    }
    // Exact client filtering must read dates in the owner's DMY order too.
    provider
        .change_found_set(
            2,
            FoundSetChange::Find {
                requests: vec![find_request(
                    &[("Name", "==Write t*"), ("Due", "2/*/2014")],
                    false,
                )],
                kind: FindKind::Replace,
            },
        )
        .unwrap();
    assert_eq!(provider.records(2, 0, 10).unwrap().records[0].id, 2);
    // Identical criteria on the root find its own record, not peer record 2.
    provider
        .change_found_set(
            1,
            FoundSetChange::Find {
                requests: vec![find_request(&[("Name", "==Write tests")], false)],
                kind: FindKind::Replace,
            },
        )
        .unwrap();
    assert_eq!(provider.records(1, 0, 10).unwrap().records[0].id, 1);
}

#[test]
fn imported_layout_find_combines_requests_and_preserves_windows_on_refusal() {
    use om_data::found::{FindKind, FoundSetChange};
    let host = Host::new("imported-find-sets", find_files());
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let ids = |p: &mut NativeProvider| {
        p.records(2, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    let find = |p: &mut NativeProvider, kind, requests| {
        p.change_found_set(2, FoundSetChange::Find { requests, kind })
    };
    let write = || find_request(&[("Name", "==Write tests")], false);
    let other = || find_request(&[("Name", "==Other")], false);
    find(&mut provider, FindKind::Replace, vec![write(), other()]).unwrap();
    assert_eq!(ids(&mut provider), [1, 2]);
    find(&mut provider, FindKind::Constrain, vec![write()]).unwrap();
    assert_eq!(ids(&mut provider), [2]);
    find(&mut provider, FindKind::Extend, vec![other()]).unwrap();
    assert_eq!(ids(&mut provider), [1, 2]);
    let omit = || find_request(&[("Name", "==Other")], true);
    find(
        &mut provider,
        FindKind::Replace,
        vec![write(), other(), omit()],
    )
    .unwrap();
    assert_eq!(ids(&mut provider), [2]);
    find(&mut provider, FindKind::Replace, vec![omit()]).unwrap();
    assert_eq!(ids(&mut provider), [2]);
    provider.use_window(2);
    assert_eq!(ids(&mut provider), [1, 2]);
    find(&mut provider, FindKind::Replace, vec![other()]).unwrap();
    provider.use_window(1);
    assert_eq!(ids(&mut provider), [2]);
    // Every later criterion is preflighted before any query, and no peer
    // occurrence or calculation is accidentally enabled by routing.
    for change in [
        FoundSetChange::Find {
            kind: FindKind::Extend,
            requests: vec![other(), find_request(&[("Tasks::Name", "Write")], true)],
        },
        FoundSetChange::Find {
            kind: FindKind::Replace,
            requests: vec![other(), find_request(&[("Found context", "1")], false)],
        },
    ] {
        let before = host
            .captures
            .with_opcode(om_protocol::Opcode::QueryProgram)
            .len();
        assert_eq!(
            provider.change_found_set(2, change).unwrap_err().code,
            Some(3)
        );
        assert_eq!(
            host.captures
                .with_opcode(om_protocol::Opcode::QueryProgram)
                .len(),
            before
        );
        assert_eq!(ids(&mut provider), [2]);
        assert_eq!(provider.calc_schema().unwrap().unwrap().file_name, "Root");
    }
    let error = find(
        &mut provider,
        FindKind::Replace,
        vec![find_request(&[("Name", "==Missing")], false)],
    )
    .unwrap_err();
    assert_eq!(error.code, Some(401));
    assert!(ids(&mut provider).is_empty());
    provider.use_window(2);
    assert_eq!(ids(&mut provider), [1]);
    provider
        .change_found_set(2, FoundSetChange::ShowOmitted)
        .unwrap();
    assert_eq!(ids(&mut provider), [2]);
    provider
        .change_found_set(2, FoundSetChange::ShowAll)
        .unwrap();
    provider
        .change_found_set(2, FoundSetChange::Omit(vec![2]))
        .unwrap();
    assert_eq!(ids(&mut provider), [1]);
}

/// Extend the imported Find fixture with stored scalar sort values. Root's
/// colliding Count/Due/At fields are text, while the owner has numeric/date/time
/// metadata and day/month/year dates. Three equal numbers test stable ties.
fn sort_files() -> Vec<(&'static str, OwnedNode)> {
    let mut files = find_files();
    for (field, name) in [(COUNT, "Count"), (DUE, "Due"), (90, "At")] {
        fixture::add_field(&mut files[0].1, TABLE, field, name, 1, 1, 0, 0, 0);
    }
    let peer = &mut files[1].1;
    for (id, count, due, at) in [
        (1, "10", "02/03/2026", "12:01"),
        (2, "2", "11/02/2026", "2:00"),
        (3, "2", "11/02/2026", "2:00"),
        (4, "2", "02/03/2026", "9:00"),
        (5, "100", "01/01/2026", "23:59"),
    ] {
        put(peer, id, NAME, if id == 5 { "Other" } else { "Peer" });
        put(peer, id, COUNT, count);
        put(peer, id, DUE, due);
        put(peer, id, 90, at);
        put(
            peer,
            id,
            fixture::DOUBLED,
            &(count.parse::<u32>().unwrap() * 2).to_string(),
        );
    }
    let ids: om_hbam::RidList = (1..=5).collect();
    peer.put(&[&uint_segment(TABLE), &[0x0d]], ids.to_node([]));
    // Unsupported fields are present beside supported keys so preflight must
    // reject the whole request, including a later key and summary options.
    fixture::add_field(peer, TABLE, 92, "Repeated", 1, 1, 0, 0, 0);
    let mut options = vec![0; 26];
    options[0] = 1;
    options[1] = 1;
    options[4..8].copy_from_slice(&21u32.to_be_bytes());
    options[24..26].copy_from_slice(&2u16.to_be_bytes());
    peer.put(
        &[&uint_segment(TABLE), &[3], &[5], &[92], &[2]],
        OwnedNode::leaf([], options),
    );
    fixture::add_field(peer, TABLE, 93, "Container", 1, 6, 0, 0, 0);
    fixture::add_field(peer, TABLE, 94, "Summary", 3, 2, 0, 0, 0);
    files
}

fn imported_ids(provider: &mut NativeProvider) -> Vec<i64> {
    provider
        .records(2, 0, 10)
        .unwrap()
        .records
        .iter()
        .map(|r| r.id)
        .collect()
}

#[test]
fn imported_layout_sort_uses_owner_types_subset_and_stable_multifield_order() {
    use om_data::found::{FindKind, FoundSetChange, SortField, SortOrder::*};
    let host = Host::new("imported-sort-types", sort_files());
    let mut provider = host.open(ROOT);
    let table = provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    let downloads = |file: &str| {
        host.captures
            .row_downloads()
            .iter()
            .filter(|(f, t, _)| f == file && *t == TABLE)
            .map(|(_, _, n)| n)
            .sum::<usize>()
    };
    for (name, expected) in [
        ("Count", vec![2, 3, 4, 1, 5]),
        ("Remote Tasks::Due", vec![5, 2, 3, 1, 4]),
        ("At", vec![2, 3, 4, 1, 5]),
        ("Doubled", vec![2, 3, 4, 1, 5]),
    ] {
        provider
            .change_found_set(2, FoundSetChange::Unsort)
            .unwrap();
        let before = (downloads(ROOT), downloads(RELATED));
        let result = provider
            .change_found_set(
                2,
                FoundSetChange::Sort(vec![SortField::new(name, Ascending)]),
            )
            .unwrap();
        assert!(result.sorted);
        assert_eq!((result.found, result.total), (5, 5));
        assert_eq!(
            (downloads(ROOT), downloads(RELATED)),
            (before.0, before.1 + 5),
            "only owner bodies are read"
        );
        assert_eq!(imported_ids(&mut provider), expected, "{name}");
        let data = provider
            .calc_data(&[CalcNeed::FoundSet {
                occurrence: REMOTE,
                table,
            }])
            .unwrap();
        let schema = provider.calc_schema().unwrap().unwrap();
        let field = schema
            .field(
                REMOTE,
                if name.ends_with("Due") {
                    DUE
                } else if name == "Count" {
                    COUNT
                } else if name == "At" {
                    90
                } else {
                    fixture::DOUBLED
                },
            )
            .unwrap()
            .id;
        assert_eq!(
            data.sorts[&REMOTE].as_ref(),
            &[field],
            "qualified keys expose native field ids"
        );
    }
    provider
        .change_found_set(
            2,
            FoundSetChange::Find {
                requests: vec![find_request(&[("Name", "==Peer")], false)],
                kind: FindKind::Replace,
            },
        )
        .unwrap();
    provider
        .change_found_set(2, FoundSetChange::Omit(vec![1]))
        .unwrap();
    provider
        .change_found_set(
            2,
            FoundSetChange::Sort(vec![SortField::new("Due", Descending)]),
        )
        .unwrap();
    assert_eq!(imported_ids(&mut provider), [4, 2, 3]);
    provider
        .change_found_set(2, FoundSetChange::Records(vec![3, 4, 2]))
        .unwrap();
    let before = (downloads(ROOT), downloads(RELATED));
    provider
        .change_found_set(
            2,
            FoundSetChange::Sort(vec![
                SortField::new("Remote Tasks::Count", Ascending),
                SortField::new("Due", Descending),
            ]),
        )
        .unwrap();
    assert_eq!(
        (downloads(ROOT), downloads(RELATED)),
        (before.0, before.1 + 3),
        "only current found records are downloaded"
    );
    assert_eq!(imported_ids(&mut provider), [4, 3, 2]);
    provider
        .change_found_set(2, FoundSetChange::Omit(vec![4]))
        .unwrap();
    assert_eq!(imported_ids(&mut provider), [3, 2]);
    assert!(
        !provider
            .change_found_set(2, FoundSetChange::Unsort)
            .unwrap()
            .sorted
    );
    assert_eq!(imported_ids(&mut provider), [2, 3]);
    assert!(
        !provider
            .change_found_set(2, FoundSetChange::ShowOmitted)
            .unwrap()
            .sorted
    );
    assert_eq!(imported_ids(&mut provider), [1, 4, 5]);
    provider
        .change_found_set(2, FoundSetChange::ShowAll)
        .unwrap();
    assert_eq!(imported_ids(&mut provider), [1, 2, 3, 4, 5]);
    // The local file still sorts on its own fields and bodies.
    provider
        .change_found_set(
            1,
            FoundSetChange::Sort(vec![SortField::new("Name", Ascending)]),
        )
        .unwrap();
    let root = provider.records(1, 0, 10).unwrap();
    assert_eq!(
        root.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(provider.calc_schema().unwrap().unwrap().file_name, "Root");
    assert!(
        host.captures
            .row_downloads()
            .iter()
            .all(|(_, table, _)| *table == TABLE),
        "client-only table ids never go on the wire"
    );
}

#[test]
fn imported_layout_sort_refusal_preserves_sorted_windows_and_caller_context() {
    use om_data::found::{FoundSetChange, SortField, SortOrder::*, SummaryFunction, SummaryOrder};
    let host = Host::new("imported-sort-refusal", sort_files());
    let mut provider = host.open(ROOT);
    let table = provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    provider.use_window(1);
    provider
        .change_found_set(2, FoundSetChange::Records(vec![3, 2, 1]))
        .unwrap();
    provider
        .change_found_set(
            2,
            FoundSetChange::Sort(vec![SortField::new("Count", Ascending)]),
        )
        .unwrap();
    assert_eq!(imported_ids(&mut provider), [3, 2, 1]);
    provider.copy_window(1, 2);
    provider.use_window(2);
    provider
        .change_found_set(2, FoundSetChange::Omit(vec![3]))
        .unwrap();
    provider
        .change_found_set(2, FoundSetChange::Unsort)
        .unwrap();
    assert_eq!(imported_ids(&mut provider), [1, 2]);
    provider.use_window(1);
    let mut summary = SortField::new("Name", Ascending);
    summary.summary = Some(SummaryOrder {
        function: SummaryFunction::Total,
        field: "Count".into(),
    });
    for (key, code) in [
        (SortField::new("Tasks::Name", Ascending), 3),
        (SortField::new("Remote Tasks::Tasks::Name", Ascending), 3),
        (SortField::new("Found context", Ascending), 3),
        (SortField::new("Repeated", Ascending), 3),
        (SortField::new("Container", Ascending), 3),
        (SortField::new("Summary", Ascending), 3),
        (SortField::new("Setting", Ascending), 3),
        (SortField::new("Missing", Ascending), 102),
        (summary, 3),
    ] {
        let before = host.captures.row_downloads();
        let error = provider
            .change_found_set(
                2,
                FoundSetChange::Sort(vec![SortField::new("Count", Descending), key]),
            )
            .unwrap_err();
        assert_eq!(error.code, Some(code));
        assert_eq!(
            host.captures.row_downloads(),
            before,
            "all keys are checked before any body IO"
        );
        assert_eq!(imported_ids(&mut provider), [3, 2, 1]);
        let data = provider
            .calc_data(&[CalcNeed::FoundSet {
                occurrence: REMOTE,
                table,
            }])
            .unwrap();
        assert_eq!(data.sorts[&REMOTE].as_ref(), &[COUNT]);
        assert_eq!(provider.calc_schema().unwrap().unwrap().file_name, "Root");
    }
    provider.use_window(2);
    assert_eq!(imported_ids(&mut provider), [1, 2]);
}

#[test]
fn related_file_layouts_browse_and_expose_only_supported_edit_capabilities() {
    let mut files = files();
    fixture::add_field(
        &mut files[1].1,
        TABLE,
        19,
        "Found context",
        2,
        1,
        0,
        0x2_0020,
        0,
    );
    fixture::add_formula(
        &mut files[1].1,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        19,
        5,
        "Get ( FoundCount ) & \"|\" & Get ( RecordNumber ) & \"|\" & GetNthRecord ( Name ; 1 )",
    );
    // Layout 2 shows Remote Tasks.
    files[0].1.put(
        &[&[4], &[5], &uint_segment(fixture::NOTES_LAYOUT), &[2]],
        OwnedNode::leaf(Vec::new(), layout_def(REMOTE)),
    );
    let host = Host::new("writes", files);
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let layout = u32::try_from(fixture::NOTES_LAYOUT).unwrap();
    assert!(provider.render_layout(layout).unwrap().is_some());
    let capabilities = provider.record_capabilities(layout).unwrap();
    assert!(capabilities.update);
    assert!(capabilities.create && capabilities.delete);
    assert!(
        capabilities
            .editable_fields
            .as_ref()
            .unwrap()
            .contains("Name")
            && capabilities
                .editable_fields
                .as_ref()
                .unwrap()
                .contains("Code")
    );
    assert!(
        !capabilities
            .editable_fields
            .as_ref()
            .unwrap()
            .contains("Setting")
            && !capabilities
                .editable_fields
                .as_ref()
                .unwrap()
                .contains("File Info")
    );
    let page = provider.records(layout, 0, 1).unwrap();
    let value = |page: &om_data::RecordPage, name: &str| {
        let field = page.fields.iter().position(|f| f.name == name).unwrap();
        page.records[0].values[field].clone().unwrap_or_default()
    };
    assert_eq!(page.total, 2);
    assert_eq!(page.records[0].id, 1);
    assert_eq!(value(&page, "Count"), "37");
    assert_eq!(value(&page, "Setting"), "child-global");
    assert_eq!(
        value(&page, "File Info"),
        "Related|37|child-function|42|3|child"
    );
    let next = provider.records(layout, 1, 1).unwrap();
    assert_eq!(next.records[0].id, 2);
    assert_eq!(value(&next, "Name"), "Ship");
    provider
        .change_found_set(layout, om_data::found::FoundSetChange::Omit(vec![1]))
        .unwrap();
    let omitted = provider.records(layout, 0, 10).unwrap();
    assert_eq!(omitted.total, 1);
    assert_eq!(omitted.records[0].id, 2);
    assert_eq!(value(&omitted, "Found context"), "1|1|Ship");
    provider
        .change_found_set(layout, om_data::found::FoundSetChange::Records(vec![2, 1]))
        .unwrap();
    let ordered = provider.records(layout, 0, 1).unwrap();
    assert_eq!(value(&ordered, "Found context"), "2|1|Ship");
    let own = provider.records(1, 0, 1).unwrap();
    assert_eq!(own.total, 2);
    assert_eq!(value(&own, "Count"), "3");
    provider.use_window(2);
    let other_window = provider.records(layout, 0, 1).unwrap();
    assert_eq!(other_window.total, 2);
    assert_eq!(other_window.records[0].id, 1);
    assert_eq!(value(&other_window, "Found context"), "2|1|Write tests");
    assert!(host
        .captures
        .downloaded_tables()
        .iter()
        .all(|(_, table)| *table < 0x1_0000_0000));
    let refused = |result: Result<(), om_data::DataError>| {
        let error = result.unwrap_err();
        assert_eq!(error.code, Some(3), "{error}");
        assert!(
            error
                .message
                .contains("“Remote Tasks” shows a table of a related file"),
            "{error}"
        );
    };
    let values = om_data::FieldChanges::from([("Name".to_string(), Some("Changed".to_string()))]);
    assert!(provider.supports_record_import());
    // Import (new or existing rows) stays refused; New Record is owned.
    for record in [None, Some(1)] {
        refused(
            provider
                .import_record(
                    layout,
                    record,
                    &[(NAME, Some("Changed".into()))].into(),
                    true,
                )
                .map(drop),
        );
    }
    provider.begin_transaction(Default::default()).unwrap();
    assert_eq!(
        provider
            .create_record(layout, values.clone())
            .unwrap_err()
            .code,
        Some(3)
    );
    assert_eq!(
        provider.update_record(layout, 1, values).unwrap_err().code,
        Some(3)
    );
    // A deletion is held in the related file's part of the transaction:
    // it locks the record and commits nothing before Commit Transaction.
    provider.delete_record(layout, 1).unwrap();
    provider.revert_transaction().unwrap();
    refused(provider.set_next_serial_value(REMOTE, NAME, "10"));
    for op in [
        om_protocol::Opcode::HostCreateRecord,
        om_protocol::Opcode::HostCommit,
        om_protocol::Opcode::HostGetSerial,
    ] {
        assert!(host.captures.with_opcode(op).is_empty(), "{op:?} was sent");
    }
    // Root's own layout still writes.
    let created = provider
        .create_record(
            u32::try_from(fixture::LAYOUT).unwrap(),
            om_data::FieldChanges::from([("Name".to_string(), Some("New".to_string()))]),
        )
        .unwrap();
    assert!(created.id > 2);
}

#[test]
fn imported_layout_rules_check_custom_function_record_context_before_writes() {
    for (call, selector, supported) in [
        ("Get ( RecordID )", "RecordID", false),
        ("ActiveContext()", "RecordID", false),
        ("NestedContext()", "RecordID", false),
        ("NestedContext()", "FileName", true),
    ] {
        let mut root = fixture::tree(&[], true);
        reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
        root.put(
            &[&[4], &[5], &[2], &[2]],
            OwnedNode::leaf([], layout_def(REMOTE)),
        );
        let mut peer = fixture::tree(&[], true);
        fixture::add_field(&mut peer, TABLE, 20, "Audit record", 1, 1, 0, 0x88, 0);
        let mut names = fixture::names();
        names.add_custom_function(1, "ActiveContext", 0);
        names.add_custom_function(2, "NestedContext", 0);
        for (id, name, body) in [
            (1, "ActiveContext", format!("Get ( {selector} )")),
            (2, "NestedContext", "ActiveContext()".into()),
        ] {
            peer.put(
                &[&[16], &[5], &[id], &[16]],
                om_scripts::text_node(&[], name),
            );
            peer.put(&[&[16], &[5], &[id], &[33]], om_scripts::text_node(&[], ""));
            let formula = om_formula::compile(&body, &om_formula::Context::new(&names)).unwrap();
            peer.put(
                &[&[16], &[5], &[id], &[35], &[6]],
                OwnedNode::leaf([], om_formula::polish::encode(&formula.expr)),
            );
        }
        fixture::add_formula(
            &mut peer,
            &names,
            (TABLE, OCCURRENCE),
            20,
            5,
            &format!("Name & \"|\" & {call}"),
        );
        let host = Host::new("layout-rule-context", vec![(ROOT, root), (RELATED, peer)]);
        let mut provider = host.open(ROOT);
        let page = provider.records(2, 0, 1).unwrap();
        // Creation shares the plan, and so the recursive pre-effect guard.
        assert_eq!(provider.record_capabilities(2).unwrap().create, supported);
        let result =
            provider.update_record(2, 1, [("Name".into(), Some("Peer edit".into()))].into());
        if supported {
            let row = result.unwrap();
            let audit = page
                .fields
                .iter()
                .position(|f| f.name == "Audit record")
                .unwrap();
            assert_eq!(row.values[audit].as_deref(), Some("Peer edit|Related"));
            assert_eq!(
                provider.records(2, 0, 1).unwrap().records[0].values[audit].as_deref(),
                Some("Peer edit|Related")
            );
            let files: Vec<_> = host
                .captures
                .with_opcode(om_protocol::Opcode::HostCommit)
                .into_iter()
                .map(|(file, _)| file)
                .collect();
            assert_eq!(files, [RELATED]);
        } else {
            let error = result.expect_err("unsupported context must refuse before effects");
            assert!(error.to_string().contains("Get() selector"), "{error}");
            let write = om_data::ScriptWrite {
                layout: 2,
                table: None,
                record: Some(1),
                cells: [(NAME, Some("Script peer edit".into()))].into(),
                fields: Default::default(),
                typed: Default::default(),
                skip_validation: false,
                privilege_set: None,
                record_number: None,
                prepared: None,
            };
            assert!(provider.write_record(&write).is_err());
            let error = provider.create_record(2, Default::default()).unwrap_err();
            assert!(error.to_string().contains("Get() selector"), "{error}");
            let new = om_data::ScriptWrite {
                record: None,
                ..write
            };
            assert!(provider.write_record(&new).is_err());
            for opcode in [
                om_protocol::Opcode::Lock,
                om_protocol::Opcode::HostCreateRecord,
                om_protocol::Opcode::HostCommit,
            ] {
                assert!(
                    host.captures.with_opcode(opcode).is_empty(),
                    "{call}: {opcode:?}"
                );
            }
            let name = page.fields.iter().position(|f| f.name == "Name").unwrap();
            assert_eq!(
                provider.records(2, 0, 1).unwrap().records[0].values[name].as_deref(),
                Some("Write tests")
            );
        }
    }
}

/// Browse and Set Field share the existing engine, using the peer's rules,
/// auto-entry, stored calculations and wire ids, even when root ids collide.
#[test]
fn imported_layout_edits_commit_on_the_owner_and_return_fresh_calculations() {
    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    // Same ids, different custom function and auto-entry definitions.
    for (index, marker) in [(0, "root"), (1, "peer")] {
        fixture::add_field(
            &mut files[index].1,
            TABLE,
            20,
            "Edited by",
            1,
            1,
            0,
            0x88,
            0,
        );
        let mut names = fixture::names();
        names.add_custom_function(1, "Marker", 0);
        fixture::add_formula(
            &mut files[index].1,
            &names,
            (TABLE, OCCURRENCE),
            20,
            5,
            &format!("Name & \"|{marker}|\" & Marker() & \"|\" & Get ( FileName )"),
        );
    }
    let host = Host::new("layout-edits", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let imported = schema.table_of(REMOTE).unwrap().id;
    let layout = fixture::NOTES_LAYOUT as u32;
    let page = provider.records(layout, 0, 1).unwrap();
    let value = |row: &om_data::Record, name: &str| {
        row.values[page.fields.iter().position(|f| f.name == name).unwrap()]
            .clone()
            .unwrap_or_default()
    };
    let generation = provider.calc_data(&[]).unwrap().generation;
    let edited = provider
        .update_record(
            layout,
            1,
            [("Name".into(), Some("Peer edit".into()))].into(),
        )
        .unwrap();
    assert_eq!(value(&edited, "Name"), "Peer edit");
    assert_eq!(
        value(&edited, "Edited by"),
        "Peer edit|peer|child-function|Related"
    );
    assert_eq!(
        value(&provider.records(1, 0, 1).unwrap().records[0], "Name"),
        "Write tests"
    );
    assert!(provider.calc_data(&[]).unwrap().generation > generation);
    let write = om_data::ScriptWrite {
        layout,
        table: None,
        record: Some(1),
        cells: [(COUNT, Some("41".into()))].into(),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };
    let edited = provider.write_record(&write).unwrap();
    assert_eq!(value(&edited, "Count"), "41");
    assert_eq!(value(&edited, "Doubled"), "82");
    assert_eq!(
        value(&edited, "File Info"),
        "Related|41|child-function|46||child"
    );
    let reread = provider.records(layout, 0, 1).unwrap();
    assert_eq!(value(&reread.records[0], "Count"), "41");
    // Root rules remain independent and its ordinary edit path still commits.
    let root = provider
        .update_record(1, 1, [("Name".into(), Some("Root edit".into()))].into())
        .unwrap();
    assert_eq!(
        root.values.last().unwrap().as_deref().unwrap(),
        "Root edit|root|root-function|Root"
    );
    assert_eq!(
        value(&provider.records(layout, 0, 1).unwrap().records[0], "Name"),
        "Peer edit"
    );
    let commits = host.captures.with_opcode(om_protocol::Opcode::HostCommit);
    assert_eq!(
        commits.iter().filter(|(file, _)| file == RELATED).count(),
        2
    );
    assert_eq!(commits.iter().filter(|(file, _)| file == ROOT).count(), 1);
    for (_, transaction) in host.captures.with_opcode(om_protocol::Opcode::Lock) {
        assert_eq!(
            om_protocol::write::parse_lock_request(&transaction)
                .unwrap()
                .path[0],
            uint_segment(TABLE)
        );
    }
    for (_, transaction) in commits {
        let commit = om_protocol::write::parse_commit_request(&transaction).unwrap();
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&commit.list)).unwrap();
        assert_eq!(tree.root.children[0].segment.0, uint_segment(TABLE));
    }
    let before = host.captures.with_opcode(om_protocol::Opcode::Lock).len();
    assert_eq!(
        provider
            .set_global(imported, SETTING, 1, "unsupported")
            .unwrap_err()
            .code,
        Some(3)
    );
    for cells in [
        [(SETTING, Some("unsupported".into()))].into(),
        [(NAME * 256 + 2, Some("repeat".into()))].into(),
    ] {
        assert_eq!(
            provider
                .write_record(&om_data::ScriptWrite {
                    cells,
                    ..write.clone()
                })
                .unwrap_err()
                .code,
            Some(3)
        );
    }
    assert_eq!(
        host.captures.with_opcode(om_protocol::Opcode::Lock).len(),
        before
    );
}

/// Deletion needs no field-entry plan. The complete owner engine must retain
/// records and container references on refusal, then verify peer-only removal.
#[test]
fn imported_layout_delete_uses_owner_verification_without_field_entry_rules() {
    use om_data::found::{FoundSetChange, SortField, SortOrder};
    use om_protocol::Opcode;

    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    for (_, tree) in &mut files {
        fixture::add_photo(tree);
    }
    // Editing this table cannot safely evaluate its auto-entry selector.
    fixture::add_field(&mut files[1].1, TABLE, 21, "Audit record", 1, 1, 0, 0x88, 0);
    fixture::add_formula(
        &mut files[1].1,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        21,
        5,
        "Get ( RecordID )",
    );
    let host = Host::new("layout-delete", files);
    let mut provider = host.open(ROOT);
    let capabilities = provider.record_capabilities(2).unwrap();
    assert!(capabilities.delete);
    assert!(!capabilities.update && !capabilities.create);
    let ids = |provider: &mut NativeProvider, layout| {
        provider
            .records(layout, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&mut provider, 1), [1, 2]);
    provider
        .change_found_set(
            2,
            FoundSetChange::Sort(vec![SortField::new("Count", SortOrder::Ascending)]),
        )
        .unwrap();
    provider.use_window(7);
    provider
        .change_found_set(2, FoundSetChange::Records(vec![1]))
        .unwrap();
    provider.use_window(1);
    assert_eq!(ids(&mut provider, 2), [2, 1]);

    // Held in the related file's part of the transaction: nothing
    // is committed, and the revert keeps the record.
    provider.begin_transaction(Default::default()).unwrap();
    let commits = host.captures.with_opcode(Opcode::HostCommit).len();
    provider.delete_record(2, 1).unwrap();
    provider.revert_transaction().unwrap();
    assert_eq!(host.captures.with_opcode(Opcode::HostCommit).len(), commits);
    assert_eq!(ids(&mut provider, 2), [2, 1], "the reverted deletion");
    let generation = provider.calc_data(&[]).unwrap().generation;
    let barrier = host.block_saves(1);
    let error = provider.delete_record(2, 1).unwrap_err();
    assert!(
        error.message.contains("Related file “Related”")
            && error.message.contains("commit record deletion"),
        "{error}"
    );
    assert!(provider.calc_data(&[]).unwrap().generation > generation);
    assert_eq!(ids(&mut provider, 2), [2, 1]);
    assert!(
        provider
            .change_found_set(2, FoundSetChange::Omit(vec![]))
            .unwrap()
            .sorted
    );
    assert!(host
        .captures
        .with_opcode(Opcode::AdjustReferenceCount)
        .is_empty());
    drop(barrier);

    let generation = provider.calc_data(&[]).unwrap().generation;
    provider.delete_record(2, 1).unwrap();
    assert!(provider.calc_data(&[]).unwrap().generation > generation);
    assert_eq!(ids(&mut provider, 2), [2]);
    assert!(
        provider
            .change_found_set(2, FoundSetChange::Omit(vec![]))
            .unwrap()
            .sorted
    );
    provider.use_window(7);
    assert!(ids(&mut provider, 2).is_empty());
    provider.use_window(1);
    assert_eq!(ids(&mut provider, 1), [1, 2]);
    let releases = host.captures.with_opcode(Opcode::AdjustReferenceCount);
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].0, RELATED);
    let release =
        om_protocol::container::parse_adjust_reference_count_request(&releases[0].1).unwrap();
    assert_eq!(release.delta, -1);
    assert_eq!(release.path[0], uint_segment(TABLE));
    for (file, tx) in host.captures.with_opcode(Opcode::Lock) {
        assert_eq!(file, RELATED);
        assert_eq!(
            om_protocol::write::parse_lock_request(&tx).unwrap().path[0],
            uint_segment(TABLE)
        );
    }
    assert!(host
        .captures
        .with_opcode(Opcode::HostCommit)
        .iter()
        .all(|(file, _)| file == RELATED));
    // Fresh connections verify stored identity, independently of caller caches.
    assert_eq!(ids(&mut host.open(RELATED), 1), [2]);
    assert_eq!(ids(&mut host.open(ROOT), 1), [1, 2]);
    provider.delete_record(1, 2).unwrap();
    assert_eq!(ids(&mut host.open(ROOT), 1), [1]);
    assert_eq!(ids(&mut host.open(RELATED), 1), [2]);
}

/// New Record, Duplicate Record's copied values and a script's New Record
/// run the owner's complete creation engine: its id, rules, auto-entry,
/// custom function, account and file name, although Root issues the same
/// next id. The caller's window takes each record as a local creation does.
#[test]
fn imported_layout_creation_uses_the_owner_engine_and_caller_found_set() {
    use om_data::found::{FoundSetChange, SortField, SortOrder};
    use om_data::ScriptWrite;
    use om_protocol::Opcode;

    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    // Same ids, different custom function and auto-entry definitions,
    // evaluated even while Name is empty (0x20).
    for (index, marker) in [(0, "root"), (1, "peer")] {
        fixture::add_field(&mut files[index].1, TABLE, 20, "Entered", 1, 1, 0, 0xa8, 0);
        let mut names = fixture::names();
        names.add_custom_function(1, "Marker", 0);
        fixture::add_formula(
            &mut files[index].1,
            &names,
            (TABLE, OCCURRENCE),
            20,
            5,
            &format!("Name & \"|{marker}|\" & Marker() & \"|\" & Get ( FileName )"),
        );
    }
    let host = Host::new("layout-create", files);
    let mut provider = host.open(ROOT);
    let layout = fixture::NOTES_LAYOUT as u32;
    assert!(provider.record_capabilities(layout).unwrap().create);
    let fields = provider.records(layout, 0, 1).unwrap().fields;
    let value = |row: &om_data::Record, name: &str| {
        row.values[fields.iter().position(|f| f.name == name).unwrap()]
            .clone()
            .unwrap_or_default()
    };
    let ids = |provider: &mut NativeProvider, layout| {
        provider
            .records(layout, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    // A sorted subset here; window 7 keeps another subset.
    provider
        .change_found_set(layout, FoundSetChange::Records(vec![2]))
        .unwrap();
    provider
        .change_found_set(
            layout,
            FoundSetChange::Sort(vec![SortField::new("Name", SortOrder::Ascending)]),
        )
        .unwrap();
    provider.use_window(7);
    provider
        .change_found_set(layout, FoundSetChange::Records(vec![1]))
        .unwrap();
    provider.use_window(1);
    let new = ScriptWrite {
        layout,
        table: None,
        record: None,
        cells: Default::default(),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };

    // New Record from the menu inside a script's transaction is refused
    // before any request (a script's own write is held in the transaction,
    // #1006: related_validation_values.rs); globals and repetitions before
    // any lock, creation or commit.
    provider.begin_transaction(Default::default()).unwrap();
    let before = host.captures.transactions().len();
    let error = provider
        .create_record(layout, Default::default())
        .unwrap_err();
    assert_eq!(error.code, Some(3), "{error}");
    assert_eq!(host.captures.transactions().len(), before);
    provider.revert_transaction().unwrap();
    for cells in [
        [(SETTING, Some("global".into()))].into(),
        [(NAME * 256 + 2, Some("repeat".into()))].into(),
    ] {
        let write = ScriptWrite {
            cells,
            ..new.clone()
        };
        assert_eq!(provider.write_record(&write).unwrap_err().code, Some(3));
    }
    let effects = || {
        [Opcode::Lock, Opcode::HostCreateRecord, Opcode::HostCommit]
            .map(|op| host.captures.with_opcode(op).len())
    };
    assert_eq!(effects(), [0, 0, 0]);

    // New Record (menu): the owner's id and values only.
    let generation = provider.calc_data(&[]).unwrap().generation;
    let created = provider.create_record(layout, Default::default()).unwrap();
    assert_eq!(created.id, 3);
    assert_eq!(value(&created, "Entered"), "|peer|child-function|Related");
    assert_eq!(value(&created, "Creator"), ACCOUNT);
    assert!(provider.calc_data(&[]).unwrap().generation > generation);
    // Duplicate Record (menu) passes the source's editable values.
    let duplicate = provider
        .create_record(
            layout,
            [("Name".into(), Some("Ship".into())), ("Code".into(), None)].into(),
        )
        .unwrap();
    assert_eq!(duplicate.id, 4);
    assert_eq!(value(&duplicate, "Name"), "Ship");
    assert_eq!(
        value(&duplicate, "Entered"),
        "Ship|peer|child-function|Related"
    );
    // A script's New Record and Set Field: stored calculations recomputed.
    let scripted = provider
        .write_record(&ScriptWrite {
            cells: [(NAME, Some("Scripted".into())), (COUNT, Some("21".into()))].into(),
            ..new.clone()
        })
        .unwrap();
    assert_eq!(scripted.id, 5);
    assert_eq!(value(&scripted, "Doubled"), "42");
    assert_eq!(
        value(&scripted, "Entered"),
        "Scripted|peer|child-function|Related"
    );
    // The sorted subset takes them at its end; window 7 is unchanged.
    assert_eq!(ids(&mut provider, layout), [2, 3, 4, 5]);
    assert!(
        provider
            .change_found_set(layout, FoundSetChange::Omit(vec![]))
            .unwrap()
            .sorted
    );
    provider.use_window(7);
    assert_eq!(ids(&mut provider, layout), [1]);
    provider.use_window(1);

    // An unverifiable owner save is reported as such and joins no found set:
    // the owner's commit cannot be saved after its record id was (#927).
    let barrier = host.block_commit_saves(1);
    let error = provider
        .create_record(layout, Default::default())
        .unwrap_err();
    assert!(
        error.code.is_none()
            && error.message.contains("Related file “Related”")
            && error.message.contains("could not be verified"),
        "{error}"
    );
    drop(barrier);
    assert_eq!(ids(&mut provider, layout), [2, 3, 4, 5]);

    // Root's colliding ids are untouched, and its own creation keeps its
    // own rules and connection.
    assert_eq!(ids(&mut provider, 1), [1, 2]);
    let root = provider.create_record(1, Default::default()).unwrap();
    assert_eq!(root.id, 3);
    assert_eq!(
        root.values.last().unwrap().as_deref(),
        Some("|root|root-function|Root")
    );
    for op in [Opcode::HostCreateRecord, Opcode::HostCommit] {
        let files: Vec<_> = host
            .captures
            .with_opcode(op)
            .into_iter()
            .map(|(file, _)| file)
            .collect();
        assert_eq!(files.iter().filter(|f| *f == ROOT).count(), 1, "{op:?}");
        assert_eq!(files.iter().filter(|f| *f == RELATED).count(), 4, "{op:?}");
    }
    // Fresh connections verify stored identity, independently of caches.
    let mut peer = host.open(RELATED);
    let stored = peer.records(1, 0, 10).unwrap();
    assert_eq!(
        stored.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(value(&stored.records[2], "Creator"), ACCOUNT);
    assert_eq!(value(&stored.records[4], "Doubled"), "42");
    assert_eq!(ids(&mut host.open(ROOT), 1), [1, 2, 3]);
}

/// Root's Tasks::Code is validated Always by value list 5, the values of
/// Remote Tasks::Name: a write the list checks would look the value up in
/// Related's index, which Root's connection cannot ask. It is refused
/// before anything is sent; writes the list does not check still save.
/// A new record of a related file's layout is held by that file in the
/// window's record session (#892): the window shows it, the host has
/// none. The related file's part of the session goes aside with it, so
/// another window's delete there is written at once, not held in the
/// session; the session's revert leaves that delete and stores nothing.
#[test]
fn a_held_related_file_record_and_its_part_stay_with_their_window() {
    use om_data::ScriptWrite;

    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    let host = Host::new("layout-held", files);
    let mut provider = host.open(ROOT);
    let layout = fixture::NOTES_LAYOUT as u32;
    let ids = |provider: &mut NativeProvider| {
        provider
            .records(layout, 0, 10)
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    let stored = |host: &Host| {
        rows_of(host, RELATED)
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>()
    };
    let before = stored(&host);
    provider.use_window(1);
    let shown = ids(&mut provider);
    provider.begin_record_commit().unwrap();
    let held = provider
        .write_record(&ScriptWrite {
            layout,
            table: None,
            record: None,
            cells: Default::default(),
            fields: Default::default(),
            typed: Default::default(),
            skip_validation: false,
            privilege_set: None,
            record_number: None,
            prepared: None,
        })
        .unwrap();
    assert_eq!(ids(&mut provider), [&shown[..], &[held.id]].concat());
    assert_eq!(stored(&host), before);

    provider.use_window(2);
    assert_eq!(ids(&mut provider), shown);
    provider.delete_record(layout, shown[0]).unwrap();
    assert_eq!(stored(&host), before[1..]);

    provider.use_window(1);
    assert!(provider.record_session_open());
    provider.revert_record_session().unwrap();
    assert_eq!(stored(&host), before[1..]);
    assert_eq!(ids(&mut provider), shown[1..]);
}

#[test]
fn writes_checked_against_a_related_files_value_list_are_refused_first() {
    use fixture::CODE;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    // Validated Always by a value list (schema-path.md 5.5), list 5.
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x500);
    root.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(CODE), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    root.put(
        &[&[0x21], &[5], &[5], &[0x10]],
        om_scripts::text_node(&[], "Related names"),
    );
    // A field value list (file-metadata.md 6.2) of Remote Tasks::Name.
    let mut list = vec![
        1, 0, 0, 4, 3, 0xd0, 0, 0x40, 2, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ];
    list.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
    let mut child = fixture::tree(&[], true);
    indexed(&mut child, NAME);
    let host = Host::new("list-validation", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let sent = || {
        [
            om_protocol::Opcode::QueryProgram,
            om_protocol::Opcode::Lock,
            om_protocol::Opcode::HostCreateRecord,
            om_protocol::Opcode::HostCommit,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let before = sent();
    let code = || om_data::FieldChanges::from([("Code".to_owned(), Some("Ship".to_owned()))]);
    for result in [
        provider.update_record(layout, 1, code()),
        provider.create_record(layout, code()),
    ] {
        let error = result.unwrap_err();
        assert_eq!(error.code, Some(3), "{error}");
        assert!(error.message.contains("“Code”"), "{error}");
    }
    assert_eq!(sent(), before, "nothing was locked, created or looked up");
    let cells = || [(CODE, Some("Ship".to_owned()))].into();
    let write = om_data::ScriptWrite {
        layout,
        table: None,
        record: Some(1),
        cells: cells(),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };
    assert_eq!(
        provider.check_entry(TABLE, CODE, "Ship").unwrap_err().code,
        Some(3)
    );
    assert_eq!(provider.write_record(&write).unwrap_err().code, Some(3));
    provider.begin_transaction(Default::default()).unwrap();
    for result in [
        provider.update_record(layout, 1, code()),
        provider.create_record(layout, code()),
        provider.write_record(&write),
        provider.write_record(&om_data::ScriptWrite {
            record: None,
            ..write.clone()
        }),
    ] {
        assert_eq!(result.unwrap_err().code, Some(3));
    }
    assert_eq!(
        sent(),
        before,
        "no index query, lock or creation, including script/transaction writes"
    );
    provider.revert_transaction().unwrap();
    // Name is not checked against the list.
    let name = om_data::FieldChanges::from([("Name".to_owned(), Some("Renamed".to_owned()))]);
    provider.update_record(layout, 1, name).unwrap();
}

/// Root's layout of its own Tasks shows fields of Related's Tasks, reached
/// through a relationship drawn from Related's occurrence on number keys,
/// and a field of a file the host does not have.
fn portal_files() -> Vec<(&'static str, OwnedNode)> {
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference(&mut root, (MISSING, "Unavailable Tasks"), 2, "Not hosted");
    fixture::add_relationship(&mut root, 9, (REMOTE, COUNT), 0, (OCCURRENCE, COUNT));
    fixture::add_relationship(&mut root, 10, (OCCURRENCE, COUNT), 0, (MISSING, COUNT));
    let shown = [
        (REMOTE, NAME),
        (REMOTE, COUNT),
        (REMOTE, fixture::DOUBLED),
        (MISSING, NAME),
    ];
    let objects: Vec<Vec<u8>> = shown
        .iter()
        .zip(1u64..)
        .map(|((occurrence, field), key)| {
            let bounds = om_layout::Rect {
                top: 30.0 * key as f64,
                left: 10.0,
                bottom: 30.0 * key as f64 + 20.0,
                right: 200.0,
                other: Vec::new(),
            };
            let serial = |id: u64| serial::decode(&uint_segment(id)).unwrap();
            om_layout::encode::field_object(key, &bounds, serial(*occurrence), serial(*field))
        })
        .collect();
    root.put(
        &[&[4], &[5], &uint_segment(fixture::LAYOUT), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            om_layout::encode::append_to_store(&[], &[1, 2, 3, 4], &objects),
        ),
    );
    vec![(ROOT, root), (RELATED, fixture::tree(&[], true))]
}

/// [`portal_files`], with Related's tree changed by `change`.
fn portal_files_with(change: impl FnOnce(&mut OwnedNode)) -> Vec<(&'static str, OwnedNode)> {
    let mut files = portal_files();
    change(&mut files[1].1);
    files
}

/// The Tasks records of `file` on `host`, as that file reads them: id,
/// Name, Count and Doubled.
fn rows_of(host: &Host, file: &str) -> Vec<(i64, String, String, String)> {
    let mut provider = host.open(file);
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let rows = page
        .records
        .iter()
        .map(|record| {
            let text = |name| record.values[at(name)].clone().unwrap_or_default();
            (record.id, text("Name"), text("Count"), text("Doubled"))
        })
        .collect();
    provider.close();
    rows
}

/// The commits (op70) `host` was sent for `file`.
fn commits_to(host: &Host, file: &str) -> usize {
    host.captures
        .with_opcode(om_protocol::Opcode::HostCommit)
        .iter()
        .filter(|(name, _)| name == file)
        .count()
}

/// A write of `cells` of record `record` (`None` creates one) of `table`
/// through Root's Tasks layout, as the window commits a related record.
fn related_write(
    table: Option<u64>,
    record: Option<i64>,
    cells: &[(u64, &str)],
) -> om_data::ScriptWrite {
    om_data::ScriptWrite {
        layout: fixture::LAYOUT as u32,
        table,
        record,
        cells: cells
            .iter()
            .map(|(cell, text)| (*cell, Some((*text).to_owned())))
            .collect(),
        typed: cells.iter().map(|(cell, _)| *cell).collect(),
        ..Default::default()
    }
}

/// A layout of Root's own table creates, edits and deletes Related's
/// records shown through a relationship (#1000), as the window commits them
/// (in a transaction) and as a script writes them outside one: Related
/// writes each, by its own rules, and Root's records are not written. A
/// script's transaction holding records of both files commits both (#1006).
#[test]
fn a_root_layout_writes_another_files_related_records_in_that_file() {
    let host = Host::new("related-owner-writes", portal_files());
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let remote = |record, cells: &[(u64, &str)]| related_write(Some(table), record, cells);

    // Browse offers Related's own editable fields, not its calculation, and
    // nothing of a file that is not open.
    let related = provider
        .record_capabilities(fixture::LAYOUT as u32)
        .unwrap()
        .related;
    let fields = &related[&REMOTE];
    assert!(
        fields.contains(&NAME) && fields.contains(&COUNT),
        "{fields:?}"
    );
    assert!(!fields.contains(&fixture::DOUBLED) && !fields.contains(&SETTING));
    assert!(!related.contains_key(&MISSING), "{related:?}");

    // A row begun in a portal's creation row, with its match field.
    provider.begin_transaction(Default::default()).unwrap();
    let made = provider
        .write_record(&remote(None, &[(NAME, "Made"), (COUNT, "4")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    let id = made.id;
    // Doubled is Related's own stored calculation.
    let row = |name: &str, count: &str, doubled: &str| {
        (id, name.to_owned(), count.to_owned(), doubled.to_owned())
    };
    assert!(rows_of(&host, RELATED).contains(&row("Made", "4", "8")));

    // Edited in a transaction, then by a script outside one.
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&remote(Some(id), &[(COUNT, "6")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    provider
        .write_record(&remote(Some(id), &[(NAME, "Renamed")]))
        .unwrap();
    assert!(rows_of(&host, RELATED).contains(&row("Renamed", "6", "12")));

    // A commit Related's host refuses at op70 (a system exception): the
    // error names the file, and its report says what became of the file is
    // unknown, since the commit went (#1006).
    let report = |error: &om_data::DataError| {
        error
            .commit
            .iter()
            .map(|file| {
                let name = file.file.rsplit('/').next().unwrap();
                (name.trim_end_matches(".fmp12").to_owned(), file.outcome)
            })
            .collect::<Vec<_>>()
    };
    let stem = |name: &str| name.trim_end_matches(".fmp12").to_owned();
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&remote(Some(id), &[(NAME, "Refused")]))
        .unwrap();
    let barrier = host.block_saves(1);
    let error = provider.commit_transaction(false).unwrap_err();
    drop(barrier);
    assert!(
        error.message.starts_with("Related file “Related”"),
        "{error}"
    );
    assert_eq!(
        report(&error),
        [(stem(RELATED), om_data::FileOutcome::Unknown)]
    );
    assert!(rows_of(&host, RELATED).contains(&row("Renamed", "6", "12")));

    // Reverted: nothing is written, and the lock goes with it (the next
    // write locks the record again).
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&remote(Some(id), &[(NAME, "Reverted")]))
        .unwrap();
    provider.revert_transaction().unwrap();

    // Records of both files in one script transaction (#1006): both are
    // committed, Related first, each once. Then the window's commit of a
    // record with its related records of both files, saved the same way.
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&related_write(None, Some(1), &[(NAME, "Script edit")]))
        .unwrap();
    provider
        .write_record(&remote(Some(id), &[(NAME, "Script")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    assert_eq!(
        (commits_to(&host, ROOT), commits_to(&host, RELATED)),
        (sent.0 + 1, sent.1 + 1)
    );
    assert!(rows_of(&host, RELATED).contains(&row("Script", "6", "12")));
    assert!(rows_of(&host, ROOT).iter().any(|r| r.1 == "Script edit"));
    let root_edit = related_write(None, Some(1), &[(NAME, "Root edit")]);
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&remote(Some(id), &[(NAME, "Both")]))
        .unwrap();
    provider.write_record(&root_edit).unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(
        (commits_to(&host, ROOT), commits_to(&host, RELATED)),
        (sent.0 + 2, sent.1 + 2)
    );
    assert!(rows_of(&host, RELATED).contains(&row("Both", "6", "12")));

    // Delete Portal Row, committed with the record.
    provider.begin_transaction(Default::default()).unwrap();
    provider.delete_related_record(table, id).unwrap();
    provider.commit_transaction(false).unwrap();
    assert!(rows_of(&host, RELATED).iter().all(|row| row.0 != id));

    // Root's record was written twice: by the script's transaction and by
    // the window's two-file commit.
    assert_eq!(commits_to(&host, ROOT), 2);
    let root_rows = rows_of(&host, ROOT);
    assert_eq!(
        root_rows
            .iter()
            .map(|row| row.1.as_str())
            .collect::<Vec<_>>(),
        ["Root edit", "Ship"]
    );
    provider.close();
}

/// A related file on the opened file's connection holds its records under
/// the opened file's transaction, as FileMaker Pro does on FileMaker Server
/// (write-path.md 1.5, #1006). Its lock and its commit carry that
/// transaction's id. The transaction ends only after every participant
/// settled: once in each file that held part of it, the opened file last
/// (a host may keep its locks by file, as OpenMaker Server does), so no
/// participant's part of it releases another's locks early.
#[test]
fn a_related_file_on_the_connection_commits_under_the_opened_files_transaction() {
    use om_protocol::{write, Opcode};
    let host = Host::new("related-owner-shared-txn", portal_files());
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let before = host.captures.transactions().len();
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Shared")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();

    let sent = host.captures.transactions()[before..].to_vec();
    let of = |op: Opcode| {
        sent.iter()
            .enumerate()
            .filter(|(_, (_, transaction))| transaction.first() == Some(&(op as u8)))
            .map(|(at, (file, transaction))| (at, file.clone(), transaction.clone()))
            .collect::<Vec<_>>()
    };
    let locks = of(Opcode::Lock);
    let lock = locks.iter().find(|(_, file, _)| file == RELATED).unwrap();
    let txn = write::parse_lock_request(&lock.2).unwrap().txn;
    let commits = of(Opcode::HostCommit);
    assert_eq!(commits.len(), 1, "Related's records alone");
    assert_eq!(commits[0].1, RELATED);
    assert_eq!(write::parse_commit_request(&commits[0].2).unwrap().txn, txn);
    let ends = of(Opcode::KillAllLocks);
    let files: Vec<&str> = ends.iter().map(|(_, file, _)| file.as_str()).collect();
    assert_eq!(
        files,
        [RELATED, ROOT],
        "once in each file, the opened file last"
    );
    for (at, _, end) in &ends {
        assert_eq!(write::parse_kill_all_locks_request(end).unwrap().txn, txn);
        assert!(*at > commits[0].0, "after the related file's commit");
    }

    // Revert Transaction ends it the same way, after both parts settled.
    // (No other connection opens meanwhile: captures list them in order.)
    let before = host.captures.transactions().len();
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Reverted")]))
        .unwrap();
    provider.revert_transaction().unwrap();
    let ends: Vec<String> = host.captures.transactions()[before..]
        .iter()
        .filter(|(_, transaction)| transaction.first() == Some(&(Opcode::KillAllLocks as u8)))
        .map(|(file, _)| file.clone())
        .collect();
    assert_eq!(
        ends,
        [RELATED, ROOT],
        "once in each file, the opened file last"
    );
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Shared"));
    provider.close();
}

/// A script's transaction across files commits all or none (#1006,
/// Claris's Open Transaction): Related refusing its part before any record
/// data, or Root's records too large for one request, reverts both parts,
/// nothing sent, each file's report "not submitted"; Related's commit failing, the first submitted, leaves Root
/// unsubmitted and unchanged. Revert Transaction reverts both. After each,
/// the same records commit in both files: nothing is left locked.
#[test]
fn a_script_transaction_across_files_commits_all_or_none() {
    // Related's Name: not empty, validated always.
    let host = Host::new(
        "related-owner-script-all-or-none",
        portal_files_with(|related| {
            fixture::set_field_validation(related, fixture::TABLE, NAME, 0x408)
        }),
    );
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let outcomes = |error: &om_data::DataError| {
        error
            .commit
            .iter()
            .map(|file| file.outcome)
            .collect::<Vec<_>>()
    };
    let names = |file: &str| {
        rows_of(&host, file)
            .into_iter()
            .map(|row| row.1)
            .collect::<Vec<_>>()
    };
    let before = (names(ROOT), names(RELATED));
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    let both = |provider: &mut NativeProvider, root: &str, related: &str| {
        provider.begin_transaction(Default::default()).unwrap();
        provider
            .write_record(&related_write(None, Some(1), &[(NAME, root)]))
            .unwrap();
        provider
            .write_record(&related_write(Some(table), Some(1), &[(NAME, related)]))
            .unwrap();
    };

    // Refused before any record data: Related's empty Name.
    both(&mut provider, "Root kept", "");
    let error = provider.commit_transaction(false).unwrap_err();
    assert_eq!(error.code, Some(509), "{error}");
    let not = om_data::FileOutcome::NotSubmitted;
    assert_eq!(outcomes(&error), [not, not]);
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    assert_eq!((names(ROOT), names(RELATED)), before);

    // Root's records too large for one upload (#1006 review): 18 new
    // records of 60,000 characters each, every cell within its limit, past
    // one request's. Found while preparing, so Related, submitted first,
    // sends nothing either.
    provider.begin_transaction(Default::default()).unwrap();
    let cell = "x".repeat(60_000);
    for _ in 0..18 {
        provider
            .write_record(&related_write(None, None, &[(NAME, &cell)]))
            .unwrap();
    }
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Large")]))
        .unwrap();
    let error = provider.commit_transaction(false).unwrap_err();
    assert!(error.message.contains("larger than one request"), "{error}");
    assert_eq!(outcomes(&error), [not, not], "{error}");
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    assert_eq!((names(ROOT), names(RELATED)), before);

    // Related's commit fails at the host: Root is not submitted.
    both(&mut provider, "Root kept", "Failed");
    let barrier = host.block_saves(1);
    let error = provider.commit_transaction(false).unwrap_err();
    drop(barrier);
    assert_eq!(outcomes(&error), [om_data::FileOutcome::Unknown, not]);
    assert_eq!(commits_to(&host, ROOT), sent.0);
    assert_eq!((names(ROOT), names(RELATED)), before);

    // Reverted: neither file is sent a commit.
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    both(&mut provider, "Root reverted", "Reverted");
    provider.revert_transaction().unwrap();
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    assert_eq!((names(ROOT), names(RELATED)), before);

    // Nothing was left held: the same records commit in both files.
    both(&mut provider, "Root saved", "Saved");
    provider.commit_transaction(false).unwrap();
    assert_eq!(
        (commits_to(&host, ROOT), commits_to(&host, RELATED)),
        (sent.0 + 1, sent.1 + 1)
    );
    assert!(names(ROOT).contains(&"Root saved".to_owned()));
    assert!(names(RELATED).contains(&"Saved".to_owned()));
    provider.close();
}

/// A related file's preflight refusal sends nothing, and every
/// participant's part of the transaction ends with it: the opened file's
/// next commit opens a transaction of its own, and the related record is
/// free to be locked again (#1006).
#[test]
fn a_related_files_refused_preflight_ends_every_participants_part() {
    // Related's Name: not empty, validated always.
    let host = Host::new(
        "related-owner-preflight",
        portal_files_with(|related| {
            fixture::set_field_validation(related, fixture::TABLE, NAME, 0x408)
        }),
    );
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "")]))
        .unwrap();
    let error = provider.commit_transaction(false).unwrap_err();
    assert_eq!(error.code, Some(509), "{error}");
    assert_eq!(
        error
            .commit
            .iter()
            .map(|file| file.outcome)
            .collect::<Vec<_>>(),
        [om_data::FileOutcome::NotSubmitted]
    );
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);

    // Nothing of the refused commit is still open or held.
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(None, Some(1), &[(NAME, "Root's own")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Again")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Again"));
    provider.close();
}

/// Record submission and uploaded asset ownership are separate: reconnect
/// carries acknowledged acquisitions, then explicit Save transfers them once.
#[test]
fn a_refused_container_group_keeps_each_owners_bytes_and_references_on_reconnect() {
    const ATTACHMENT: u64 = 90;
    let mut files =
        portal_files_with(|related| fixture::set_field_validation(related, TABLE, NAME, 0x408));
    for (_, file) in &mut files {
        fixture::add_field(file, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
    }
    let mut delete = om_scripts::ScriptStep::new(10);
    delete.flags |= om_scripts::flag::NO_INTERACT;
    let script_tree = fixture::tree(
        &[fixture::script(
            1,
            "Drop library",
            vec![fixture::on(86, true), delete],
        )],
        true,
    );
    files[0]
        .1
        .put(&[&[17]], script_tree.at(&[&[17]]).unwrap().clone());
    let host = Host::new("related-container-reconnect", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let member = |file: &str, reference: &str| {
        let member = om_data::containers::reference(reference).unwrap();
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut session = om_session::Session::connect(
            "127.0.0.1",
            host.port,
            client.cert.pem().as_bytes(),
            client.key_pair.serialize_pem().as_bytes(),
            "127.0.0.1",
            &om_protocol::MachineInfo::default(),
        )
        .unwrap();
        session.open_file("127.0.0.1", file).unwrap();
        let creation = session.fetch_internal_creation().unwrap();
        assert!(session
            .login(
                fixture::ACCOUNT,
                fixture::PASSWORD,
                creation,
                &om_protocol::LoginEnvironment::for_remote_file("127.0.0.1", file)
            )
            .unwrap()
            .is_success());
        let path = [uint_segment(TABLE), vec![0x1f], vec![5], member];
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let reply = session.download(&refs).unwrap();
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
            &reply.slots,
            path.iter().map(|p| om_hbam::KeySegment(p)).collect(),
        ))
        .unwrap();
        let node = tree.node(&refs).unwrap().to_owned_node();
        session.disconnect().unwrap();
        assert_eq!(
            node.at(&[&[0x12]]).unwrap().value.as_deref(),
            Some(1u32.to_be_bytes().as_slice())
        );
        node
    };
    // A direct save transfers its acquisition out of the pending draft
    // ledger. A later unrelated transaction's Revert cannot release it.
    let direct = provider
        .container_reference(
            fixture::LAYOUT as u32,
            "Attachment",
            &om_data::calc::Container::file("direct.bin", b"direct save".to_vec()),
        )
        .unwrap()
        .unwrap();
    let saved = provider
        .create_record(
            fixture::LAYOUT as u32,
            [
                ("Name".into(), Some("Direct saved".into())),
                ("Attachment".into(), Some(direct.clone())),
            ]
            .into(),
        )
        .unwrap();
    provider.begin_record_commit().unwrap();
    provider.revert_record_session().unwrap();
    member(ROOT, &direct);
    let updated = provider
        .container_reference(
            fixture::LAYOUT as u32,
            "Attachment",
            &om_data::calc::Container::file("updated.bin", b"direct update".to_vec()),
        )
        .unwrap()
        .unwrap();
    provider
        .update_record(
            fixture::LAYOUT as u32,
            saved.id,
            [("Attachment".into(), Some(updated.clone()))].into(),
        )
        .unwrap();
    provider.begin_record_commit().unwrap();
    provider.revert_record_session().unwrap();
    member(ROOT, &updated);
    provider.begin_record_commit().unwrap();
    let root_ref = provider
        .container_reference(
            fixture::LAYOUT as u32,
            "Attachment",
            &om_data::calc::Container::file("root.bin", b"immutable root bytes".to_vec()),
        )
        .unwrap()
        .unwrap();
    let root = provider
        .write_record(&related_write(
            None,
            None,
            &[(NAME, "New root"), (ATTACHMENT, &root_ref)],
        ))
        .unwrap();
    provider.mark_record_session();
    let related_ref = provider
        .related_container_reference(
            table,
            ATTACHMENT,
            &om_data::calc::Container::file("related.bin", b"immutable related bytes".to_vec()),
        )
        .unwrap()
        .unwrap();
    let related = provider
        .write_record(&related_write(
            Some(table),
            None,
            &[(NAME, "New related"), (ATTACHMENT, &related_ref)],
        ))
        .unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "")]))
        .unwrap();
    assert_eq!(
        provider.commit_record_session(false).unwrap_err().code,
        Some(509)
    );
    provider.rollback_record_session();
    let before = member(ROOT, &root_ref);
    let related_before = member(RELATED, &related_ref);
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    assert!(
        !provider.carried_record_sessions().is_empty(),
        "the acknowledged assets have a no-data carry"
    );
    // Explicit fresh login with old connections still usable exercises close's
    // releases too: those acquisitions must transfer, never be released there.
    provider.reopen(&host.connection(ROOT)).unwrap();
    assert_eq!(member(ROOT, &root_ref), before);
    assert_eq!(member(RELATED, &related_ref), related_before);
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    let mapped = provider.take_recreated_records();
    let new_related = mapped
        .iter()
        .find(|r| r.table == table && r.old == related.id as u64)
        .unwrap()
        .new
        .as_ref()
        .copied()
        .unwrap() as i64;
    provider.mark_record_session();
    provider
        .write_record(&related_write(
            Some(table),
            Some(new_related),
            &[(NAME, "New related"), (ATTACHMENT, &related_ref)],
        ))
        .unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Fixed")]))
        .unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(
        member(ROOT, &root_ref),
        before,
        "exactly one reference per stored value, and unchanged immutable member bytes"
    );
    assert!(mapped
        .iter()
        .any(|r| r.table == TABLE && r.old == root.id as u64));
    assert_eq!(member(RELATED, &related_ref), related_before);
    for (after_reconnect, drop_parked) in [
        (false, Some(false)),
        (true, Some(true)),
        (false, None),
        (true, None),
    ] {
        // A deduplicated upload acquired before the existing Root's ordinary save
        // becomes that session's responsibility. Revert must release exactly this
        // extra reference, preserving the first stored record's member.
        let duplicate = provider
            .container_reference(
                fixture::LAYOUT as u32,
                "Attachment",
                &om_data::calc::Container::file("root.bin", b"immutable root bytes".to_vec()),
            )
            .unwrap()
            .unwrap();
        assert_eq!(duplicate, root_ref);
        provider.begin_record_commit().unwrap();
        provider.mark_record_session();
        let related_copy = provider
            .related_container_reference(
                table,
                ATTACHMENT,
                &om_data::calc::Container::file("related.bin", b"immutable related bytes".to_vec()),
            )
            .unwrap()
            .unwrap();
        let current_root = mapped
            .iter()
            .find(|r| r.table == TABLE && r.old == root.id as u64)
            .unwrap()
            .new
            .as_ref()
            .copied()
            .unwrap() as i64;
        provider
            .write_record(&related_write(
                None,
                Some(current_root),
                &[(NAME, "Edited root"), (ATTACHMENT, &duplicate)],
            ))
            .unwrap();
        provider
            .write_record(&related_write(
                Some(table),
                None,
                &[(NAME, "Reverted log"), (ATTACHMENT, &related_copy)],
            ))
            .unwrap();
        provider
            .write_record(&related_write(Some(table), Some(1), &[(NAME, "")]))
            .unwrap();
        assert_eq!(
            provider.commit_record_session(false).unwrap_err().code,
            Some(509)
        );
        provider.rollback_record_session();
        assert_eq!(
            provider
                .owned_container_drafts()
                .get(&(TABLE, root_ref.clone())),
            Some(&1)
        );
        if let Some(parked) = drop_parked {
            if after_reconnect {
                provider.reopen(&host.connection(ROOT)).unwrap();
            }
            if parked {
                provider.use_window(9);
            }
            provider.drop_window(1);
            provider.use_window(1);
            // Window teardown is already Revert. Selecting its old numeric id
            // and reverting again must never consume stored references.
            provider.revert_record_session().unwrap();
            provider.revert_record_session().unwrap();
        } else {
            // A public positive adjustment must join the root's owner receipt,
            // just as related_container_reference does, before reconnect.
            provider
                .adjust_related_container(table, &related_copy, 1)
                .unwrap();
            if after_reconnect {
                provider.reopen(&host.connection(ROOT)).unwrap();
            }
            // Replacing the draft acknowledges release of its one acquisition.
            // The retained receipt must no longer own the stored record's reference.
            provider
                .adjust_container(fixture::LAYOUT as u32, &duplicate, -1)
                .unwrap();
            provider
                .adjust_related_container(table, &related_copy, -2)
                .unwrap();
            let temporary_root = provider
                .container_reference(
                    fixture::LAYOUT as u32,
                    "Attachment",
                    &om_data::calc::Container::file("temporary.bin", b"replacement root".to_vec()),
                )
                .unwrap()
                .unwrap();
            let temporary_related = provider
                .related_container_reference(
                    table,
                    ATTACHMENT,
                    &om_data::calc::Container::file(
                        "temporary.bin",
                        b"replacement related".to_vec(),
                    ),
                )
                .unwrap()
                .unwrap();
            if !after_reconnect {
                provider.reopen(&host.connection(ROOT)).unwrap();
            }
            if after_reconnect {
                // Close while another window is current. The receipt belongs to
                // the parked window's participants, whose counts must detach too.
                provider.use_window(9);
                provider.close();
                provider.use_window(1);
                provider.reopen(&host.connection(ROOT)).unwrap();
            }
            provider.revert_record_session().unwrap();
            let mut observer = host.open(ROOT);
            observer.calc_schema().unwrap();
            for (owner, reference) in [(TABLE, temporary_root), (table, temporary_related)] {
                let key = (owner, om_data::containers::reference(&reference).unwrap());
                let data = observer
                    .calc_data(&[CalcNeed::Member {
                        table: owner,
                        member: key.1.clone(),
                    }])
                    .unwrap();
                assert_eq!(
                    data.library.members.get(&key),
                    Some(&None),
                    "unused replacement settled once: after={after_reconnect} owner={owner}"
                );
            }
            observer.close();
        }
        // Reference-count modification stamps differ; immutable bytes and the
        // remaining stored record's sole reference must both survive.
        let after = member(ROOT, &root_ref);
        for node in &before.children {
            if !matches!(node.segment.as_slice(), [0xfc] | [0xfc, 1..=3]) {
                assert_eq!(after.child(&node.segment), Some(node));
            }
        }
        let after_related = member(RELATED, &related_ref);
        for node in &related_before.children {
            if !matches!(node.segment.as_slice(), [0xfc] | [0xfc, 1..=3]) {
                assert_eq!(after_related.child(&node.segment), Some(node));
            }
        }
    }
    // A library reset can reuse the raw member id and even identical bytes.
    // Its native identities must veto reconnect before any new reservation.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut reset = host.open(ROOT);
                assert_eq!(
                    reset
                        .perform_script_on_server(&om_data::runtime::ServerScriptCall {
                            file: 0,
                            script: 1,
                            parameter: None,
                            wait: true,
                        })
                        .unwrap()
                        .error,
                    om_data::runtime::ErrorCode::NONE
                );
                reset
                    .create_record(
                        fixture::LAYOUT as u32,
                        [("Name".into(), Some("Reset sentinel".into()))].into(),
                    )
                    .unwrap();
                reset.close();
            })
            .join()
            .unwrap()
    });
    provider.begin_record_commit().unwrap();
    let taken = provider
        .container_reference(
            fixture::LAYOUT as u32,
            "Attachment",
            &om_data::calc::Container::file("root.bin", b"immutable root bytes".to_vec()),
        )
        .unwrap()
        .unwrap();
    provider
        .write_record(&related_write(
            None,
            None,
            &[(NAME, "Held after reset"), (ATTACHMENT, &taken)],
        ))
        .unwrap();
    provider.mark_record_session();
    provider
        .write_record(&related_write(Some(table), None, &[(NAME, "Held log")]))
        .unwrap();
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "")]))
        .unwrap();
    assert_eq!(
        provider.commit_record_session(false).unwrap_err().code,
        Some(509)
    );
    provider.rollback_record_session();
    let replacement = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                // A genuinely independent client's writes do not contaminate this
                // worker thread's never-submitted receipt.
                let mut reset = host.open(ROOT);
                assert_eq!(
                    reset
                        .perform_script_on_server(&om_data::runtime::ServerScriptCall {
                            file: 0,
                            script: 1,
                            parameter: None,
                            wait: true,
                        })
                        .unwrap()
                        .error,
                    om_data::runtime::ErrorCode::NONE
                );
                let replacement = reset
                    .container_reference(
                        fixture::LAYOUT as u32,
                        "Attachment",
                        &om_data::calc::Container::file(
                            "root.bin",
                            b"immutable root bytes".to_vec(),
                        ),
                    )
                    .unwrap()
                    .unwrap();
                reset.close();
                replacement
            })
            .join()
            .unwrap()
    });
    assert_eq!(replacement, taken);
    let replacement_bytes = member(ROOT, &replacement);
    let wire = || {
        [
            om_protocol::Opcode::HostCreateRecord,
            om_protocol::Opcode::UploadBinaryData,
            om_protocol::Opcode::AdjustReferenceCount,
            om_protocol::Opcode::UploadListToHostTempLocation,
            om_protocol::Opcode::HostCommit,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let no_more = wire();
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert!(!provider.carried_record_sessions().is_empty());
    for _ in 0..2 {
        assert!(provider
            .revert_record_session()
            .unwrap_err()
            .message
            .starts_with("Unresolved:"));
    }
    provider.drop_window(1);
    provider.use_window(1);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    provider.close();
    assert_eq!(
        wire(),
        no_more,
        "unknown ownership neither reserves, uploads, commits nor releases"
    );
    assert_eq!(member(ROOT, &replacement), replacement_bytes);
}

/// Existing-record attachment entry retains its accepted baseline and same RID.
#[test]
fn an_existing_root_keeps_its_uploaded_attachment_without_rebasing_on_reconnect() {
    use om_protocol::Opcode;
    const ATTACHMENT: u64 = 90;
    let mut tree = security::tree(&[], security::CUSTOM_KEY);
    fixture::add_field(&mut tree, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
    let host = Host::new("existing-root-upload", vec![(ROOT, tree)]);
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    // Read fresh persisted trees independently of the provider's library cache.
    let member = |reference: &str| {
        let path = host
            .handle
            .files()
            .into_iter()
            .find(|f| f.name == ROOT)
            .unwrap()
            .path;
        let file = om_hbam::HbamFile::open(&path).unwrap();
        let tree = file.key_tree().unwrap();
        let key = om_data::containers::reference(reference).unwrap();
        tree.node(&[&uint_segment(TABLE), &[0x1f], &[5], &key])
            .map(|n| n.to_owned_node())
    };
    let reference_count = |node: &OwnedNode| {
        u32::from_be_bytes(
            node.child(&[0x12])
                .unwrap()
                .value
                .as_deref()
                .unwrap()
                .try_into()
                .unwrap(),
        )
    };
    let mut provider = host.open(ROOT);
    // The original is itself an actual ACKed upload that a direct save stored.
    let original = provider
        .container_reference(
            fixture::LAYOUT as u32,
            "Attachment",
            &om_data::calc::Container::file("original.bin", b"stored original".to_vec()),
        )
        .unwrap()
        .unwrap();
    provider
        .update_record(
            fixture::LAYOUT as u32,
            1,
            [("Attachment".into(), Some(original.clone()))].into(),
        )
        .unwrap();
    provider.close();
    let original_member = member(&original).unwrap();
    assert_eq!(reference_count(&original_member), 1);
    let mut provider = host.open(ROOT);
    let hold = |p: &mut NativeProvider, payload: &[u8]| {
        let page = p.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let version = page.versions.get(&1).copied();
        assert!(p
            .prepare_container_edit(fixture::LAYOUT as u32, 1, version, "Attachment")
            .unwrap());
        let baseline = p.submission_mark().unwrap();
        let mut data = om_data::calc::Container::file("draft.bin", payload.to_vec());
        data.path_type = Some("file");
        data.streams
            .push((om_data::calc::StreamType(*b"uNkn"), b"\0keep\xff".to_vec()));
        let reference = p
            .container_reference(fixture::LAYOUT as u32, "Attachment", &data)
            .unwrap()
            .unwrap();
        p.write_record(&related_write(None, Some(1), &[(ATTACHMENT, &reference)]))
            .unwrap();
        assert_eq!(
            baseline.until(p.submission_mark()),
            om_data::Submitted::Nothing
        );
        reference
    };
    let saved_before = rows_of(&host, ROOT);
    let reference = hold(&mut provider, b"save this attachment");
    let uploaded = member(&reference).unwrap();
    assert!(uploaded.child(b"uNkn").is_some());
    assert_eq!(member(&original), Some(original_member.clone()));
    assert_eq!(rows_of(&host, ROOT), saved_before);
    let calls = host.captures.transactions().len();
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(
        host.captures.transactions().len(),
        calls,
        "cached availability sends nothing"
    );
    host.handle.close_connections();
    let before = wire();
    let ownership = provider.owned_container_drafts();
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(provider.owned_container_drafts(), ownership);
    provider.reopen(&host.connection(ROOT)).unwrap();
    assert_eq!(
        wire(),
        before,
        "same RID: no reserve, Save, upload, count or generation"
    );
    assert!(provider.take_recreated_records().is_empty());
    assert_eq!(member(&original), Some(original_member));
    assert_eq!(member(&reference), Some(uploaded.clone()));
    assert_eq!(rows_of(&host, ROOT), saved_before);
    provider.mark_record_session();
    let mut staged = related_write(None, Some(1), &[(ATTACHMENT, &reference)]);
    staged
        .fields
        .insert("Attachment".into(), Some(reference.clone()));
    provider
        .write_record(&staged)
        .expect("stage the explicit Save after recovery");
    provider.commit_record_session(false).unwrap();
    assert!(
        member(&original).is_none(),
        "release the old stored member only after Save"
    );
    assert_eq!(member(&reference), Some(uploaded.clone()));
    let mut read = host.open(ROOT);
    let page = read.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let field = page
        .fields
        .iter()
        .position(|f| f.name == "Attachment")
        .unwrap();
    assert_eq!(
        page.records.iter().find(|r| r.id == 1).unwrap().values[field].as_deref(),
        Some(reference.as_str())
    );
    read.close();

    let discarded = hold(&mut provider, b"discard only this acquisition");
    host.handle.close_connections();
    let before = wire();
    provider.reopen(&host.connection(ROOT)).unwrap();
    assert_eq!(wire(), before);
    provider.revert_record_session().unwrap();
    provider.revert_record_session().unwrap();
    assert!(member(&discarded).is_none());
    assert_eq!(
        member(&reference),
        Some(uploaded.clone()),
        "Revert never owns the stored reference"
    );

    let retained = hold(&mut provider, b"retain across a version conflict");
    let retained_member = member(&retained);
    host.handle.close_connections();
    // Another client is on another worker thread: its completed write does not
    // contaminate this worker's never-submitted request counter.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut other = host.open(ROOT);
                other
                    .update_record(
                        fixture::LAYOUT as u32,
                        1,
                        [("Name".into(), Some("Other client's change".into()))].into(),
                    )
                    .unwrap();
                other.close();
            })
            .join()
            .unwrap()
    });
    let changed = rows_of(&host, ROOT);
    let before = wire();
    let ownership = provider.owned_container_drafts();
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(),
        before,
        "conflict is refused before replacement work/transfer"
    );
    assert_eq!(provider.owned_container_drafts(), ownership);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    provider.close();
    assert_eq!(
        wire(),
        before,
        "unknown/conflicting proof grants no cleanup authority"
    );
    assert_eq!(rows_of(&host, ROOT), changed);
    assert_eq!(member(&reference), Some(uploaded));
    assert_eq!(member(&retained), retained_member);
}

/// Before either file's first Save, each accepted existing row and actual upload
/// already belongs to one explicitly prepared never-submitted group.
#[test]
fn existing_root_and_related_uploads_survive_loss_before_any_save() {
    existing_attachment_upload_lifecycle(false, false, false);
}

#[test]
fn related_first_existing_attachment_uploads_survive_loss_before_any_save() {
    existing_attachment_upload_lifecycle(true, false, false);
}

#[test]
fn three_existing_attachment_owners_survive_loss_before_any_save() {
    existing_attachment_upload_lifecycle(true, true, false);
}

#[test]
fn stored_root_container_copy_keeps_only_its_extra_acquisition_through_recovery() {
    existing_attachment_upload_lifecycle(false, false, true);
}

/// A stored copy is the first mutation; Root acquires no op88 upload merely
/// to establish its accepted row and temporary ownership.
#[test]
fn stored_source_first_prepares_before_its_count_and_joins_related_uploads() {
    const SOURCE: u64 = 91;
    const BACKUP: u64 = 92;
    let mut files = portal_files();
    for (name, tree) in &mut files {
        tree.set_child(
            security::tree(&[], security::CUSTOM_KEY)
                .child(&[23])
                .unwrap()
                .clone(),
        );
        if *name == ROOT {
            fixture::add_field(tree, TABLE, SOURCE, "StoredAttachment", 1, 6, 0, 0, 0);
            fixture::add_field(tree, TABLE, BACKUP, "AttachmentBackup", 1, 6, 0, 0, 0);
        } else {
            fixture::add_field(tree, TABLE, SOURCE, "Attachment", 1, 6, 0, 0, 0);
        }
    }
    let host = Host::new("stored-source-first", files);
    let mut seed = host.open(ROOT);
    let mut data = om_data::calc::Container::file("stored.bin", b"original bytes".to_vec());
    data.streams.push((
        om_data::calc::StreamType(*b"uNkn"),
        b"unknown stream".to_vec(),
    ));
    let reference = seed
        .container_reference(fixture::LAYOUT as u32, "StoredAttachment", &data)
        .unwrap()
        .unwrap();
    seed.update_record(
        fixture::LAYOUT as u32,
        1,
        [("StoredAttachment".into(), Some(reference.clone()))].into(),
    )
    .unwrap();
    seed.close();
    let wire = || {
        use om_protocol::Opcode;
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
            Opcode::Lock,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let source_node = || {
        let path = host
            .handle
            .files()
            .into_iter()
            .find(|f| f.name == ROOT)
            .unwrap()
            .path;
        let disk = om_hbam::HbamFile::open(path).unwrap();
        let tree = disk.key_tree().unwrap();
        tree.node(&[
            &uint_segment(TABLE),
            &[0x1f],
            &[5],
            &om_data::containers::reference(&reference).unwrap(),
        ])
        .unwrap()
        .to_owned_node()
    };
    let source_count = || {
        u32::from_be_bytes(
            source_node()
                .child(&[0x12])
                .unwrap()
                .value
                .as_deref()
                .unwrap()
                .try_into()
                .unwrap(),
        )
    };
    let source_bytes = || {
        let mut node = source_node();
        node.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        node
    };
    let original_bytes = source_bytes();
    let read_source = |provider: &mut NativeProvider| {
        let page = provider.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let data = provider
            .calc_data(&[
                CalcNeed::Record {
                    table: TABLE,
                    record: 1,
                },
                CalcNeed::Member {
                    table: TABLE,
                    member: om_data::containers::reference(&reference).unwrap(),
                },
            ])
            .unwrap();
        let schema = provider.calc_schema().unwrap().unwrap();
        let data = RefCell::new(data);
        let cached = CachedSource::new(&data);
        let calc = FileCalc::new(&schema, &cached, Date::from_ymd(2026, 1, 1).unwrap());
        let om_data::calc::Value::Container(source) =
            calc.field_value(OCCURRENCE, 1, SOURCE, 1).unwrap()
        else {
            panic!("typed source")
        };
        (page, source)
    };
    let stage = |provider: &mut NativeProvider, phase: u8| {
        let (page, source) = read_source(provider);
        if phase == 0 {
            let before = wire();
            let baseline = om_data::ContainerEditBaseline {
                layout: fixture::LAYOUT as u32,
                record: 1,
                version: page.versions.get(&1).copied(),
            };
            let mut unproved = source.clone();
            std::sync::Arc::make_mut(unproved.library.as_mut().unwrap()).source = None;
            assert_eq!(
                provider
                    .prepare_stored_container_edit(baseline, "AttachmentBackup", &unproved)
                    .unwrap(),
                om_data::EditPreparation::Unavailable
            );
            assert_eq!(
                provider
                    .prepare_stored_container_edit(
                        om_data::ContainerEditBaseline {
                            version: None,
                            ..baseline
                        },
                        "AttachmentBackup",
                        &source
                    )
                    .unwrap(),
                om_data::EditPreparation::Unavailable
            );
            assert!(provider.carried_record_sessions().is_empty());
            assert_eq!(
                wire(),
                before,
                "missing proof creates neither a session nor a count"
            );
        }
        assert_eq!(
            provider
                .prepare_stored_container_edit(
                    om_data::ContainerEditBaseline {
                        layout: fixture::LAYOUT as u32,
                        record: 1,
                        version: page.versions.get(&1).copied()
                    },
                    "AttachmentBackup",
                    &source,
                )
                .unwrap(),
            om_data::EditPreparation::Prepared
        );
        let before = wire();
        let count = source_count();
        let copied = provider
            .container_reference(fixture::LAYOUT as u32, "AttachmentBackup", &source)
            .unwrap()
            .unwrap();
        let mut expected = before;
        expected[6] += 1;
        assert_eq!(
            wire(),
            expected,
            "first acquisition is op87 only; Root never uploads"
        );
        assert_eq!(source_count(), count + 1);
        provider
            .write_record(&related_write(None, Some(1), &[(BACKUP, &copied)]))
            .unwrap();
        let before = wire();
        assert_eq!(
            provider.carried_record_sessions(),
            vec![1],
            "first op87 ACK owns a before-work receipt without Root op88"
        );
        assert_eq!(wire(), before, "availability is cached/NOWIRE");
        // Normal on-demand admission AFTER the stored-copy acquisition.
        provider
            .calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
            .unwrap();
        let table = provider
            .calc_schema()
            .unwrap()
            .unwrap()
            .table_of(REMOTE)
            .unwrap()
            .id;
        assert_ne!(
            table, TABLE,
            "same native table/RID belongs to another file"
        );
        let accepted = provider
            .calc_data(&[CalcNeed::Record { table, record: 1 }])
            .unwrap();
        let version = accepted.records[&(table, 1)].as_ref().unwrap().mod_count;
        assert!(provider
            .prepare_related_container_edit(REMOTE, table, 1, version, SOURCE, None)
            .unwrap());
        let acquired = provider
            .related_container_reference(
                table,
                SOURCE,
                &om_data::calc::Container::file("related.bin", vec![phase, 2, 3]),
            )
            .unwrap()
            .unwrap();
        provider
            .write_record(&related_write(Some(table), Some(1), &[(SOURCE, &acquired)]))
            .unwrap();
        assert_eq!(
            provider.carried_record_sessions(),
            vec![1],
            "whole group remains affirmative"
        );
        acquired
    };
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection(ROOT)).unwrap();
    let original_rows = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    stage(&mut provider, 0);
    assert_eq!(
        (rows_of(&host, ROOT), rows_of(&host, RELATED)),
        original_rows,
        "no first Save attempted"
    );
    host.handle.close_connections();
    let before = wire();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[7] += 2;
    assert_eq!(
        wire(),
        expected,
        "all-owner same-RID locks only; no count/upload/Save/generation"
    );
    assert!(provider.take_recreated_records().is_empty());
    provider.commit_record_session(false).unwrap();
    assert_eq!(source_count(), 2, "original source plus saved backup");
    assert_eq!(source_bytes(), original_bytes);
    let saved = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    stage(&mut provider, 1);
    host.handle.close_connections();
    let before = wire();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[7] += 2;
    assert_eq!(wire(), expected);
    provider.revert_record_session().unwrap();
    let before_repeat = wire();
    provider.revert_record_session().unwrap();
    assert_eq!(wire(), before_repeat, "repeat Revert releases nothing");
    assert_eq!(
        source_count(),
        2,
        "only the extra known acquisition was released"
    );
    assert_eq!(source_bytes(), original_bytes);
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), saved);
    let retained_related = stage(&mut provider, 2);
    host.handle.close_connections();
    let mut peer = host.open(RELATED);
    peer.update_record(
        fixture::LAYOUT as u32,
        1,
        [("Name".into(), Some("peer change".into()))].into(),
    )
    .unwrap();
    peer.close();
    let before = wire();
    let owned = provider.owned_container_drafts();
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(),
        before,
        "later-owner conflict precedes every replacement lock/transfer"
    );
    assert!(provider.revert_record_session().is_err());
    assert_eq!(provider.owned_container_drafts(), owned);
    // After close the imported client-ID map is deliberately gone. Verify
    // retained data at its actual owning file instead of querying that map.
    let related_member = || {
        let path = host
            .handle
            .files()
            .into_iter()
            .find(|f| f.name == RELATED)
            .unwrap()
            .path;
        let disk = om_hbam::HbamFile::open(path).unwrap();
        let tree = disk.key_tree().unwrap();
        tree.node(&[
            &uint_segment(TABLE),
            &[0x1f],
            &[5],
            &om_data::containers::reference(&retained_related).unwrap(),
        ])
        .map(|n| n.to_owned_node())
    };
    let retained_bytes = related_member();
    assert!(retained_bytes.is_some());
    provider.close();
    assert_eq!(related_member(), retained_bytes);
    assert_eq!(wire(), before, "no prefix release");
    assert_eq!(source_count(), 3);
    assert_eq!(source_bytes(), original_bytes);

    // The very first acquisition can lose its ACK before any Related file is
    // admitted. Retain that pending receipt; no formerly eligible prefix exists.
    let relay = relay::Relay::start(host.port, Duration::ZERO);
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut lost = NativeProvider::new(om_provider::NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: [host.ca_pem(), relay.ca_pem.as_slice()].concat(),
        callback_host: "127.0.0.1".into(),
        machine: om_protocol::MachineInfo::default(),
    })
    .with_related_on_demand(true);
    let mut connection = host.connection(ROOT);
    connection.port = relay.port;
    lost.open(&connection).unwrap();
    let (page, source) = read_source(&mut lost);
    assert_eq!(
        lost.prepare_stored_container_edit(
            om_data::ContainerEditBaseline {
                layout: fixture::LAYOUT as u32,
                record: 1,
                version: page.versions.get(&1).copied(),
            },
            "AttachmentBackup",
            &source
        )
        .unwrap(),
        om_data::EditPreparation::Prepared
    );
    relay.lose_reply_after(om_protocol::Opcode::AdjustReferenceCount);
    assert!(lost
        .container_reference(fixture::LAYOUT as u32, "AttachmentBackup", &source)
        .is_err());
    assert!(lost.connection_lost().is_some());
    assert_eq!(
        source_count(),
        4,
        "host applied the increment but its ACK is unknown"
    );
    let before = wire();
    assert!(lost.carried_record_sessions().is_empty());
    assert!(lost.reopen(&connection).is_err());
    assert!(lost.revert_record_session().is_err());
    lost.close();
    assert_eq!(
        wire(),
        before,
        "unknown first count is never retried or released"
    );
    assert_eq!(source_count(), 4);
    assert_eq!(source_bytes(), original_bytes);
}

fn existing_attachment_upload_lifecycle(related_first: bool, third_owner: bool, stored_copy: bool) {
    use om_protocol::Opcode;
    let lost_copy = RefCell::new(None::<relay::Relay>);
    const ATTACHMENT: u64 = 90;
    const SOURCE: u64 = 91;
    const BACKUP: u64 = 92;
    const OTHER: &str = "Other.fmp12";
    const THIRD: u64 = 0xd0_0043;
    const PASSIVE: &str = "Passive.fmp12";
    const PASSIVE_OCCURRENCE: u64 = 0xd0_0044;
    let mut files = portal_files();
    if third_owner {
        reference(&mut files[0].1, (THIRD, "Other Tasks"), 3, "Other");
        reference(
            &mut files[0].1,
            (PASSIVE_OCCURRENCE, "Read Only Tasks"),
            4,
            "Passive",
        );
        files.push((OTHER, fixture::tree(&[], true)));
    }
    let file_names: Vec<_> = files.iter().map(|(name, _)| *name).collect();
    for (file, tree) in &mut files {
        tree.set_child(
            security::tree(&[], security::CUSTOM_KEY)
                .child(&[23])
                .unwrap()
                .clone(),
        );
        fixture::add_field(tree, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
        if stored_copy && *file == ROOT {
            fixture::add_field(tree, TABLE, SOURCE, "StoredAttachment", 1, 6, 0, 0, 0);
            fixture::add_field(tree, TABLE, BACKUP, "AttachmentBackup", 1, 6, 0, 0, 0);
        }
        if *file == RELATED {
            fixture::set_field_validation(tree, TABLE, ATTACHMENT, 0x401);
            let mut names = fixture::names();
            names.add_field(TABLE, ATTACHMENT, "Attachment");
            fixture::add_formula(
                tree,
                &names,
                (TABLE, OCCURRENCE),
                ATTACHMENT,
                0x19,
                "GetContainerAttribute ( Attachment ; \"filename\" ) <> \"refused.bin\"",
            );
        }
    }
    if third_owner {
        files.push((PASSIVE, security::tree(&[], &[3])));
    }
    let host = Host::new("existing-two-owner-upload", files);
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
            Opcode::Lock,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let member = |file: &str, reference: &str| {
        let path = host
            .handle
            .files()
            .into_iter()
            .find(|f| f.name == file)
            .unwrap()
            .path;
        let disk = om_hbam::HbamFile::open(path).unwrap();
        let tree = disk.key_tree().unwrap();
        let key = om_data::containers::reference(reference).unwrap();
        tree.node(&[&uint_segment(TABLE), &[0x1f], &[5], &key])
            .map(|n| n.to_owned_node())
    };
    let read_reference = |file: &str| {
        let mut reader = host.open(file);
        let page = reader.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let field = page
            .fields
            .iter()
            .position(|f| f.name == "Attachment")
            .unwrap();
        let value = page.records[0].values[field].clone().unwrap();
        reader.close();
        value
    };
    let members = |refs: &[String]| {
        file_names
            .iter()
            .zip(refs)
            .map(|(file, reference)| member(file, reference))
            .collect::<Vec<_>>()
    };
    let stored_rows = || {
        file_names
            .iter()
            .map(|file| rows_of(&host, file))
            .collect::<Vec<_>>()
    };
    let original: Vec<_> = file_names
        .iter()
        .copied()
        .map(|file| {
            let mut seed = host.open(file);
            let reference = seed
                .container_reference(
                    fixture::LAYOUT as u32,
                    "Attachment",
                    &om_data::calc::Container::file(
                        "stored.bin",
                        format!("original {file}").into_bytes(),
                    ),
                )
                .unwrap()
                .unwrap();
            seed.update_record(
                fixture::LAYOUT as u32,
                1,
                [("Attachment".into(), Some(reference.clone()))].into(),
            )
            .unwrap();
            if stored_copy && file == ROOT {
                let mut data =
                    om_data::calc::Container::file("source.bin", b"retained source bytes".to_vec());
                data.streams.push((
                    om_data::calc::StreamType(*b"uNkn"),
                    b"source unknown stream".to_vec(),
                ));
                let source = seed
                    .container_reference(fixture::LAYOUT as u32, "StoredAttachment", &data)
                    .unwrap()
                    .unwrap();
                seed.update_record(
                    fixture::LAYOUT as u32,
                    1,
                    [("StoredAttachment".into(), Some(source))].into(),
                )
                .unwrap();
            }
            seed.close();
            reference
        })
        .collect();
    let source_reference = stored_copy.then(|| {
        let mut fresh = host.open(ROOT);
        let page = fresh.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let at = page
            .fields
            .iter()
            .position(|f| f.name == "StoredAttachment")
            .unwrap();
        let source = page.records[0].values[at].clone().unwrap();
        fresh.close();
        source
    });
    let source_count = || {
        let node = member(ROOT, source_reference.as_deref().unwrap()).unwrap();
        u32::from_be_bytes(
            node.child(&[0x12])
                .unwrap()
                .value
                .as_deref()
                .unwrap()
                .try_into()
                .unwrap(),
        )
    };
    let source_bytes = || {
        let mut node = member(ROOT, source_reference.as_deref().unwrap()).unwrap();
        node.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        node
    };
    let original_source = stored_copy.then(source_bytes);
    let original_members = members(&original);
    let passive_original = third_owner.then(|| rows_of(&host, PASSIVE));
    let mut provider = host.provider().with_related_on_demand(third_owner);
    provider.open(&host.connection(ROOT)).unwrap();
    if third_owner {
        provider
            .calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
            .unwrap();
    }
    let table = provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    assert_ne!(
        table, TABLE,
        "colliding native table/RID values have separate owners"
    );
    let hold = |p: &mut NativeProvider, epoch: u8| {
        let other_opens = host
            .captures
            .opens_and_logins()
            .0
            .get(OTHER)
            .copied()
            .unwrap_or(0);
        if third_owner && epoch == 1 {
            assert!(p.calc_schema().unwrap().unwrap().is_pending(THIRD));
        }
        // This accepted row read precedes all attachment work. It is never
        // reconstructed from a later observation or a refused-save report.
        let data = p
            .calc_data(&[CalcNeed::Record { table, record: 1 }])
            .unwrap();
        let related_version = data.records[&(table, 1)].as_ref().unwrap().mod_count;
        let page = p.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let baseline = p.submission_mark().unwrap();
        let root_upload = |p: &mut NativeProvider| {
            assert!(p
                .prepare_container_edit(
                    fixture::LAYOUT as u32,
                    1,
                    page.versions.get(&1).copied(),
                    "Attachment",
                )
                .unwrap());
            let mut data = om_data::calc::Container::file("root.bin", vec![epoch, 1, 2]);
            data.path_type = Some("file");
            data.streams.push((
                om_data::calc::StreamType(*b"uNkn"),
                b"\0root unknown\xff".to_vec(),
            ));
            let root_ref = p
                .container_reference(fixture::LAYOUT as u32, "Attachment", &data)
                .unwrap()
                .unwrap();
            p.write_record(&related_write(None, Some(1), &[(ATTACHMENT, &root_ref)]))
                .unwrap();
            root_ref
        };
        let related_upload = |p: &mut NativeProvider| {
            assert!(p
                .prepare_related_container_edit(
                    REMOTE,
                    table,
                    1,
                    related_version,
                    ATTACHMENT,
                    related_first.then_some(om_data::ContainerEditBaseline {
                        layout: fixture::LAYOUT as u32,
                        record: 1,
                        version: page.versions.get(&1).copied(),
                    })
                )
                .unwrap());
            let mut data = om_data::calc::Container::file(
                if epoch == 1 {
                    "refused.bin"
                } else {
                    "related.bin"
                },
                vec![epoch, 3, 4],
            );
            data.path_type = Some("file");
            data.streams.push((
                om_data::calc::StreamType(*b"uNkn"),
                b"\0related unknown\xff".to_vec(),
            ));
            let related_ref = p
                .related_container_reference(table, ATTACHMENT, &data)
                .unwrap()
                .unwrap();
            p.write_record(&related_write(
                Some(table),
                Some(1),
                &[(ATTACHMENT, &related_ref)],
            ))
            .unwrap();
            related_ref
        };
        let (root_ref, related_ref) = if related_first {
            let related_ref = related_upload(p);
            let calls = host.captures.transactions().len();
            assert!(
                p.carried_record_sessions().is_empty(),
                "awaiting Root is incomplete"
            );
            assert_eq!(
                host.captures.transactions().len(),
                calls,
                "availability is NOWIRE"
            );
            assert!(
                p.owned_container_drafts()
                    .keys()
                    .all(|(owner, _)| *owner != TABLE),
                "Root originals are observed, never owned acquisitions"
            );
            let root_ref = root_upload(p);
            (root_ref, related_ref)
        } else {
            let root_ref = root_upload(p);
            let related_ref = related_upload(p);
            (root_ref, related_ref)
        };
        assert_eq!(
            baseline.until(p.submission_mark()),
            om_data::Submitted::Nothing
        );
        let mut references = vec![root_ref, related_ref];
        if third_owner {
            // A passive own Read-Only file opens between the asset owners.
            // It must retain its IDs without joining their write/asset proof.
            p.calc_data(&[CalcNeed::Occurrence {
                occurrence: PASSIVE_OCCURRENCE,
            }])
            .unwrap();
            let passive = p
                .calc_schema()
                .unwrap()
                .unwrap()
                .table_of(PASSIVE_OCCURRENCE)
                .unwrap()
                .id;
            let read = p
                .calc_data(&[CalcNeed::Record {
                    table: passive,
                    record: 1,
                }])
                .unwrap();
            assert!(read.records[&(passive, 1)].is_some());
            assert!(p
                .owned_container_drafts()
                .keys()
                .all(|(table, _)| *table != passive));
            // B is reached only now, after A -> Root -> passive read. Use
            // its actual accepted row version, never a later pinned value.
            if epoch == 1 {
                assert_eq!(
                    host.captures
                        .opens_and_logins()
                        .0
                        .get(OTHER)
                        .copied()
                        .unwrap_or(0),
                    other_opens,
                    "B has not been opened by the editing client"
                );
            }
            p.calc_data(&[CalcNeed::Occurrence { occurrence: THIRD }])
                .unwrap();
            let other = p
                .calc_schema()
                .unwrap()
                .unwrap()
                .table_of(THIRD)
                .unwrap()
                .id;
            let accepted = p
                .calc_data(&[CalcNeed::Record {
                    table: other,
                    record: 1,
                }])
                .unwrap();
            let version = accepted.records[&(other, 1)].as_ref().unwrap().mod_count;
            assert!(p
                .prepare_related_container_edit(
                    THIRD,
                    other,
                    1,
                    version,
                    ATTACHMENT,
                    Some(om_data::ContainerEditBaseline {
                        layout: fixture::LAYOUT as u32,
                        record: 1,
                        version: page.versions.get(&1).copied(),
                    })
                )
                .unwrap());
            let mut data = om_data::calc::Container::file("other.bin", vec![epoch, 5, 6]);
            data.path_type = Some("file");
            data.streams.push((
                om_data::calc::StreamType(*b"uNkn"),
                b"other unknown bytes".to_vec(),
            ));
            let reference = p
                .related_container_reference(other, ATTACHMENT, &data)
                .unwrap()
                .unwrap();
            p.write_record(&related_write(
                Some(other),
                Some(1),
                &[(ATTACHMENT, &reference)],
            ))
            .unwrap();
            references.push(reference);
        }
        if stored_copy {
            // Read the actual typed stored source through the normal calculation
            // host, including its accepted row and member metadata.
            let source_at = page
                .fields
                .iter()
                .position(|f| f.name == "StoredAttachment")
                .unwrap();
            let source_text = page.records[0].values[source_at].as_deref().unwrap();
            let source_member = om_data::containers::reference(source_text).unwrap();
            let data = p
                .calc_data(&[
                    CalcNeed::Record {
                        table: TABLE,
                        record: 1,
                    },
                    CalcNeed::Member {
                        table: TABLE,
                        member: source_member,
                    },
                ])
                .unwrap();
            let schema = p.calc_schema().unwrap().unwrap();
            let data = RefCell::new(data);
            let cached = CachedSource::new(&data);
            let calc = FileCalc::new(&schema, &cached, Date::from_ymd(2026, 1, 1).unwrap());
            let om_data::calc::Value::Container(source) =
                calc.field_value(OCCURRENCE, 1, SOURCE, 1).unwrap()
            else {
                panic!("stored typed source")
            };
            let before = wire();
            // Metadata-only values have no source-cell authority. Missing
            // provenance must refuse before any count or ownership change.
            if epoch == 1 {
                let mut unproved = source.clone();
                std::sync::Arc::make_mut(unproved.library.as_mut().unwrap()).source = None;
                assert_eq!(
                    p.container_reference(fixture::LAYOUT as u32, "AttachmentBackup", &unproved)
                        .unwrap_err()
                        .code,
                    Some(3)
                );
                assert_eq!(wire(), before);
            }
            let before_count = source_count();
            if let Some(relay) = lost_copy.borrow().as_ref() {
                relay.lose_reply_after(Opcode::AdjustReferenceCount);
            }
            let result = p.container_reference(fixture::LAYOUT as u32, "AttachmentBackup", &source);
            if lost_copy.borrow().is_some() {
                let error = result.unwrap_err();
                assert!(p.connection_lost().is_some(), "the increment ACK was lost: {error:?}; wire {:?}; source count {} before {before_count}", wire(), source_count());
                assert!(p.carried_record_sessions().is_empty());
                return references;
            }
            let copied = result.unwrap().unwrap();
            assert_eq!(source_count(), before_count + 1);
            assert_eq!(source_bytes(), original_source.as_ref().unwrap().clone());
            let mut expected = before;
            expected[6] += 1;
            assert_eq!(
                wire(),
                expected,
                "copy takes exactly one op87, no upload or Save"
            );
            assert_eq!(copied, source_text);
            p.write_record(&related_write(None, Some(1), &[(BACKUP, &copied)]))
                .unwrap();
        }
        // No mark/Commit/validation refusal has been invoked to create proof.
        references
    };
    let stored = stored_rows();
    let mut references = hold(&mut provider, 1);
    let mut uploads = members(&references);
    assert!(uploads
        .iter()
        .all(|n| n.as_ref().unwrap().child(b"uNkn").is_some()));
    assert_eq!(members(&original), original_members);
    let calls = host.captures.transactions().len();
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(
        host.captures.transactions().len(),
        calls,
        "availability is cached/NOWIRE"
    );
    host.handle.close_connections();
    let before = wire();
    let owned = provider.owned_container_drafts();
    assert!(provider.revert_record_session().is_err());
    assert_eq!(provider.owned_container_drafts(), owned);
    assert_eq!(
        provider.carried_record_sessions(),
        vec![1],
        "detached failed Revert remains retryable"
    );
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[7] += file_names.len();
    assert_eq!(
        wire(),
        expected,
        "only SAME-RID locks; no reserve/Save/reupload/count/generation"
    );
    assert!(provider.take_recreated_records().is_empty());
    assert_eq!(stored_rows(), stored);
    assert_eq!(members(&references), uploads);
    // The GUI marks an ordinary Save, then rolls it back after validation
    // refuses before any record submission. That temporary mark must not
    // invalidate the before-work group receipt or its cleanup authority.
    let before_refusal = wire();
    provider.mark_record_session();
    let refused = provider.commit_record_session(false).unwrap_err();
    assert_eq!(refused.code, Some(507), "{refused}");
    provider.rollback_record_session();
    assert_eq!(
        wire(),
        before_refusal,
        "validation refuses before wire mutation"
    );
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(stored_rows(), stored);
    assert_eq!(member(RELATED, &references[1]), uploads[1]);
    let corrected = provider
        .related_container_reference(
            table,
            ATTACHMENT,
            &om_data::calc::Container::file("related.bin", vec![1, 3, 5]),
        )
        .unwrap()
        .unwrap();
    provider
        .write_record(&related_write(
            Some(table),
            Some(1),
            &[(ATTACHMENT, &corrected)],
        ))
        .unwrap();
    provider
        .adjust_related_container(table, &references[1], -1)
        .unwrap();
    assert_eq!(member(RELATED, &references[1]), None);
    references[1] = corrected;
    uploads[1] = member(RELATED, &references[1]);
    host.handle.close_connections();
    let before = wire();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[7] += file_names.len();
    assert_eq!(
        wire(),
        expected,
        "corrected refusal still reconnects without mutation"
    );
    provider.commit_record_session(false).unwrap();
    assert_eq!(
        file_names
            .iter()
            .map(|file| read_reference(file))
            .collect::<Vec<_>>(),
        references
    );
    assert_eq!(members(&original), vec![None; file_names.len()]);
    if stored_copy {
        assert_eq!(
            source_count(),
            2,
            "source + explicitly saved backup; no original ownership release"
        );
        assert_eq!(source_bytes(), original_source.as_ref().unwrap().clone());
        let mut fresh = host.open(ROOT);
        let page = fresh.record_by_id(fixture::LAYOUT as u32, 1).unwrap();
        let at = page
            .fields
            .iter()
            .position(|f| f.name == "AttachmentBackup")
            .unwrap();
        assert_eq!(page.records[0].values[at], source_reference);
        fresh.close();
    }

    let discarded = hold(&mut provider, 2);
    host.handle.close_connections();
    let before = wire();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[7] += file_names.len();
    assert_eq!(wire(), expected);
    provider.revert_record_session().unwrap();
    let after_revert = wire();
    provider.revert_record_session().unwrap();
    assert_eq!(wire(), after_revert, "repeat Revert cannot decrement again");
    assert_eq!(members(&discarded), vec![None; file_names.len()]);
    if stored_copy {
        assert_eq!(
            source_count(),
            2,
            "Revert releases only the extra acknowledged acquisition once"
        );
        assert_eq!(source_bytes(), original_source.as_ref().unwrap().clone());
    }
    assert_eq!(members(&references), uploads);

    let retained = hold(&mut provider, 3);
    let retained_members = members(&retained);
    host.handle.close_connections();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut other = host.open(file_names.last().unwrap());
                other
                    .update_record(
                        fixture::LAYOUT as u32,
                        1,
                        [("Name".into(), Some("Other accepted writer".into()))].into(),
                    )
                    .unwrap();
                other.close();
            })
            .join()
            .unwrap()
    });
    let before = wire();
    let owned = provider.owned_container_drafts();
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(),
        before,
        "all-owner version proof precedes even Root's replacement lock"
    );
    assert!(provider.revert_record_session().is_err());
    assert_eq!(provider.owned_container_drafts(), owned);
    provider.close();
    assert_eq!(
        wire(),
        before,
        "no prefix cleanup after the later owner conflicts"
    );
    assert_eq!(members(&retained), retained_members);
    assert_eq!(members(&references), uploads);
    if let Some(original) = passive_original {
        assert_eq!(
            rows_of(&host, PASSIVE),
            original,
            "passive rows are unchanged"
        );
    }
    if stored_copy {
        let relay = relay::Relay::start(host.port, Duration::ZERO);
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut p = NativeProvider::new(om_provider::NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: [host.ca_pem(), relay.ca_pem.as_slice()].concat(),
            callback_host: "127.0.0.1".into(),
            machine: om_protocol::MachineInfo::default(),
        });
        let mut connection = host.connection(ROOT);
        connection.port = relay.port;
        p.open(&connection).unwrap();
        let before_count = source_count();
        *lost_copy.borrow_mut() = Some(relay);
        let acquired = hold(&mut p, 4);
        let retained = members(&acquired);
        assert_eq!(
            source_count(),
            before_count + 1,
            "host applied the unacknowledged increment"
        );
        let before = wire();
        assert!(p.reopen(&connection).is_err());
        assert!(p.revert_record_session().is_err());
        p.close();
        assert_eq!(
            wire(),
            before,
            "no retry, reacquisition or prefix release after lost op87 ACK"
        );
        assert_eq!(members(&acquired), retained);
        assert_eq!(source_count(), before_count + 1);
        assert_eq!(source_bytes(), original_source.as_ref().unwrap().clone());
    }
}

/// Multiple accepted existing and new rows share their physical owner's
/// transaction and acquisition ledger, across same-table and cross-table IDs.
#[test]
fn mixed_creation_uploads_survive_loss_before_the_first_save() {
    mixed_creation_lifecycle(false);
}

#[test]
fn existing_root_scalar_first_uploads_and_mixed_rows_survive_loss_before_save() {
    mixed_creation_lifecycle(true);
}

fn mixed_creation_lifecycle(existing_root: bool) {
    use om_protocol::Opcode;
    const ATTACHMENT: u64 = 90;
    const SERIAL: u64 = 80;
    const UUID: u64 = 81;
    const CLOCK: u64 = 82;
    const LOG: u64 = 0xd0_0043;
    const PASSIVE: u64 = 0xd0_0044;
    let mut root = fixture::tree(&[], true);
    let mut child = security::tree(&[], security::CUSTOM_KEY);
    root.set_child(child.child(&[23]).unwrap().clone());
    for (tree, tables) in [
        (&mut root, vec![(TABLE, OCCURRENCE)]),
        (
            &mut child,
            vec![
                (TABLE, OCCURRENCE),
                (fixture::NOTES, fixture::NOTES_OCCURRENCE),
            ],
        ),
    ] {
        for (table, occurrence) in tables {
            fixture::add_field(tree, table, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
            fixture::add_field(tree, table, SERIAL, "Serial", 1, 2, 0, 2, 0);
            fixture::set_field_serial_increment(tree, table, SERIAL, 1);
            let mut names = fixture::names();
            names.add_field(table, SERIAL, "Serial");
            for (id, name, kind, formula) in [
                (UUID, "UUID", 1, "Get ( UUID )"),
                (CLOCK, "Clock", 5, "Get ( CurrentTimestamp )"),
            ] {
                names.add_field(table, id, name);
                fixture::add_field(tree, table, id, name, 1, kind, 0, 8, 0);
                fixture::add_formula(tree, &names, (table, occurrence), id, 5, formula);
            }
        }
    }
    let owner = Host::new("multirow-owner", vec![(RELATED, child)]);
    let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", owner.port);
    reference_with(
        &mut root,
        (REMOTE, "Remote Tasks"),
        1,
        "Related",
        &[("fmnet", &elsewhere, "Related")],
    );
    reference_with(
        &mut root,
        (LOG, "Log Notes"),
        1,
        "Related",
        &[("fmnet", &elsewhere, "Related")],
    );
    let mut definition = related::key(1);
    definition.extend(related::key(fixture::NOTES));
    root.put(
        &[&[3], &[17], &[5], &uint_segment(LOG), &[2]],
        OwnedNode::leaf([], definition),
    );
    reference(&mut root, (PASSIVE, "Passive Tasks"), 2, "Passive");
    let host = Host::new(
        "multirow-root",
        vec![(ROOT, root), ("Passive.fmp12", security::tree(&[], &[3]))],
    );
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
            Opcode::Lock,
        ]
        .map(|op| host.captures.with_opcode(op).len() + owner.captures.with_opcode(op).len())
    };
    let disk = |h: &Host, file: &str| {
        let path = h
            .handle
            .files()
            .into_iter()
            .find(|f| f.name == file)
            .unwrap()
            .path;
        let f = om_hbam::HbamFile::open(path).unwrap();
        f.key_tree().unwrap().root.to_owned_node()
    };
    let member = |h: &Host, file: &str, table: u64, reference: &str| {
        let tree = disk(h, file);
        let key = om_data::containers::reference(reference).unwrap();
        tree.at(&[&uint_segment(table), &[0x1f], &[5], &key])
            .cloned()
    };
    let count = |n: &Option<OwnedNode>| {
        n.as_ref()
            .map(|n| {
                u32::from_be_bytes(
                    n.child(&[0x12])
                        .unwrap()
                        .value
                        .as_deref()
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
            })
            .unwrap_or(0)
    };
    let rows = || {
        [disk(&host, ROOT), disk(&owner, RELATED)].map(|tree| {
            [TABLE, fixture::NOTES].map(|table| tree.at(&[&uint_segment(table), &[5]]).cloned())
        })
    };
    let passive = disk(&host, "Passive.fmp12");
    let original = {
        let mut p = owner.open(RELATED);
        let r = p
            .container_reference(
                fixture::LAYOUT as u32,
                "Attachment",
                &om_data::calc::Container::file("original.bin", b"stored original".to_vec()),
            )
            .unwrap()
            .unwrap();
        p.update_record(
            fixture::LAYOUT as u32,
            1,
            [("Attachment".into(), Some(r.clone()))].into(),
        )
        .unwrap();
        p.close();
        r
    };
    let original_member = member(&owner, RELATED, TABLE, &original);
    let stored = |h: &Host, file: &str, table: u64, record: i64, field: u64| {
        let tree = disk(h, file);
        let cell = tree
            .at(&[
                &uint_segment(table),
                &[5],
                &serial::encode(record as u64),
                &uint_segment(field),
            ])
            .unwrap();
        om_hbam::record::Cell::from_owned(cell)
            .text()
            .unwrap()
            .unwrap()
    };
    let mut p = host.provider_trusting(&owner).with_related_on_demand(true);
    p.open(&host.connection(ROOT)).unwrap();
    if existing_root {
        let layout = fixture::LAYOUT as u32;
        let original = rows();
        let page = p.record_by_id(layout, 1).unwrap();
        let version = page.versions[&1];
        let request = |version| om_data::RecordEditRequest {
            baseline: om_data::ContainerEditBaseline {
                layout,
                record: 1,
                version,
            },
            cells: [NAME].into_iter().collect(),
            fields: Default::default(),
        };
        assert_eq!(
            p.prepare_record_edit(&request(None)).unwrap(),
            om_data::EditPreparation::Unavailable
        );
        // Missing recovery proof must preserve ordinary held entry. It cannot
        // subsequently turn into a before-work baseline via Insert or a reread.
        p.begin_record_commit().unwrap();
        p.write_record(&related_write(
            None,
            Some(1),
            &[(NAME, "ordinary unavailable")],
        ))
        .unwrap();
        assert_eq!(
            p.prepare_record_edit(&request(Some(version))).unwrap(),
            om_data::EditPreparation::Unavailable
        );
        assert!(!p
            .prepare_container_edit(layout, 1, Some(version), "Attachment")
            .unwrap());
        assert_eq!(rows(), original);
        p.revert_record_session().unwrap();
        p.release_record(layout, 1);
        assert_eq!(
            p.prepare_record_edit(&request(Some(version + 1)))
                .unwrap_err()
                .code,
            Some(306)
        );
        assert_eq!(rows(), original, "a stale baseline accepts no mutation");
    }
    let issued = |p: &mut NativeProvider, table: u64, record: i64| {
        let data = p
            .calc_data(&[CalcNeed::Record {
                table,
                record: record as u64,
            }])
            .unwrap();
        let row = data.records[&(table, record as u64)].as_ref().unwrap();
        [SERIAL, UUID, CLOCK].map(|f| format!("{:?}", row.get(f, 1).unwrap()))
    };
    let immutable = |node: &Option<OwnedNode>| {
        let mut node = node.clone().unwrap();
        assert_eq!(
            node.child(&[0xd8]).unwrap().value.as_ref().unwrap().len(),
            16
        );
        node.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        node
    };
    let mut saved = None;
    let mut saved_assets = None;
    for phase in 0..3 {
        let before_rows = rows();
        let mut accepted_root_version = None;
        let mut root_name_index = None;
        let mut root = if existing_root {
            let layout = fixture::LAYOUT as u32;
            p.release_record(layout, 1);
            let accepted = p.record_by_id(layout, 1).unwrap();
            root_name_index = accepted.fields.iter().position(|f| f.name == "Name");
            let version = accepted.versions.get(&1).copied();
            accepted_root_version = version;
            if phase == 1 {
                // Insert-first uses the same accepted row but takes its actual
                // upload before the later scalar footprint extension below.
                accepted.records.into_iter().find(|r| r.id == 1).unwrap()
            } else {
                assert_eq!(
                    p.prepare_record_edit(&om_data::RecordEditRequest {
                        baseline: om_data::ContainerEditBaseline {
                            layout,
                            record: 1,
                            version
                        },
                        cells: [NAME].into_iter().collect(),
                        fields: Default::default(),
                    })
                    .unwrap(),
                    om_data::EditPreparation::Prepared
                );
                // This scalar mutation precedes Insert; its original proof must
                // survive the later library/acquisition footprint extension.
                p.write_record(&related_write(None, Some(1), &[(NAME, "Root")]))
                    .unwrap()
            }
        } else {
            p.begin_record_commit().unwrap();
            p.write_record(&related_write(None, None, &[(NAME, "Root")]))
                .unwrap()
        };
        let original_root_id = root.id;
        let root_issued = (!existing_root).then(|| issued(&mut p, TABLE, root.id));
        if existing_root {
            assert!(p
                .prepare_container_edit(
                    fixture::LAYOUT as u32,
                    root.id,
                    accepted_root_version,
                    "Attachment"
                )
                .unwrap());
        }
        let upload = om_data::calc::Container::file("root.bin", b"root upload".to_vec());
        let root_ref = p
            .container_reference(fixture::LAYOUT as u32, "Attachment", &upload)
            .unwrap()
            .unwrap();
        p.write_record(&related_write(
            None,
            Some(root.id),
            &[(ATTACHMENT, &root_ref)],
        ))
        .unwrap();
        if existing_root && phase == 1 {
            assert_eq!(
                p.prepare_record_edit(&om_data::RecordEditRequest {
                    baseline: om_data::ContainerEditBaseline {
                        layout: fixture::LAYOUT as u32,
                        record: root.id,
                        version: accepted_root_version,
                    },
                    cells: [NAME].into_iter().collect(),
                    fields: Default::default(),
                })
                .unwrap(),
                om_data::EditPreparation::Prepared
            );
            p.write_record(&related_write(
                None,
                Some(root.id),
                &[(NAME, "Insert first")],
            ))
            .unwrap();
        }
        p.calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
            .unwrap();
        let related = p
            .calc_schema()
            .unwrap()
            .unwrap()
            .table_of(REMOTE)
            .unwrap()
            .id;
        for id in [1, 2] {
            let data = p
                .calc_data(&[CalcNeed::Record {
                    table: related,
                    record: id,
                }])
                .unwrap();
            let version = data.records[&(related, id)]
                .as_ref()
                .unwrap()
                .mod_count
                .unwrap();
            p.release_related_record(related, id as i64);
            assert!(p.retain_related_record(related, id as i64, version));
            p.write_record(&related_write(
                Some(related),
                Some(id as i64),
                &[(NAME, if id == 1 { "first" } else { "second" })],
            ))
            .unwrap();
        }
        assert!(
            p.owned_container_drafts()
                .keys()
                .all(|(table, _)| *table != related),
            "scalar-only rows acquire no library authority"
        );
        let data = p
            .calc_data(&[CalcNeed::Record {
                table: related,
                record: 2,
            }])
            .unwrap();
        let version = data.records[&(related, 2)].as_ref().unwrap().mod_count;
        assert!(p
            .prepare_related_container_edit(REMOTE, related, 2, version, ATTACHMENT, None)
            .unwrap());
        let existing_ref = p
            .related_container_reference(
                related,
                ATTACHMENT,
                &om_data::calc::Container::file("existing.bin", b"existing upload".to_vec()),
            )
            .unwrap()
            .unwrap();
        p.write_record(&related_write(
            Some(related),
            Some(2),
            &[(ATTACHMENT, &existing_ref)],
        ))
        .unwrap();
        // A passive own Read-Only file precedes the late same-owner table demand.
        p.calc_data(&[CalcNeed::Occurrence {
            occurrence: PASSIVE,
        }])
        .unwrap();
        if phase == 0 {
            assert!(p.calc_schema().unwrap().unwrap().is_pending(LOG));
        }
        p.calc_data(&[CalcNeed::Occurrence { occurrence: LOG }])
            .unwrap();
        let logs = p.calc_schema().unwrap().unwrap().table_of(LOG).unwrap().id;
        assert_ne!(logs, related);
        let mut creations = Vec::new();
        let mut payload = om_data::calc::Container::file("log.bin", b"deduplicated logs".to_vec());
        payload.path_type = Some("file");
        payload.streams.push((
            om_data::calc::StreamType(*b"uNkn"),
            b"unknown\0\xff".to_vec(),
        ));
        for name in ["logA", "logB"] {
            let mut write = related_write(Some(logs), None, &[(NAME, name)]);
            let preview = p.preview_new_record(&write).unwrap().unwrap();
            for field in [SERIAL, UUID, CLOCK] {
                assert!(preview
                    .values
                    .get(&field)
                    .and_then(|v| v.as_ref())
                    .is_some_and(|v| !v.is_empty()));
            }
            write.prepared = Some(preview.id);
            let r = p.write_record(&write).unwrap();
            let values = issued(&mut p, logs, r.id);
            assert!(p
                .prepare_related_container_edit(LOG, logs, r.id, None, ATTACHMENT, None)
                .unwrap());
            let reference = p
                .related_container_reference(logs, ATTACHMENT, &payload)
                .unwrap()
                .unwrap();
            p.write_record(&related_write(
                Some(logs),
                Some(r.id),
                &[(ATTACHMENT, &reference)],
            ))
            .unwrap();
            creations.push((r.id, values, reference, preview.values));
        }
        if phase == 0 {
            assert!(
                creations
                    .iter()
                    .any(|(id, _, _, _)| *id == if existing_root { 2 } else { root.id }),
                "raw native RIDs collide across owner tables"
            );
        }
        assert_eq!(
            creations[0].2, creations[1].2,
            "two actual ACKs deduplicate in one library"
        );
        let log_ref = creations[0].2.clone();
        let upload_nodes = [
            member(&host, ROOT, TABLE, &root_ref),
            member(&owner, RELATED, TABLE, &existing_ref),
            member(&owner, RELATED, fixture::NOTES, &log_ref),
        ];
        assert!(upload_nodes[2].as_ref().unwrap().child(b"uNkn").is_some());
        for (h, file, table) in [
            (&host, ROOT, TABLE),
            (&owner, RELATED, TABLE),
            (&owner, RELATED, fixture::NOTES),
        ] {
            assert_eq!(
                disk(h, file)
                    .at(&[&uint_segment(table), &[0x1f], &[1], &[0xd8]])
                    .unwrap()
                    .value
                    .as_ref()
                    .unwrap()
                    .len(),
                16
            );
        }
        let counts = upload_nodes.each_ref().map(count);
        assert_eq!(counts[2], if phase == 0 { 2 } else { 4 });
        assert_eq!(rows(), before_rows, "no early record writes");
        let before = wire();
        assert_eq!(p.carried_record_sessions(), vec![1]);
        assert_eq!(wire(), before, "availability is NOWIRE");
        host.handle.close_connections();
        owner.handle.close_connections();
        if phase == 2 {
            let mut writer = owner.open(RELATED);
            writer
                .update_record(
                    fixture::LAYOUT as u32,
                    2,
                    [("Name".into(), Some("changed later row".into()))].into(),
                )
                .unwrap();
            writer.close();
            let before = wire();
            assert!(p
                .reopen(&host.connection(ROOT))
                .unwrap_err()
                .message
                .starts_with("Unresolved:"));
            assert_eq!(
                wire(),
                before,
                "later-row original proof BEFORE first replacement lock/reservation"
            );
            let owned = p.owned_container_drafts();
            assert!(p.revert_record_session().is_err());
            assert_eq!(p.owned_container_drafts(), owned);
            p.close();
            assert_eq!(wire(), before, "no prefix release on conflicting group");
            assert_eq!(
                member(&owner, RELATED, fixture::NOTES, &log_ref),
                upload_nodes[2]
            );
            break;
        }
        let before = wire();
        if phase == 1 {
            let owned = p.owned_container_drafts();
            assert!(
                p.revert_record_session().is_err(),
                "offline validation cannot release"
            );
            assert_eq!(p.owned_container_drafts(), owned);
            assert_eq!(
                p.carried_record_sessions(),
                vec![1],
                "failed Revert retains the whole receipt"
            );
        }
        p.reopen(&host.connection(ROOT)).unwrap();
        let mut expected = before;
        expected[0] += if existing_root { 2 } else { 3 };
        expected[7] += if existing_root { 3 } else { 2 };
        assert_eq!(
            wire(),
            expected,
            "only 3 reservations and 2 SAME-RID locks; no issued values, upload, count or Save"
        );
        assert_eq!(rows(), before_rows);
        assert_eq!(
            [
                member(&host, ROOT, TABLE, &root_ref),
                member(&owner, RELATED, TABLE, &existing_ref),
                member(&owner, RELATED, fixture::NOTES, &log_ref)
            ],
            upload_nodes
        );
        let mapped = p.take_recreated_records();
        assert_eq!(mapped.len(), if existing_root { 2 } else { 3 });
        let remap = |table, old: i64| {
            *mapped
                .iter()
                .find(|m| m.table == table && m.old == old as u64)
                .unwrap()
                .new
                .as_ref()
                .unwrap() as i64
        };
        if let Some(root_issued) = root_issued {
            root.id = remap(TABLE, root.id);
            assert_eq!(issued(&mut p, TABLE, root.id), root_issued);
        } else {
            assert_eq!(root.id, original_root_id, "existing Root stays SAME-RID");
            assert!(mapped.iter().all(|r| r.table != TABLE));
        }
        for (id, values, _, _) in &mut creations {
            *id = remap(logs, *id);
            assert_eq!(issued(&mut p, logs, *id), *values);
        }
        if existing_root && phase == 1 {
            // Existing calc_data intentionally reads stored values. An empty
            // ordinary held write returns the retained cells without replacing
            // Name, so this assertion cannot hide a lost post-Insert edit.
            let held_before = wire();
            let shown = p
                .write_record(&related_write(None, Some(root.id), &[]))
                .unwrap();
            assert_eq!(
                shown.values[root_name_index.unwrap()].as_deref(),
                Some("Insert first")
            );
            let held_after = wire();
            for index in [0, 1, 2, 3, 5, 6, 7] {
                assert_eq!(held_after[index], held_before[index], "the explicit no-change write may query the host clock but submits/acquires/releases nothing");
            }
        }
        p.mark_record_session();
        // Normal caller restages every mapped row before the explicit Save.
        p.write_record(&related_write(None, Some(root.id), &[(NAME, "Root")]))
            .unwrap();
        for (id, _, _, _) in &creations {
            p.write_record(&related_write(Some(logs), Some(*id), &[(NAME, "log")]))
                .unwrap();
        }
        if phase == 0 {
            let commits = wire()[2];
            p.commit_record_session(false).unwrap();
            assert_eq!(
                wire()[2],
                commits + 2,
                "one commit per physical owner, not per row/table"
            );
            let mut reader = owner.open(RELATED);
            let records = reader
                .records(fixture::NOTES_LAYOUT as u32, 0, 100)
                .unwrap();
            assert_eq!(records.records.len(), 3);
            for (id, values, reference, _) in &creations {
                let row = reader
                    .calc_data(&[CalcNeed::Record {
                        table: fixture::NOTES,
                        record: *id as u64,
                    }])
                    .unwrap();
                let row = row.records[&(fixture::NOTES, *id as u64)].as_ref().unwrap();
                assert_eq!(
                    [SERIAL, UUID, CLOCK].map(|f| format!("{:?}", row.get(f, 1).unwrap())),
                    *values
                );
                let native = member(&owner, RELATED, fixture::NOTES, reference);
                assert_eq!(count(&native), 2);
            }
            reader.close();
            assert_eq!(stored(&host, ROOT, TABLE, root.id, ATTACHMENT), root_ref);
            assert_eq!(stored(&owner, RELATED, TABLE, 1, ATTACHMENT), original);
            assert_eq!(stored(&owner, RELATED, TABLE, 2, ATTACHMENT), existing_ref);
            for (id, _, reference, preview) in &creations {
                assert_eq!(
                    stored(&owner, RELATED, fixture::NOTES, *id, ATTACHMENT),
                    *reference
                );
                for field in [SERIAL, UUID, CLOCK] {
                    assert_eq!(
                        Some(stored(&owner, RELATED, fixture::NOTES, *id, field)),
                        preview[&field],
                        "the accepted owning preview survives creation, reconnect and Save"
                    );
                }
            }
            // A known Save consumes the held references without rewriting any
            // immutable member subtree (including native IDs and unknown data).
            assert_eq!(
                [
                    member(&host, ROOT, TABLE, &root_ref),
                    member(&owner, RELATED, TABLE, &existing_ref),
                    member(&owner, RELATED, fixture::NOTES, &log_ref)
                ],
                upload_nodes
            );
            saved = Some(rows());
            saved_assets = Some(upload_nodes.each_ref().map(immutable));
        } else {
            p.revert_record_session().unwrap();
            let after = wire();
            p.revert_record_session().unwrap();
            assert_eq!(wire(), after, "no second release");
            assert_eq!(rows(), saved.clone().unwrap());
            let retained = [
                member(&host, ROOT, TABLE, &root_ref),
                member(&owner, RELATED, TABLE, &existing_ref),
                member(&owner, RELATED, fixture::NOTES, &log_ref),
            ];
            assert_eq!(retained.each_ref().map(count), [1, 1, 2]);
            assert_eq!(
                retained.each_ref().map(immutable),
                saved_assets.clone().unwrap()
            );
        }
        assert_eq!(disk(&host, "Passive.fmp12"), passive);
        assert_eq!(member(&owner, RELATED, TABLE, &original), original_member);
    }
    assert_eq!(disk(&host, "Passive.fmp12"), passive);
    assert_eq!(member(&owner, RELATED, TABLE, &original), original_member);
}

/// A normal New Record can own a real upload before its first Save. The
/// before-work baseline, own rules and library proof travel together.
#[test]
fn an_unsaved_new_root_keeps_its_uploaded_attachment_across_explicit_reconnect() {
    use om_protocol::Opcode;
    const ATTACHMENT: u64 = 90;
    const SERIAL: u64 = 80;
    const UUID: u64 = 81;
    const CLOCK: u64 = 82;
    let mut tree = security::tree(&[], security::CUSTOM_KEY);
    fixture::add_field(&mut tree, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
    fixture::add_field(&mut tree, TABLE, SERIAL, "Serial", 1, 2, 0, 0x2, 0);
    fixture::set_field_serial_increment(&mut tree, TABLE, SERIAL, 1);
    tree.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(SERIAL),
            &[0x0c],
        ],
        om_scripts::text_node(&[], "7"),
    );
    let mut names = fixture::names();
    names.add_field(TABLE, SERIAL, "Serial");
    for (id, name, kind, formula) in [
        (UUID, "UUID", 1, "Get ( UUID )"),
        (CLOCK, "Clock", 5, "Get ( CurrentTimestamp )"),
    ] {
        names.add_field(TABLE, id, name);
        fixture::add_field(&mut tree, TABLE, id, name, 1, kind, 0, 0x8, 0);
        fixture::add_formula(&mut tree, &names, (TABLE, OCCURRENCE), id, 5, formula);
    }
    let ca = related::Ca::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut host = Host::serving_at("single-upload", vec![(ROOT, tree)], port, &ca).unwrap();
    let wire = |h: &Host| {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| h.captures.with_opcode(op).len())
    };
    let asset = |h: &Host, reference: &str| {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut session = om_session::Session::connect(
            "127.0.0.1",
            h.port,
            client.cert.pem().as_bytes(),
            client.key_pair.serialize_pem().as_bytes(),
            "127.0.0.1",
            &om_protocol::MachineInfo::default(),
        )
        .unwrap();
        session.open_file("127.0.0.1", ROOT).unwrap();
        let creation = session.fetch_internal_creation().unwrap();
        assert!(session
            .login(
                ACCOUNT,
                fixture::PASSWORD,
                creation,
                &om_protocol::LoginEnvironment::for_remote_file("127.0.0.1", ROOT)
            )
            .unwrap()
            .is_success());
        let library = session
            .download(&[&uint_segment(TABLE), &[0x1f], &[1], &[0xd8]])
            .unwrap();
        assert_eq!(library.lock_error, 0);
        assert_eq!(library.value.len(), 16);
        let path = [
            uint_segment(TABLE),
            vec![0x1f],
            vec![5],
            om_data::containers::reference(reference).unwrap(),
        ];
        let refs: Vec<_> = path.iter().map(Vec::as_slice).collect();
        let reply = session.download(&refs).unwrap();
        assert_eq!(reply.lock_error, 0);
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
            &reply.slots,
            path.iter().map(|p| om_hbam::KeySegment(p)).collect(),
        ))
        .unwrap();
        let member = tree.node(&refs).unwrap().to_owned_node();
        assert_eq!(
            member.child(&[0xd8]).unwrap().value.as_ref().unwrap().len(),
            16
        );
        session.disconnect().unwrap();
        (library.value, member)
    };
    let count = |node: &OwnedNode| {
        u32::from_be_bytes(
            node.child(&[0x12])
                .unwrap()
                .value
                .as_deref()
                .unwrap()
                .try_into()
                .unwrap(),
        )
    };
    let immutable = |node: &OwnedNode| {
        let mut node = node.clone();
        node.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        node
    };
    let mut provider = host.open(ROOT);
    let fields = provider
        .records(fixture::LAYOUT as u32, 0, 1)
        .unwrap()
        .fields;
    let issued = |record: &om_data::Record| {
        ["Serial", "UUID", "Clock"].map(|name| {
            record.values[fields.iter().position(|f| f.name == name).unwrap()]
                .clone()
                .unwrap()
        })
    };
    let hold = |p: &mut NativeProvider| {
        p.begin_record_commit().unwrap();
        let baseline = p.submission_mark().unwrap();
        let root = p
            .write_record(&related_write(None, None, &[(NAME, "New attachment")]))
            .unwrap();
        let mut upload =
            om_data::calc::Container::file("attachment.bin", b"retained uploaded bytes".to_vec());
        upload.path_type = Some("file");
        upload.streams.push((
            om_data::calc::StreamType(*b"uNkn"),
            b"\0unknown stream\xff".to_vec(),
        ));
        let reference = p
            .container_reference(fixture::LAYOUT as u32, "Attachment", &upload)
            .unwrap()
            .unwrap();
        let root = p
            .write_record(&related_write(
                None,
                Some(root.id),
                &[(ATTACHMENT, &reference)],
            ))
            .unwrap();
        assert_eq!(
            baseline.until(p.submission_mark()),
            om_data::Submitted::Nothing
        );
        (root, reference)
    };
    let stored = rows_of(&host, ROOT);
    let (root, reference) = hold(&mut provider);
    let original_issued = issued(&root);
    assert_eq!(original_issued[0], "7");
    assert!(!original_issued[1].is_empty() && !original_issued[2].is_empty());
    let uploaded = asset(&host, &reference);
    assert_eq!(count(&uploaded.1), 1);
    assert!(uploaded.1.child(b"uNkn").is_some());
    assert_eq!(
        rows_of(&host, ROOT),
        stored,
        "upload is separate from record submission"
    );
    let before = wire(&host);
    assert_eq!((before[1], before[2], before[5]), (0, 0, 1));
    let calls = host.captures.transactions().len();
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    assert_eq!(
        host.captures.transactions().len(),
        calls,
        "availability makes no wire requests"
    );
    host.handle.close_connections();
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    // Failed Revert detached the sole cleanup authority before attempting
    // validation. Retry must not mistake the empty live counts for lost proof.
    let ownership_before_revert = provider.owned_container_drafts();
    for _ in 0..2 {
        assert!(provider
            .revert_record_session()
            .unwrap_err()
            .message
            .starts_with("Unresolved:"));
        assert_eq!(provider.carried_record_sessions(), vec![1]);
        assert_eq!(provider.owned_container_drafts(), ownership_before_revert);
        assert_eq!(wire(&host), before, "failed validation sends no decrement");
    }
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = before;
    expected[0] += 1;
    assert_eq!(
        wire(&host),
        expected,
        "only one new RID, no upload/count/value reissue"
    );
    assert_eq!(asset(&host, &reference), uploaded);
    assert_eq!(rows_of(&host, ROOT), stored);
    let mapped = provider.take_recreated_records();
    assert_eq!(mapped.len(), 1);
    assert_eq!((mapped[0].table, mapped[0].old), (TABLE, root.id as u64));
    let mut new_id = *mapped[0].new.as_ref().unwrap() as i64;
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(page.records.iter().find(|r| r.id == new_id).unwrap()),
        original_issued
    );
    let corrected = provider
        .write_record(&related_write(
            None,
            Some(new_id),
            &[(NAME, "Explicit save")],
        ))
        .unwrap();
    assert_eq!(issued(&corrected), original_issued);
    // The latest accepted values and original upload ledger survive another
    // explicit reconnect before Save; no new creation values are generated.
    let before_again = wire(&host);
    host.handle.close_connections();
    assert_eq!(provider.carried_record_sessions(), vec![1]);
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected_again = before_again;
    expected_again[0] += 1;
    assert_eq!(wire(&host), expected_again);
    let mapped = provider.take_recreated_records();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].old, new_id as u64);
    new_id = *mapped[0].new.as_ref().unwrap() as i64;
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let latest = page.records.iter().find(|r| r.id == new_id).unwrap();
    assert_eq!(issued(latest), original_issued);
    assert_eq!(
        latest.values[page.fields.iter().position(|f| f.name == "Name").unwrap()].as_deref(),
        Some("Explicit save")
    );
    provider.commit_record_session(false).unwrap();
    assert_eq!(asset(&host, &reference), uploaded);
    let mut independent = host.open(ROOT);
    let page = independent.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let saved = page.records.iter().find(|r| r.id == new_id).unwrap();
    assert_eq!(issued(saved), original_issued);
    assert_eq!(
        saved.values[page
            .fields
            .iter()
            .position(|f| f.name == "Attachment")
            .unwrap()]
        .as_deref(),
        Some(reference.as_str())
    );
    assert_eq!(
        saved.values[page.fields.iter().position(|f| f.name == "Name").unwrap()].as_deref(),
        Some("Explicit save")
    );
    independent.close();

    // Actual op88 dedup acquires ONE extra reference. Save POSTUPLOAD trees,
    // narrowing only this same authenticated principal's own Edit permission.
    let stored = rows_of(&host, ROOT);
    let (held, duplicate) = hold(&mut provider);
    assert_eq!(duplicate, reference);
    let held_issued = issued(&held);
    let frozen = provider.carried_record_sessions();
    let ownership = provider.owned_container_drafts();
    let twice = asset(&host, &reference);
    assert_eq!(count(&twice.1), 2);
    let path = host
        .handle
        .files()
        .into_iter()
        .find(|f| f.name == ROOT)
        .unwrap()
        .path;
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    let file = om_hbam::HbamFile::open(&path).unwrap();
    let saved = file.key_tree().unwrap().root.to_owned_node();
    drop(file);
    let restart = |tag, tree: OwnedNode| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(next) = Host::serving_at(tag, vec![(ROOT, tree.clone())], port, &ca) {
                break next;
            }
            assert!(Instant::now() < deadline, "synthetic listener did not stop");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let mut narrowed = saved.clone();
    narrowed.put(
        &[&[23], &[2], &[5], security::CUSTOM_KEY, &[2]],
        OwnedNode::leaf([], vec![0x10, 3]),
    );
    host = restart("single-upload-narrowed", narrowed);
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(&host),
        [0; 7],
        "own proof precedes any replacement or asset mutation"
    );
    assert_eq!(provider.carried_record_sessions(), frozen);
    assert_eq!(provider.owned_container_drafts(), ownership);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    provider.drop_window(1);
    provider.use_window(1);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(wire(&host), [0; 7], "no cleanup fallthrough");
    assert_eq!(asset(&host, &reference), twice);
    assert_eq!(rows_of(&host, ROOT), stored);
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("single-upload-restored", saved);
    provider.reopen(&host.connection(ROOT)).unwrap();
    assert_eq!(wire(&host), [1, 0, 0, 0, 0, 0, 0]);
    let mapped = provider.take_recreated_records();
    let id = *mapped
        .iter()
        .find(|m| m.table == TABLE && m.old == held.id as u64)
        .unwrap()
        .new
        .as_ref()
        .unwrap() as i64;
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(page.records.iter().find(|r| r.id == id).unwrap()),
        held_issued
    );
    provider.revert_record_session().unwrap();
    let settled = wire(&host);
    assert_eq!(settled, [1, 0, 0, 0, 0, 0, 1]);
    provider.revert_record_session().unwrap();
    assert_eq!(wire(&host), settled, "only one cleanup authority");
    let (library, member) = asset(&host, &reference);
    assert_eq!(library, uploaded.0);
    assert_eq!(
        count(&member),
        1,
        "the saved record's only reference survives"
    );
    assert_eq!(immutable(&member), immutable(&uploaded.1));
    assert_eq!(rows_of(&host, ROOT), stored);
    provider.close();
}

/// Uploaded acquisitions need the same own static rules on every participant,
/// including cleanup after an explicit reconnect refused a narrowed owner.
#[test]
fn static_custom_uploaded_container_recovery_keeps_each_owners_acquisitions() {
    use om_protocol::Opcode;
    const ATTACHMENT: u64 = 90;
    const SERIAL: u64 = 80;
    const UUID: u64 = 81;
    const CLOCK: u64 = 82;
    let mut files = portal_files_with(|tree| {
        fixture::set_field_validation(tree, TABLE, NAME, 0x408);
    });
    let mut names = fixture::names();
    for (id, name) in [(SERIAL, "Serial"), (UUID, "UUID"), (CLOCK, "Clock")] {
        names.add_field(TABLE, id, name);
    }
    for (_, tree) in &mut files {
        tree.set_child(
            security::tree(&[], security::CUSTOM_KEY)
                .child(&[23])
                .unwrap()
                .clone(),
        );
        fixture::add_field(tree, TABLE, ATTACHMENT, "Attachment", 1, 6, 0, 0, 0);
        fixture::add_field(tree, TABLE, SERIAL, "Serial", 1, 2, 0, 0x2, 0);
        fixture::set_field_serial_increment(tree, TABLE, SERIAL, 1);
        tree.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(SERIAL),
                &[0x0c],
            ],
            om_scripts::text_node(&[], "7"),
        );
        for (id, name, kind, formula) in [
            (UUID, "UUID", 1, "Get ( UUID )"),
            (CLOCK, "Clock", 5, "Get ( CurrentTimestamp )"),
        ] {
            fixture::add_field(tree, TABLE, id, name, 1, kind, 0, 0x8, 0);
            fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), id, 5, formula);
        }
    }
    let ca = related::Ca::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut host = Host::serving_at("custom-container", files, port, &ca).unwrap();
    let wire = |h: &Host| {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| h.captures.with_opcode(op).len())
    };
    // Independent authenticated reads include the native library identity and
    // complete stored streams, not just a reference string or MD5.
    let asset = |h: &Host, file: &str, reference: &str| {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut session = om_session::Session::connect(
            "127.0.0.1",
            h.port,
            client.cert.pem().as_bytes(),
            client.key_pair.serialize_pem().as_bytes(),
            "127.0.0.1",
            &om_protocol::MachineInfo::default(),
        )
        .unwrap();
        session.open_file("127.0.0.1", file).unwrap();
        let creation = session.fetch_internal_creation().unwrap();
        assert!(session
            .login(
                ACCOUNT,
                fixture::PASSWORD,
                creation,
                &om_protocol::LoginEnvironment::for_remote_file("127.0.0.1", file)
            )
            .unwrap()
            .is_success());
        let library = session
            .download(&[&uint_segment(TABLE), &[0x1f], &[1], &[0xd8]])
            .unwrap();
        assert_eq!(library.lock_error, 0);
        assert_eq!(library.value.len(), 16);
        let path = [
            uint_segment(TABLE),
            vec![0x1f],
            vec![5],
            om_data::containers::reference(reference).unwrap(),
        ];
        let refs: Vec<_> = path.iter().map(Vec::as_slice).collect();
        let reply = session.download(&refs).unwrap();
        assert_eq!(reply.lock_error, 0);
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
            &reply.slots,
            path.iter().map(|p| om_hbam::KeySegment(p)).collect(),
        ))
        .unwrap();
        let member = tree.node(&refs).unwrap().to_owned_node();
        assert_eq!(
            member.child(&[0xd8]).unwrap().value.as_ref().unwrap().len(),
            16
        );
        session.disconnect().unwrap();
        (library.value, member)
    };
    let count = |node: &OwnedNode| {
        u32::from_be_bytes(
            node.child(&[0x12])
                .unwrap()
                .value
                .as_deref()
                .unwrap()
                .try_into()
                .unwrap(),
        )
    };
    let immutable = |node: &OwnedNode| {
        let mut node = node.clone();
        node.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        node
    };
    let mut provider = host.open(ROOT);
    let table = provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    let fields = provider
        .records(fixture::LAYOUT as u32, 0, 1)
        .unwrap()
        .fields;
    let issued = |record: &om_data::Record| {
        ["Serial", "UUID", "Clock"].map(|name| {
            record.values[fields.iter().position(|f| f.name == name).unwrap()]
                .clone()
                .unwrap()
        })
    };
    let hold = |p: &mut NativeProvider| {
        p.begin_record_commit().unwrap();
        // Leave the one-file upload recovery shape through ordinary live
        // related work, then require the existing all-participant receipt.
        let root = p
            .write_record(&related_write(None, None, &[(NAME, "New root")]))
            .unwrap();
        let mut root_upload =
            om_data::calc::Container::file("root.bin", b"immutable Root upload".to_vec());
        root_upload.path_type = Some("file");
        root_upload.streams.push((
            om_data::calc::StreamType(*b"uNkn"),
            b"\0retained unknown stream\xff".to_vec(),
        ));
        let root_ref = p
            .container_reference(fixture::LAYOUT as u32, "Attachment", &root_upload)
            .unwrap()
            .unwrap();
        let root = p
            .write_record(&related_write(
                None,
                Some(root.id),
                &[(ATTACHMENT, &root_ref)],
            ))
            .unwrap();
        p.mark_record_session();
        let related_ref = p
            .related_container_reference(
                table,
                ATTACHMENT,
                &om_data::calc::Container::file(
                    "related.bin",
                    b"immutable Related upload".to_vec(),
                ),
            )
            .unwrap()
            .unwrap();
        let related = p
            .write_record(&related_write(
                Some(table),
                None,
                &[(NAME, "New related"), (ATTACHMENT, &related_ref)],
            ))
            .unwrap();
        p.write_record(&related_write(Some(table), Some(1), &[(NAME, "")]))
            .unwrap();
        let err = p.commit_record_session(false).unwrap_err();
        assert_eq!(err.code, Some(509), "{err}");
        p.rollback_record_session();
        (root, related, root_ref, related_ref)
    };
    let (root, other, root_ref, other_ref) = hold(&mut provider);
    assert_eq!(root.id, other.id, "raw IDs collide across owning files");
    let root_issued = issued(&root);
    let other_issued = issued(&other);
    assert!(!root_issued[1].is_empty() && !root_issued[2].is_empty());
    let before = [
        asset(&host, ROOT, &root_ref),
        asset(&host, RELATED, &other_ref),
    ];
    assert!(before.iter().all(|(_, node)| count(node) == 1));
    assert!(
        before[0].1.child(b"uNkn").is_some(),
        "the proof includes an unknown uploaded stream"
    );
    let stored = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    let sent = wire(&host);
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mut expected = sent;
    expected[0] += 2;
    assert_eq!(
        wire(&host),
        expected,
        "only replacement RID reservations, no asset mutation or reissue"
    );
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), stored);
    assert_eq!(
        [
            asset(&host, ROOT, &root_ref),
            asset(&host, RELATED, &other_ref)
        ],
        before
    );
    let mapped = provider.take_recreated_records();
    let remap = |table, record| {
        *mapped
            .iter()
            .find(|r| r.table == table && r.old == record)
            .unwrap()
            .new
            .as_ref()
            .unwrap() as i64
    };
    let root_id = remap(TABLE, root.id as u64);
    let other_id = remap(table, other.id as u64);
    let root_page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(root_page.records.iter().find(|r| r.id == root_id).unwrap()),
        root_issued
    );
    provider.mark_record_session();
    let corrected = provider
        .write_record(&related_write(
            Some(table),
            Some(other_id),
            &[(NAME, "New related"), (ATTACHMENT, &other_ref)],
        ))
        .unwrap();
    assert_eq!(issued(&corrected), other_issued);
    provider
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Corrected")]))
        .unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(
        [
            asset(&host, ROOT, &root_ref),
            asset(&host, RELATED, &other_ref)
        ],
        before
    );
    for (file, id, expected, reference) in [
        (ROOT, root_id, root_issued, &root_ref),
        (RELATED, other_id, other_issued, &other_ref),
    ] {
        let mut independent = host.open(file);
        let page = independent.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let record = page.records.iter().find(|r| r.id == id).unwrap();
        assert_eq!(issued(record), expected);
        let attachment = page
            .fields
            .iter()
            .position(|f| f.name == "Attachment")
            .unwrap();
        assert_eq!(
            record.values[attachment].as_deref(),
            Some(reference.as_str())
        );
        independent.close();
    }
    // These uploads deduplicate against the stored records but acquire one
    // additional reference each. Neither narrowed rights nor teardown may
    // release Root first, and a later Revert may release ONLY the extra counts.
    let (held_root, _, root_copy, related_copy) = hold(&mut provider);
    assert_eq!(root_copy, root_ref);
    assert_eq!(related_copy, other_ref);
    let held_issued = issued(&held_root);
    let frozen = provider.carried_record_sessions();
    let ownership = provider.owned_container_drafts();
    let uploaded = [
        asset(&host, ROOT, &root_ref),
        asset(&host, RELATED, &other_ref),
    ];
    assert!(uploaded.iter().all(|(_, node)| count(node) == 2));
    let stored = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    let paths = host.handle.files();
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    // POST-upload trees preserve member bytes, count 2, native identities and
    // saved counter state; only Related's own rule leaf changes below.
    let saved: Vec<_> = [ROOT, RELATED]
        .into_iter()
        .map(|name| {
            let path = &paths.iter().find(|f| f.name == name).unwrap().path;
            let file = om_hbam::HbamFile::open(path).unwrap();
            (name, file.key_tree().unwrap().root.to_owned_node())
        })
        .collect();
    let restart = |tag, trees: Vec<(&str, OwnedNode)>| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(next) = Host::serving_at(tag, trees.clone(), port, &ca) {
                break next;
            }
            assert!(Instant::now() < deadline, "synthetic listener did not stop");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let mut narrowed = saved.clone();
    narrowed[1].1.put(
        &[&[23], &[2], &[5], security::CUSTOM_KEY, &[2]],
        OwnedNode::leaf([], vec![0x10, 3]),
    );
    host = restart("custom-container-narrowed", narrowed);
    assert!(provider
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(&host),
        [0; 7],
        "ALL own proofs precede any reservation, count or upload"
    );
    assert_eq!(provider.carried_record_sessions(), frozen);
    assert_eq!(provider.owned_container_drafts(), ownership);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert!(provider
        .adjust_container(fixture::LAYOUT as u32, &root_ref, -1)
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert!(provider
        .adjust_related_container(table, &other_ref, 1)
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    provider.drop_window(1);
    provider.use_window(1);
    assert!(provider
        .revert_record_session()
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(
        wire(&host),
        [0; 7],
        "failed cleanup never decrements Root first or falls through"
    );
    assert_eq!(
        [
            asset(&host, ROOT, &root_ref),
            asset(&host, RELATED, &other_ref)
        ],
        uploaded
    );
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), stored);
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("custom-container-restored", saved);
    provider.reopen(&host.connection(ROOT)).unwrap();
    assert_eq!(wire(&host), [2, 0, 0, 0, 0, 0, 0]);
    let mapped = provider.take_recreated_records();
    let root_id = *mapped
        .iter()
        .find(|m| m.table == TABLE && m.old == held_root.id as u64)
        .unwrap()
        .new
        .as_ref()
        .unwrap() as i64;
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(page.records.iter().find(|r| r.id == root_id).unwrap()),
        held_issued
    );
    provider.revert_record_session().unwrap();
    let settled = wire(&host);
    assert_eq!(settled, [2, 0, 0, 0, 0, 0, 2]);
    provider.revert_record_session().unwrap();
    assert_eq!(
        wire(&host),
        settled,
        "one detached receipt releases each acquisition once"
    );
    for (file, reference, original) in [
        (ROOT, &root_ref, &before[0]),
        (RELATED, &other_ref, &before[1]),
    ] {
        let (library, member) = asset(&host, file, reference);
        assert_eq!(library, original.0);
        assert_eq!(
            count(&member),
            1,
            "the stored record's only reference survives"
        );
        assert_eq!(immutable(&member), immutable(&original.1));
    }
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), stored);
    provider.close();
}

/// Own static custom rules authorize recovery; a same-principal narrowed
/// owner must freeze the entire group before even Root reserves another ID.
#[test]
fn static_custom_scalar_recovery_checks_every_owners_original_permissions() {
    use om_protocol::Opcode;
    const SERIAL: u64 = 80;
    const UUID: u64 = 81;
    const CLOCK: u64 = 82;
    let mut files = portal_files_with(|tree| {
        fixture::set_field_validation(tree, TABLE, NAME, 0x408);
    });
    let mut names = fixture::names();
    names.add_custom_function(1, "Marker", 0);
    for (id, name) in [(SERIAL, "Serial"), (UUID, "UUID"), (CLOCK, "Clock")] {
        names.add_field(TABLE, id, name);
    }
    for (_, tree) in &mut files {
        tree.set_child(
            security::tree(&[], security::CUSTOM_KEY)
                .child(&[23])
                .unwrap()
                .clone(),
        );
        // A container-bearing schema without a container draft is still scalar.
        fixture::add_field(tree, TABLE, 90, "Attachment", 1, 6, 0, 0, 0);
        fixture::add_field(tree, TABLE, SERIAL, "Serial", 1, 2, 0, 0x2, 0);
        fixture::set_field_serial_increment(tree, TABLE, SERIAL, 1);
        tree.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(SERIAL),
                &[0x0c],
            ],
            om_scripts::text_node(&[], "7"),
        );
        for (id, name, kind, formula) in [
            (UUID, "UUID", 1, "Get ( UUID )"),
            (CLOCK, "Clock", 5, "Get ( CurrentTimestamp )"),
        ] {
            fixture::add_field(tree, TABLE, id, name, 1, kind, 0, 0x8, 0);
            fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), id, 5, formula);
        }
    }
    // A replacing auto-entry exposes a changed custom-function definition:
    // replaying the old creation with a new schema would later store "B".
    function(&mut files[0].1, "A");
    fixture::add_field(&mut files[0].1, TABLE, 83, "MarkerValue", 1, 1, 0, 0x88, 0);
    names.add_field(TABLE, 83, "MarkerValue");
    fixture::add_formula(
        &mut files[0].1,
        &names,
        (TABLE, OCCURRENCE),
        83,
        5,
        "Marker()",
    );
    let ca = related::Ca::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut host = Host::serving_at("custom-recovery", files.clone(), port, &ca).unwrap();
    let writes = |host: &Host| {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::HostTimeStamp,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let mut provider = host.open(ROOT);
    let table = provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    let fields = provider
        .records(fixture::LAYOUT as u32, 0, 1)
        .unwrap()
        .fields;
    let issued = |row: &om_data::Record| {
        ["Serial", "UUID", "Clock"].map(|name| {
            row.values[fields.iter().position(|f| f.name == name).unwrap()]
                .clone()
                .unwrap()
        })
    };
    let remote = |id, name: &str| related_write(Some(table), id, &[(NAME, name)]);
    let hold = |p: &mut NativeProvider| {
        p.begin_record_commit().unwrap();
        let root = p
            .write_record(&related_write(None, None, &[(NAME, "New root")]))
            .unwrap();
        p.mark_record_session();
        let other = p.write_record(&remote(None, "New related")).unwrap();
        p.write_record(&remote(Some(1), "")).unwrap();
        let error = p.commit_record_session(false).unwrap_err();
        assert_eq!(error.code, Some(509), "{error}");
        p.rollback_record_session();
        (root, other)
    };
    let (root, other) = hold(&mut provider);
    assert_eq!(
        root.id, other.id,
        "colliding raw IDs belong to separate files"
    );
    let root_issued = issued(&root);
    let other_issued = issued(&other);
    assert_eq!(root_issued[0], "7");
    assert_eq!(other_issued[0], "7");
    assert!(!root_issued[1].is_empty() && !root_issued[2].is_empty());
    // Later legitimate input extends the old proof, rather than replacing it.
    provider
        .write_record(&related_write(None, Some(root.id), &[(COUNT, "19")]))
        .unwrap();
    let before = writes(&host);
    let stored = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    host.handle.close_connections();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let mapped = provider.take_recreated_records();
    assert_eq!(mapped.len(), 2);
    let remap = |table, id| {
        *mapped
            .iter()
            .find(|m| m.table == table && m.old == id as u64)
            .unwrap()
            .new
            .as_ref()
            .unwrap() as i64
    };
    let root_id = remap(TABLE, root.id);
    let other_id = remap(table, other.id);
    let after = writes(&host);
    assert_eq!(
        after,
        [before[0] + 2, before[1], before[2], before[3], before[4]],
        "recovery reserves IDs only after every file passes, without a save or new issued values"
    );
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), stored);
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(page.records.iter().find(|r| r.id == root_id).unwrap()),
        root_issued
    );
    provider.mark_record_session();
    let other = provider
        .write_record(&remote(Some(other_id), "New related"))
        .unwrap();
    assert_eq!(issued(&other), other_issued);
    provider
        .write_record(&remote(Some(1), "Corrected"))
        .unwrap();
    provider.commit_record_session(false).unwrap();
    for (file, id, expected) in [
        (ROOT, root_id, root_issued),
        (RELATED, other_id, other_issued),
    ] {
        let mut independent = host.open(file);
        let page = independent.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        assert_eq!(
            issued(page.records.iter().find(|r| r.id == id).unwrap()),
            expected
        );
        assert_eq!(page.records.len(), 3, "one new record saved in {file}");
    }
    assert!(rows_of(&host, ROOT)
        .iter()
        .any(|r| r.0 == root_id && r.2 == "19"));
    provider.close();

    // Start the negative epoch from identical committed bytes. Rehosting
    // then changes only Related's own rules, keeping its account/set keys.
    let restart = |tag: &str, files: Vec<(&str, OwnedNode)>| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(next) = Host::serving_at(tag, files.clone(), port, &ca) {
                break next;
            }
            assert!(
                Instant::now() < deadline,
                "old synthetic listener did not stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("custom-recovery-negative", files.clone());
    let mut refused = host.open(ROOT);
    let (held_root, held_other) = hold(&mut refused);
    let frozen = refused.carried_record_sessions();
    assert!(!frozen.is_empty());
    let unchanged = (rows_of(&host, ROOT), rows_of(&host, RELATED));
    // Root's function catalog is read separately from schema_downloads.
    // Changing only it must fail before any participant gets another RID.
    let mut changed_function = files.clone();
    function(&mut changed_function[0].1, "B");
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("custom-recovery-function-changed", changed_function);
    let denied = refused.reopen(&host.connection(ROOT)).unwrap_err();
    assert!(denied.message.starts_with("Unresolved:"), "{denied}");
    assert_eq!(
        writes(&host),
        [0; 5],
        "Root function changes precede all replacement reservations"
    );
    assert_eq!(refused.carried_record_sessions(), frozen);
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), unchanged);

    let mut replacement = files.clone();
    replacement[1].1.put(
        &[&[23], &[2], &[5], security::CUSTOM_KEY, &[2]],
        OwnedNode::leaf([], vec![0x10, 3]),
    );
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("custom-recovery-narrowed", replacement);
    let denied = refused.reopen(&host.connection(ROOT)).unwrap_err();
    assert!(denied.message.starts_with("Unresolved:"), "{denied}");
    assert_eq!(
        writes(&host),
        [0; 5],
        "no participant reserves, issues or submits before all permissions pass"
    );
    assert_eq!(
        refused.carried_record_sessions(),
        frozen,
        "all logical drafts stay retained"
    );
    assert_eq!((rows_of(&host, ROOT), rows_of(&host, RELATED)), unchanged);
    // Another explicit attempt cannot downgrade the retained proof. It
    // still sends no reservation, record data or value-generation effects.
    assert!(refused
        .reopen(&host.connection(ROOT))
        .unwrap_err()
        .message
        .starts_with("Unresolved:"));
    assert_eq!(writes(&host), [0; 5]);
    assert_eq!(refused.carried_record_sessions(), frozen);
    // Restoring the exact original own rules makes an explicit retry possible;
    // it must use the retained values, not issue another serial/clock/UUID.
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
    host = restart("custom-recovery-restored", files);
    refused.reopen(&host.connection(ROOT)).unwrap();
    let mapped = refused.take_recreated_records();
    let remap = |table, id| {
        *mapped
            .iter()
            .find(|m| m.table == table && m.old == id as u64)
            .unwrap()
            .new
            .as_ref()
            .unwrap() as i64
    };
    let root_id = remap(TABLE, held_root.id);
    let other_id = remap(table, held_other.id);
    assert_eq!(writes(&host), [2, 0, 0, 0, 0]);
    let page = refused.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        issued(page.records.iter().find(|r| r.id == root_id).unwrap()),
        issued(&held_root)
    );
    refused.mark_record_session();
    let other = refused
        .write_record(&remote(Some(other_id), "New related"))
        .unwrap();
    assert_eq!(issued(&other), issued(&held_other));
    refused.write_record(&remote(Some(1), "Corrected")).unwrap();
    refused.commit_record_session(false).unwrap();
    let mut independent = host.open(ROOT);
    let page = independent.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let marker = page
        .fields
        .iter()
        .position(|f| f.name == "MarkerValue")
        .unwrap();
    assert_eq!(
        page.records
            .iter()
            .find(|r| r.id == root_id)
            .unwrap()
            .values[marker]
            .as_deref(),
        Some("A")
    );
    independent.close();
    assert_eq!(rows_of(&host, ROOT).len(), 3);
    assert_eq!(rows_of(&host, RELATED).len(), 3);
    refused.close();
    host.handle.close_connections();
    assert!(host.handle.shutdown(Duration::from_secs(2)));
}

/// A window's save of a record with a related record of another file
/// (#1006), as the window's record session commits it (#1095).
///
/// - **Refused before any record data**, here by the related file's
///   required Name, the session stays as it was: the new Root record it
///   holds keeps its reserved id. So too when the related file's upload
///   fails before its op70, nothing applied. The window rolls back what it
///   staged and saves the corrected row once, with the new record.
/// - **Partly applied**: Root's commit refused after Related's committed.
///   The provider keeps the save, reports each file, and the window takes
///   no change until it is settled. Checked again, Root's record reads back
///   as before under the save's lock: not applied. Resume sends Root's work
///   only, from the values it computed, and the save is complete.
/// - **Partly applied again**: Revert Record discards what was not saved,
///   and Related keeps what it saved. Nothing is undone or sent again.
/// - **Partly applied, then the connection lost**: its report and the
///   window's freeze stay through a close, a second close and a reconnect,
///   with nothing sent, until it is discarded.
#[test]
fn a_windows_save_across_files_is_kept_when_refused_and_settled_when_partly_applied() {
    let mut files = portal_files_with(|related| {
        fixture::set_field_validation(related, fixture::TABLE, NAME, 0x408)
    });
    // An unused container field does not create asset ownership to recover.
    for (_, file) in &mut files {
        fixture::add_field(file, TABLE, 90, "Attachment", 1, 6, 0, 0, 0);
    }
    let host = Host::new("related-owner-ordinary-save", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let remote = |record, name: &str| related_write(Some(table), record, &[(NAME, name)]);
    let roots = |host: &Host| rows_of(host, ROOT).len();
    let before = roots(&host);

    // A new Root record and a related row the related file refuses.
    provider.begin_record_commit().unwrap();
    let mut made = provider
        .write_record(&related_write(None, None, &[(NAME, "New root")]))
        .unwrap();
    provider.mark_record_session();
    provider.write_record(&remote(None, "New related")).unwrap();
    provider.write_record(&remote(Some(1), "")).unwrap();
    let refused = provider.commit_record_session(false).unwrap_err();
    assert_eq!(refused.code, Some(509), "{refused}");
    assert!(provider.record_session_open(), "the new record stays held");
    assert!(provider.unsettled_save().is_none(), "nothing was applied");
    provider.rollback_record_session();
    // A public caller can correct the held Root after the refused save.
    // Reconnect must carry this successful write, not the refusal's older copy.
    provider
        .write_record(&related_write(
            None,
            Some(made.id),
            &[(NAME, "Corrected root")],
        ))
        .unwrap();
    assert!(!provider.carried_record_sessions().is_empty());
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    assert_eq!(roots(&host), before, "Root is still unsaved");
    host.handle.close_connections();
    provider.reopen(&host.connection(ROOT)).unwrap();
    let recreated = provider.take_recreated_records();
    made.id = recreated
        .iter()
        .find(|record| record.table == TABLE && record.old == made.id as u64)
        .unwrap()
        .new
        .as_ref()
        .copied()
        .unwrap() as i64;
    let held = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let name = held
        .fields
        .iter()
        .position(|field| field.name == "Name")
        .unwrap();
    assert_eq!(
        held.records
            .iter()
            .find(|record| record.id == made.id)
            .unwrap()
            .values[name]
            .as_deref(),
        Some("Corrected root"),
        "the latest accepted Root edit survives reconnect"
    );
    assert_eq!(roots(&host), before, "recovery stores no Root record");
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    // Past every preflight, the related file's upload fails before its
    // op70 (its new rows larger than one request): nothing is applied, so
    // the session stays as it was too.
    provider.mark_record_session();
    let long = "x".repeat(62_000);
    for _ in 0..18 {
        provider.write_record(&remote(None, &long)).unwrap();
    }
    let failed = provider.commit_record_session(false).unwrap_err();
    assert!(
        failed.message.contains("larger than one request"),
        "{failed}"
    );
    assert!(provider.record_session_open(), "the new record stays held");
    assert!(provider.unsettled_save().is_none(), "nothing was applied");
    provider.rollback_record_session();
    provider.write_record(&remote(Some(1), "Fixed")).unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(roots(&host), before + 1, "the new record, saved once");
    assert!(rows_of(&host, ROOT)
        .iter()
        .any(|row| row.0 == made.id && row.1 == "Corrected root"));
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Fixed"));

    // Root's commit refused after Related's committed: partly applied.
    provider.begin_record_commit().unwrap();
    provider.write_record(&remote(Some(1), "Saved")).unwrap();
    provider
        .write_record(&related_write(None, Some(made.id), &[(NAME, "Not saved")]))
        .unwrap();
    let barrier = host.block_saves(0);
    let error = provider.commit_record_session(false).unwrap_err();
    drop(barrier);
    let outcomes = |files: &[om_data::FileCommit]| {
        files
            .iter()
            .map(|file| {
                (
                    file.file.rsplit('/').next().unwrap().to_owned(),
                    file.outcome,
                )
            })
            .collect::<Vec<_>>()
    };
    let report = vec![
        ("Related".to_owned(), om_data::FileOutcome::Committed),
        ("Root".to_owned(), om_data::FileOutcome::Unknown),
    ];
    assert_eq!(outcomes(&error.commit), report);
    assert_eq!(outcomes(&provider.unsettled_save().unwrap()), report);

    // Frozen until settled: no change is taken, and Browse edits nothing.
    let frozen = provider
        .write_record(&remote(Some(1), "Later"))
        .unwrap_err();
    assert_eq!(frozen.code, Some(3), "{frozen}");
    assert!(provider.begin_record_commit().is_err());
    let capabilities = provider
        .record_capabilities(fixture::LAYOUT as u32)
        .unwrap();
    assert!(
        !capabilities.update && !capabilities.create,
        "{capabilities:?}"
    );

    // Checked again: Root's record read back as before its commit, under
    // the lock the save still holds, is proven not applied; Related's stays
    // saved. Resume sends Root's work only, from the values it computed:
    // Related is sent nothing again.
    let checked = provider.reconcile_save().unwrap();
    assert_eq!(
        outcomes(&checked),
        [
            ("Related".to_owned(), om_data::FileOutcome::Committed),
            ("Root".to_owned(), om_data::FileOutcome::NotApplied),
        ]
    );
    assert!(
        provider.write_record(&remote(Some(1), "Later")).is_err(),
        "still frozen"
    );
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    provider.resume_save().unwrap();
    assert_eq!(
        (commits_to(&host, ROOT), commits_to(&host, RELATED)),
        (sent.0 + 1, sent.1),
        "Root's work once more, Related's not again"
    );
    assert!(provider.unsettled_save().is_none());
    assert!(rows_of(&host, ROOT)
        .iter()
        .any(|row| row.0 == made.id && row.1 == "Not saved"));
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Saved"));

    // Partly applied again: Revert Record discards what was not saved, and
    // Related keeps its save.
    provider.begin_record_commit().unwrap();
    provider.write_record(&remote(Some(1), "Kept")).unwrap();
    provider
        .write_record(&related_write(None, Some(made.id), &[(NAME, "Discarded")]))
        .unwrap();
    let barrier = host.block_saves(0);
    provider.commit_record_session(false).unwrap_err();
    drop(barrier);
    provider.revert_record_session().unwrap();
    assert!(provider.unsettled_save().is_none());
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Kept"));
    assert!(rows_of(&host, ROOT)
        .iter()
        .any(|row| row.0 == made.id && row.1 == "Not saved"));
    // Its locks are gone: both records take a new save.
    provider.begin_record_commit().unwrap();
    provider.write_record(&remote(Some(1), "After")).unwrap();
    provider
        .write_record(&related_write(None, Some(made.id), &[(NAME, "After")]))
        .unwrap();
    provider.commit_record_session(false).unwrap();
    assert!(rows_of(&host, ROOT)
        .iter()
        .any(|row| row.0 == made.id && row.1 == "After"));

    // Partly applied, then the connection is lost. The report and the
    // window's freeze outlast every close and reconnect after it, the
    // second close too, until the save is discarded, and nothing is sent
    // again: Root's part is unresolved, Related's saved.
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&remote(Some(1), "Before loss"))
        .unwrap();
    provider
        .write_record(&related_write(None, Some(made.id), &[(NAME, "Lost")]))
        .unwrap();
    let barrier = host.block_saves(0);
    provider.commit_record_session(false).unwrap_err();
    drop(barrier);
    host.handle.close_connections();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while provider.connection_lost().is_none() && std::time::Instant::now() < deadline {
        let _ = provider.records(fixture::LAYOUT as u32, 0, 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(provider.connection_lost().is_some());
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    let kept = vec![
        ("Related".to_owned(), om_data::FileOutcome::Committed),
        ("Root".to_owned(), om_data::FileOutcome::Unresolved),
    ];
    let frozen = |provider: &mut NativeProvider, step: &str| {
        let report = provider
            .unsettled_save()
            .unwrap_or_else(|| panic!("{step}: kept"));
        assert_eq!(outcomes(&report), kept, "{step}");
        let refused = provider
            .write_record(&remote(Some(1), "Later"))
            .unwrap_err();
        assert_eq!(refused.code, Some(3), "{step}: {refused}");
    };
    provider.close();
    frozen(&mut provider, "the first close");
    provider.close();
    frozen(&mut provider, "a second close");
    provider.open(&host.connection(ROOT)).unwrap();
    frozen(&mut provider, "a reconnect");
    assert_eq!(
        (commits_to(&host, ROOT), commits_to(&host, RELATED)),
        sent,
        "nothing sent again"
    );
    provider.discard_save().unwrap();
    assert!(provider.unsettled_save().is_none());
    provider.begin_record_commit().unwrap();
    provider.write_record(&remote(Some(1), "Settled")).unwrap();
    provider.commit_record_session(false).unwrap();
    assert!(rows_of(&host, RELATED).iter().any(|row| row.1 == "Settled"));
    provider.close();
}

/// One name on two hosts is two files (#976): a write through Root's
/// occurrence of the other host's Related is that file's, not the
/// same-named file beside Root.
#[test]
fn related_writes_reach_the_file_on_the_host_its_occurrence_names() {
    const LOCAL: u64 = 0xd0_0043;
    let other = Host::new(
        "related-owner-other-host",
        vec![(RELATED, fixture::tree(&[], true))],
    );
    let mut root = fixture::tree(&[], true);
    let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (REMOTE, "Remote Tasks"),
        1,
        "Elsewhere",
        &[("fmnet", &elsewhere, "Related")],
    );
    reference(&mut root, (LOCAL, "Local Tasks"), 2, "Related");
    let host = Host::new(
        "related-owner-this-host",
        vec![(ROOT, root), (RELATED, fixture::tree(&[], true))],
    );
    let mut provider = host.provider_trusting(&other);
    provider.open(&host.connection(ROOT)).unwrap();
    // The other host admits Root's account, offered there once (#1010).
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(provider.related_sign_ins().is_empty());
    let there = schema.table_of(REMOTE).unwrap().id;
    assert_ne!(Some(there), schema.table_of(LOCAL).map(|t| t.id));

    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&related_write(Some(there), Some(1), &[(NAME, "There")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    let name = |host: &Host| rows_of(host, RELATED)[0].1.clone();
    assert_eq!(name(&other), "There");
    assert_eq!(name(&host), "Write tests");
    assert_eq!(
        (commits_to(&other, RELATED), commits_to(&host, RELATED)),
        (1, 0)
    );

    // The window's save of a Root record with the other host's related
    // record, on two connections (#1006). The other host refuses the
    // related file's op70, which goes first: its outcome is unknown, so the
    // save is kept, Root's work not sent. Checked again, it reads back as
    // before under the lock its own transaction still holds: not applied.
    // Resume sends it and Root's work, once each.
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(
            Some(there),
            Some(1),
            &[(NAME, "Both hosts")],
        ))
        .unwrap();
    provider
        .write_record(&related_write(None, Some(1), &[(NAME, "Root too")]))
        .unwrap();
    let barrier = other.block_saves(0);
    let error = provider.commit_record_session(false).unwrap_err();
    drop(barrier);
    let outcomes =
        |files: &[om_data::FileCommit]| files.iter().map(|file| file.outcome).collect::<Vec<_>>();
    use om_data::FileOutcome::{NotApplied, NotSubmitted, Unknown};
    assert_eq!(outcomes(&error.commit), [Unknown, NotSubmitted]);
    assert_ne!(
        error.commit[0].participant, error.commit[1].participant,
        "two files, told apart by identity"
    );
    assert_eq!(
        outcomes(&provider.reconcile_save().unwrap()),
        [NotApplied, NotSubmitted]
    );
    let roots = commits_to(&host, ROOT);
    provider.resume_save().unwrap();
    assert!(provider.unsettled_save().is_none());
    assert_eq!(name(&other), "Both hosts");
    assert_eq!(commits_to(&host, ROOT), roots + 1);
    assert!(rows_of(&host, ROOT).iter().any(|row| row.1 == "Root too"));
    provider.close();
}

/// How many op62 Unlocks (as a file closes) `host` got for `file`.
fn closes_of(host: &Host, file: &str) -> usize {
    host.captures
        .with_opcode(om_protocol::Opcode::Unlock)
        .iter()
        .filter(|(name, _)| name == file)
        .count()
}

/// A file a connection has open is held by each reader of it there
/// (#1011): Root's related Related and a script's Related, and Root's and
/// Other's related Lookup, are each opened and logged in to once, on one
/// connection. A write by another client notifies both of Related's
/// holders. Root closing first leaves the script's Related its open
/// record and its lock (no op62); its own close then sends the one op62.
#[test]
fn readers_of_one_file_on_a_connection_hold_one_open_of_it() {
    const LOOKUP: u64 = 0xd0_0044;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference(&mut root, (LOOKUP, "Lookup Tasks"), 2, "Lookup");
    let mut other = fixture::tree(&[], true);
    reference(&mut other, (LOOKUP, "Lookup Tasks"), 1, "Lookup");
    let host = Host::new(
        "held-files",
        vec![
            (ROOT, root),
            (RELATED, fixture::tree(&[], true)),
            ("Other.fmp12", other),
            ("Lookup.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = host.open(ROOT);
    let remote = root
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    let mut related = root.open_peer(RELATED).unwrap();
    let mut other = root.open_peer("Other.fmp12").unwrap();
    other.calc_schema().unwrap();
    let (opens, logins) = host.captures.opens_and_logins();
    for file in [RELATED, "Lookup.fmp12", "Other.fmp12"] {
        assert_eq!(opens.get(file), Some(&1), "{file} {opens:?}");
        assert_eq!(logins.get(file), Some(&1), "{file} {logins:?}");
    }
    assert_eq!(host.captures.connections(), 1);

    let record = |table, record| CalcNeed::Record { table, record };
    let root_before = root.calc_data(&[record(remote, 1)]).unwrap().generation;
    let related_before = related.calc_data(&[record(TABLE, 1)]).unwrap().generation;
    let mut writer = host.open(RELATED);
    writer.begin_transaction(Default::default()).unwrap();
    writer
        .write_record(&related_write(None, Some(1), &[(NAME, "Elsewhere")]))
        .unwrap();
    writer.commit_transaction(false).unwrap();
    writer.close();
    // A record no one asked for yet: a request, on whose reply the host's
    // notification comes, for each holder to take its own copy.
    assert_ne!(
        root.calc_data(&[record(remote, 99)]).unwrap().generation,
        root_before,
        "Root's related Related was notified"
    );
    assert_ne!(
        related.calc_data(&[record(TABLE, 98)]).unwrap().generation,
        related_before,
        "the script's Related was notified"
    );

    related.begin_transaction(Default::default()).unwrap();
    related
        .write_record(&related_write(None, Some(1), &[(NAME, "Kept")]))
        .unwrap();
    root.close();
    assert_eq!(
        closes_of(&host, RELATED),
        0,
        "Related stays open for its holder"
    );
    related.commit_transaction(false).unwrap();
    // A record download (op92) the host answers, after Root closed.
    let downloads = || {
        host.captures
            .row_downloads()
            .iter()
            .filter(|(file, _, _)| file == RELATED)
            .count()
    };
    let before = downloads();
    let page = related.records(fixture::LAYOUT as u32, 0, 1).unwrap();
    assert_eq!(page.records[0].id, 1);
    assert!(downloads() > before, "read from the host after Root closed");
    related.close();
    assert_eq!(closes_of(&host, RELATED), 1, "closed with its last holder");
    other.close();
    assert_eq!(rows_of(&host, RELATED)[0].1, "Kept");
}

/// Holders of one file on a connection letting go at once, and one
/// letting go while another claims the file (#1011, review round 2): the
/// last holder alone sends the op62, every time, and a handle holding the
/// file can read it, so no op62 went under it. Each round races the two on
/// threads; the connection stays up through another handle. A handle
/// holding the file sends no second Open of it.
#[test]
fn holders_letting_go_at_once_close_the_file_once_and_never_under_a_holder() {
    use om_session::{Admission, Claim};
    use std::sync::{Arc, Barrier};
    let host = Host::new("held-races", vec![(RELATED, fixture::tree(&[], true))]);
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let keep = om_session::Session::connect_verified(
        "127.0.0.1",
        host.port,
        client.cert.pem().as_bytes(),
        client.key_pair.serialize_pem().as_bytes(),
        "127.0.0.1",
        &om_protocol::MachineInfo::default(),
        host.ca_pem(),
    )
    .unwrap();
    let admission = Admission::from_digest([7; 32]);
    let open = |handle: &mut om_session::Session| {
        assert!(matches!(
            handle.claim_file(RELATED, &admission),
            Ok(Claim::Open)
        ));
        assert_eq!(handle.open_file("127.0.0.1", RELATED).unwrap().status, 0);
        let creation = handle.fetch_internal_creation().unwrap();
        let environment = om_protocol::LoginEnvironment::for_remote_file("127.0.0.1", RELATED);
        let login = handle
            .login(ACCOUNT, fixture::PASSWORD, creation, &environment)
            .unwrap();
        assert!(login.is_success());
        handle.request_notifications().unwrap();
        handle.admit_file(admission.clone()).unwrap();
    };
    // A handle holding the file sends no second Open of it.
    let mut holder = keep.share();
    open(&mut holder);
    assert!(holder.open_file("127.0.0.1", RELATED).is_err());
    drop(holder);
    for _ in 0..48 {
        let mut first = keep.share();
        open(&mut first);
        let mut second = keep.share();
        assert!(matches!(
            second.claim_file(RELATED, &admission),
            Ok(Claim::Joined(_))
        ));
        let before = closes_of(&host, RELATED);
        let barrier = Arc::new(Barrier::new(2));
        let racer = std::thread::spawn({
            let barrier = barrier.clone();
            move || {
                barrier.wait();
                first.close_file().unwrap();
            }
        });
        barrier.wait();
        second.close_file().unwrap();
        racer.join().unwrap();
        assert_eq!(closes_of(&host, RELATED), before + 1, "one op62 for both");

        let mut first = keep.share();
        open(&mut first);
        let mut second = keep.share();
        let racer = std::thread::spawn({
            let barrier = barrier.clone();
            move || {
                barrier.wait();
                first.close_file().unwrap();
            }
        });
        barrier.wait();
        match second.claim_file(RELATED, &admission) {
            // Joined before the first let go: the file is still open.
            Ok(Claim::Joined(_)) => {
                second.fetch_internal_creation().unwrap();
            }
            // After it closed: this handle opens it again.
            Ok(Claim::Open) => {
                racer.join().unwrap();
                assert_eq!(second.open_file("127.0.0.1", RELATED).unwrap().status, 0);
                second.fetch_internal_creation().unwrap();
                continue;
            }
            // While it closed: a connection of its own would open it.
            Err(om_session::SessionError::FileHeld) => {}
            Err(error) => panic!("{error}"),
        }
        racer.join().unwrap();
    }
}

/// A file open on a connection is not held for another account: a script's
/// Related opened with another account of Related's opens on a connection
/// of its own, and Root's related Related keeps its own open of it (#1011).
#[test]
fn another_account_opens_the_file_on_a_connection_of_its_own() {
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    let host = Host::new(
        "held-for-another",
        vec![(ROOT, root), (RELATED, fixture::tree(&[], true))],
    );
    let mut root = host.open(ROOT);
    root.calc_schema().unwrap().unwrap();
    let mut related = root
        .open_peer_as(RELATED, fixture::CLERK, fixture::CLERK_PASSWORD)
        .unwrap();
    assert_eq!(host.captures.connections(), 2);
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get(RELATED), Some(&2), "{opens:?}");
    assert_eq!(logins.get(RELATED), Some(&2), "{logins:?}");
    related.close();
    assert!(root
        .calc_data(&[CalcNeed::Record {
            table: TABLE,
            record: 1
        }])
        .is_ok());
    root.close();
}

/// A related record read back after a reconnect is pinned at the version
/// read then (#152): its save answers to that version even after a later
/// read saw another client's change, so the edit cannot overwrite it
/// unasked (306).
#[test]
fn a_related_record_read_back_after_a_reconnect_keeps_its_version_for_the_save() {
    let host = Host::new("related-reconnect-version", files());
    let mut root = host.open(ROOT);
    root.calc_schema().unwrap();
    for client in host.handle.clients() {
        host.handle.disconnect_client(client.id);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while root.connection_lost().is_none() && Instant::now() < deadline {
        let _ = root.records(fixture::LAYOUT as u32, 0, 1);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(root.connection_lost().is_some());
    root.reopen(&host.connection(ROOT)).unwrap();
    let table = root
        .calc_schema()
        .unwrap()
        .unwrap()
        .table_of(REMOTE)
        .unwrap()
        .id;
    let record = |root: &mut NativeProvider, id: u64| {
        let data = root
            .calc_data(&[CalcNeed::Record { table, record: id }])
            .unwrap();
        data.records[&(table, id)].clone().unwrap().mod_count
    };
    let version = record(&mut root, 1).unwrap();
    assert!(root.retain_related_record(table, 1, version));

    let mut writer = host.open(RELATED);
    writer.begin_transaction(Default::default()).unwrap();
    writer
        .write_record(&related_write(None, Some(1), &[(NAME, "Elsewhere")]))
        .unwrap();
    writer.commit_transaction(false).unwrap();
    writer.close();
    // A later read sees the change (a record not asked for yet brings the
    // notification, then the record itself is read again).
    let _ = root.calc_data(&[CalcNeed::Record { table, record: 99 }]);
    assert_ne!(record(&mut root, 1), Some(version), "the change was seen");

    root.begin_transaction(Default::default()).unwrap();
    let saved = root
        .write_record(&related_write(Some(table), Some(1), &[(NAME, "Mine")]))
        .and_then(|_| root.commit_transaction(false));
    let error = saved.expect_err("answers to the version read back");
    assert_eq!(error.code, Some(306), "{error:?}");
    let _ = root.revert_transaction();
    assert_eq!(rows_of(&host, RELATED)[0].1, "Elsewhere", "not overwritten");
    root.close();
}

#[test]
fn a_rebuilt_schema_imports_the_open_related_files_again_without_logging_in() {
    let host = Host::new("rebuild", files());
    let mut provider = host.open(ROOT);
    let first = provider.calc_schema().unwrap().unwrap();
    let remote = first.table_of(REMOTE).unwrap().id;
    // Manage Database reads the catalogs again; the schema is rebuilt.
    provider.database_schema().unwrap();
    let second = provider.calc_schema().unwrap().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(second.table_of(REMOTE).unwrap().id, remote);
    let window = Window::new(&mut provider);
    assert_eq!(window.read(&["Remote Tasks::Count"]), [(0, "37".into())]);
    let (_, logins) = host.captures.opens_and_logins();
    assert_eq!(logins.get(RELATED), Some(&1), "{logins:?}");
}

/// Root and Related share one connection (#740): the keepalive keeps it
/// past the host's idle timeout, and its loss is every file's on it,
/// reported as the host's.
#[test]
fn the_shared_connection_is_kept_alive_and_its_loss_reported() {
    let host = Host::with_idle_timeout("lost", files(), Some(Duration::from_millis(500)));
    let mut provider = host
        .provider()
        .with_keepalive(Some(Duration::from_millis(100)));
    provider.open(&host.connection(ROOT)).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let related = || {
        host.handle
            .clients()
            .into_iter()
            .find(|client| client.files.iter().any(|file| file.name == RELATED))
    };
    assert!(related().is_some());
    assert_eq!(host.handle.clients().len(), 1, "one connection");

    // Idle past the host's timeout: the keepalive keeps the connection.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(provider.connection_lost().is_none());
    let data = provider
        .calc_data(&[CalcNeed::Record {
            table: remote,
            record: 1,
        }])
        .unwrap();
    assert!(data.records[&(remote, 1)].is_some());

    assert!(host.handle.disconnect_client(related().unwrap().id));
    let deadline = Instant::now() + Duration::from_secs(10);
    let lost = loop {
        if let Some(lost) = provider.connection_lost() {
            break lost;
        }
        assert!(Instant::now() < deadline, "the loss went unnoticed");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(lost.lost);
    assert!(
        lost.message
            .contains("The connection to the host was lost ("),
        "{lost:?}"
    );
    // The related table's needs fail; they do not read as empty.
    let error = provider
        .calc_data(&[CalcNeed::Record {
            table: remote,
            record: 2,
        }])
        .unwrap_err();
    assert!(
        error
            .message
            .contains("the connection to the host was lost"),
        "{error:?}"
    );
    let started = Instant::now();
    provider.close();
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn closing_disconnects_every_related_file() {
    let host = Host::new("close", files());
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let open: Vec<String> = host
        .handle
        .clients()
        .iter()
        .flat_map(|client| client.files.iter().map(|file| file.name.clone()))
        .collect();
    assert_eq!(open.len(), 2, "{open:?}");
    assert!(open.contains(&RELATED.to_string()));
    provider.close();
    assert!(provider.related_file_warnings().is_empty());
    let deadline = Instant::now() + Duration::from_secs(10);
    while host
        .handle
        .clients()
        .iter()
        .any(|client| !client.files.is_empty())
    {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            host.handle.clients().len()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cold_and_warm_root_finds_keep_related_file_identity_and_preflight() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    indexed(&mut root, NAME);
    indexed(&mut child, NAME);
    // The files deliberately disagree about the matching record's identity.
    put(&mut child, 1, NAME, "Different");
    put(&mut child, 2, NAME, "Write tests");
    let host = Host::configured(
        "find-fallback-with-related-schema",
        vec![(ROOT, root), (RELATED, child)],
        om_server::Server::with_late_index_builds,
    );
    let mut provider = host.open(ROOT);
    provider.calc_schema().unwrap();
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let downloads = || {
        host.captures
            .row_downloads()
            .into_iter()
            .filter(|(file, table, _)| file == ROOT && *table == TABLE)
            .map(|(_, _, count)| count)
            .sum::<usize>()
    };
    for expected in [2, 0] {
        let before = downloads();
        let mut request = FindRequest::default();
        request.set("Name", "==Write tests");
        let result = provider
            .change_found_set(
                layout,
                FoundSetChange::Find {
                    requests: vec![request],
                    kind: FindKind::Replace,
                },
            )
            .unwrap();
        assert_eq!(result.found, 1);
        assert_eq!(
            downloads() - before,
            expected,
            "cold fallback then warm index"
        );
        assert_eq!(
            provider
                .records(layout, 0, 10)
                .unwrap()
                .records
                .iter()
                .map(|record| record.id)
                .collect::<Vec<_>>(),
            [1]
        );
        for kind in [FindKind::Replace, FindKind::Constrain, FindKind::Extend] {
            let mut local = FindRequest::default();
            local.set("Name", "==Ship");
            let mut related = FindRequest::default();
            related.set("Remote Tasks::Name", "==Write tests");
            let queries = host
                .captures
                .with_opcode(om_protocol::Opcode::QueryProgram)
                .len();
            let error = provider
                .change_found_set(
                    layout,
                    FoundSetChange::Find {
                        requests: vec![local, related],
                        kind,
                    },
                )
                .unwrap_err();
            assert_eq!(error.code, Some(3));
            assert_eq!(
                host.captures
                    .with_opcode(om_protocol::Opcode::QueryProgram)
                    .len(),
                queries
            );
            assert_eq!(
                provider
                    .records(layout, 0, 10)
                    .unwrap()
                    .records
                    .iter()
                    .map(|record| record.id)
                    .collect::<Vec<_>>(),
                [1],
                "refusal preserves the found set"
            );
        }
    }
}

/// A related record made in the same transaction as its parent reads the
/// parent through number match keys (#187 part B). One transaction holds a
/// new Tasks record, whose Number JobKey is a serial New Record took (7)
/// and whose Count is 21, and a Notes record made through Tasks' layout,
/// as a portal row is, whose Number JobKey holds `7.0` and whose
/// auto-entered calculation reads `Tasks::Doubled` (a stored `Count * 2`
/// the commit computes). At Commit Transaction it reads 42 from the parent
/// as the commit writes it, matched as a number (`7.0` = 7, which text
/// would not match); nothing was committed before. A child changed before
/// its parent reads the final parent too, including an acyclic chain of
/// records of the same table. A genuine cycle refuses before upload.
#[test]
fn a_held_parent_answers_its_related_records_number_key_formula() {
    use fixture::{LAYOUT, NOTES, NOTES_LAYOUT, NOTES_OCCURRENCE};
    const JOB_KEY: u64 = 30;
    const SEEN: u64 = 31;
    const PARENT_KEY: u64 = 32;
    const ANCESTORS: u64 = 33;
    const SELF_KEY: u64 = 34;
    const CREATION_UUID: u64 = 35;
    const PARENTS: u64 = 0xd0_0009;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_field(TABLE, JOB_KEY, "JobKey");
    names.add_field(NOTES, JOB_KEY, "JobKey");
    names.add_field(NOTES, SEEN, "Seen");
    fixture::add_field(&mut root, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0x2, 0);
    fixture::set_field_serial_increment(&mut root, TABLE, JOB_KEY, 1);
    root.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(JOB_KEY),
            &[0x0c],
        ],
        om_scripts::text_node(&[], "7"),
    );
    fixture::add_field(&mut root, NOTES, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
    fixture::add_field(&mut root, NOTES, SEEN, "Seen", 1, 2, 0, 0x8, 0);
    fixture::add_relationship(
        &mut root,
        11,
        (NOTES_OCCURRENCE, JOB_KEY),
        0,
        (OCCURRENCE, JOB_KEY),
    );
    names.add_field(TABLE, CREATION_UUID, "CreationUUID");
    fixture::add_field(
        &mut root,
        TABLE,
        CREATION_UUID,
        "CreationUUID",
        1,
        1,
        0,
        0x88,
        0,
    );
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        CREATION_UUID,
        5,
        "Get ( UUID )",
    );
    let seen = "Tasks::Doubled";
    fixture::add_formula(&mut root, &names, (NOTES, NOTES_OCCURRENCE), SEEN, 5, seen);
    // A self-join gives several held rows of the same table an acyclic
    // dependency chain; table-level sorting cannot distinguish its rows.
    names.add_occurrence(PARENTS, "Parents", TABLE);
    names.add_field(TABLE, PARENT_KEY, "ParentKey");
    names.add_field(TABLE, ANCESTORS, "Ancestors");
    names.add_field(TABLE, SELF_KEY, "SelfKey");
    fixture::add_field(&mut root, TABLE, SELF_KEY, "SelfKey", 1, 2, 0, 0, 0);
    fixture::add_occurrence(&mut root, PARENTS, "Parents", TABLE);
    fixture::add_field(&mut root, TABLE, PARENT_KEY, "ParentKey", 1, 2, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, ANCESTORS, "Ancestors", 1, 2, 0, 0xa8, 0);
    fixture::add_relationship(
        &mut root,
        12,
        (OCCURRENCE, PARENT_KEY),
        0,
        (PARENTS, SELF_KEY),
    );
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        ANCESTORS,
        5,
        "Parents::Ancestors + Parents::Doubled",
    );
    fixture::add_tasks(&mut root, 101..=103);
    for (record, key) in [(101, "91"), (102, "92"), (103, "93")] {
        put(&mut root, record, SELF_KEY, key);
        put(&mut root, record, ANCESTORS, "0");
    }
    let mut serial_root = root.clone();
    fixture::add_relationship(
        &mut serial_root,
        11,
        (NOTES_OCCURRENCE, JOB_KEY),
        0,
        (OCCURRENCE, SELF_KEY),
    );
    fixture::add_formula(
        &mut serial_root,
        &names,
        (NOTES, NOTES_OCCURRENCE),
        SEEN,
        5,
        "Tasks::JobKey",
    );
    let mut inactive_root = root.clone();
    const RANDOM_KEY: u64 = 36;
    const CHECKED: u64 = 37;
    const CHECKS: u64 = 0xd0_0010;
    names.add_field(TABLE, RANDOM_KEY, "RandomKey");
    names.add_field(TABLE, CHECKED, "Checked");
    names.add_occurrence(CHECKS, "Checks", NOTES);
    fixture::add_occurrence(&mut inactive_root, CHECKS, "Checks", NOTES);
    fixture::add_field(
        &mut inactive_root,
        TABLE,
        RANDOM_KEY,
        "RandomKey",
        1,
        2,
        0,
        0xa8,
        0,
    );
    fixture::add_formula(
        &mut inactive_root,
        &names,
        (TABLE, OCCURRENCE),
        RANDOM_KEY,
        5,
        "Random + Count * 0",
    );
    fixture::add_field(
        &mut inactive_root,
        TABLE,
        CHECKED,
        "Checked",
        1,
        1,
        0,
        0,
        0x401,
    );
    fixture::add_formula(
        &mut inactive_root,
        &names,
        (TABLE, OCCURRENCE),
        CHECKED,
        0x19,
        "IsEmpty ( Checks::JobKey ) or not IsEmpty ( Checks::JobKey )",
    );
    fixture::add_relationship(
        &mut inactive_root,
        13,
        (OCCURRENCE, RANDOM_KEY),
        0,
        (CHECKS, JOB_KEY),
    );
    for record in [1, 2] {
        put(&mut inactive_root, record, RANDOM_KEY, "1");
        put(&mut inactive_root, record, CHECKED, "unchanged");
    }
    let host = Host::new("held-parent", vec![(ROOT, root)]);
    let mut provider = host.open(ROOT);
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .create_record(
            LAYOUT as u32,
            [("Count".to_owned(), Some("21".to_owned()))].into(),
        )
        .unwrap();
    provider
        .write_record(&om_data::ScriptWrite {
            layout: LAYOUT as u32,
            table: Some(NOTES),
            cells: [(JOB_KEY, Some("7.0".to_owned()))].into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        commits_to(&host, ROOT),
        0,
        "nothing committed before Commit"
    );
    provider.commit_transaction(false).unwrap();
    let page = provider.records(NOTES_LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let seen: Vec<_> = page
        .records
        .iter()
        .map(|record| record.values[at("Seen")].clone())
        .collect();
    assert!(seen.contains(&Some("42".to_owned())), "{seen:?}");
    // The child first, then its parent (serial 8): preparation must read
    // the parent's final Doubled=10, not its raw/uncomputed held cells.
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .write_record(&om_data::ScriptWrite {
            layout: LAYOUT as u32,
            table: Some(NOTES),
            cells: [(JOB_KEY, Some("8".to_owned()))].into(),
            ..Default::default()
        })
        .unwrap();
    let made = provider
        .create_record(
            LAYOUT as u32,
            [("Count".to_owned(), Some("5".to_owned()))].into(),
        )
        .unwrap();
    let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
    let uuid_at = page
        .fields
        .iter()
        .position(|f| f.name == "CreationUUID")
        .unwrap();
    let uuid = page
        .records
        .iter()
        .find(|r| r.id == made.id)
        .unwrap()
        .values[uuid_at]
        .clone();
    assert!(uuid.as_ref().is_some_and(|s| !s.is_empty()));
    let serials = host
        .captures
        .with_opcode(om_protocol::Opcode::HostGetSerial)
        .len();
    let reservations = host
        .captures
        .with_opcode(om_protocol::Opcode::HostCreateRecord)
        .len();
    provider.commit_transaction(false).unwrap();
    let page = provider.records(NOTES_LAYOUT as u32, 0, 100).unwrap();
    let at = page.fields.iter().position(|f| f.name == "Seen").unwrap();
    assert!(page
        .records
        .iter()
        .any(|record| record.values[at].as_deref() == Some("10")));
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::HostGetSerial)
            .len(),
        serials
    );
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::HostCreateRecord)
            .len(),
        reservations
    );
    let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
    assert_eq!(
        page.records
            .iter()
            .find(|r| r.id == made.id)
            .unwrap()
            .values[uuid_at],
        uuid,
        "dependency discovery and final preparation retain the creation UUID"
    );
    // Child, grandchild, parent: existing records of the same table. The
    // new-record self-membership limit (#1035) is separate from this case.
    provider.begin_transaction(Default::default()).unwrap();
    for (record, count, parent) in [(101, "4", "93"), (102, "2", "91"), (103, "6", "")] {
        provider
            .update_record(
                LAYOUT as u32,
                record,
                [
                    ("Count".to_owned(), Some(count.to_owned())),
                    ("ParentKey".to_owned(), Some(parent.to_owned())),
                ]
                .into(),
            )
            .unwrap();
    }
    provider.commit_transaction(false).unwrap();
    let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
    let at = page
        .fields
        .iter()
        .position(|f| f.name == "Ancestors")
        .unwrap();
    let seen = |id| page.records.iter().find(|r| r.id == id).unwrap().values[at].as_deref();
    assert_eq!(seen(103), Some("0"));
    assert_eq!(seen(101), Some("12"));
    assert_eq!(seen(102), Some("20"));
    // A genuine pair of mutually dependent computed values remains
    // unavailable. Neither an upload nor a commit may precede the refusal.
    provider.begin_transaction(Default::default()).unwrap();
    for (record, parent) in [(102, "93"), (103, "92")] {
        provider
            .update_record(
                LAYOUT as u32,
                record,
                [
                    ("Count".to_owned(), Some("1".to_owned())),
                    ("ParentKey".to_owned(), Some(parent.to_owned())),
                ]
                .into(),
            )
            .unwrap();
    }
    let uploads = host
        .captures
        .with_opcode(om_protocol::Opcode::UploadListToHostTempLocation)
        .len();
    let commits = commits_to(&host, ROOT);
    let refused = provider.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(
        refused.message.contains("cyclic or unresolved"),
        "{refused:?}"
    );
    assert_eq!(
        host.captures
            .with_opcode(om_protocol::Opcode::UploadListToHostTempLocation)
            .len(),
        uploads
    );
    assert_eq!(commits_to(&host, ROOT), commits);
    // A deletion hides the old host row immediately and evaluates no
    // outgoing formula, even when a reader was changed before the delete.
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .update_record(
            LAYOUT as u32,
            102,
            [("ParentKey".to_owned(), Some("93".to_owned()))].into(),
        )
        .unwrap();
    provider.delete_record(LAYOUT as u32, 103).unwrap();
    provider.commit_transaction(false).unwrap();
    let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
    assert!(!page.records.iter().any(|r| r.id == 103));
    assert_eq!(
        page.records.iter().find(|r| r.id == 102).unwrap().values[at].as_deref(),
        None,
        "a deleted parent contributes no old stored calculation"
    );
    // An imported parent edited since, then an imported child reading it:
    // the child's rules run at its import, before the parent's edit is
    // computed, so it is refused rather than store the old value.
    provider.begin_transaction(Default::default()).unwrap();
    let cells = |pairs: &[(u64, &str)]| {
        pairs
            .iter()
            .map(|(cell, text)| (*cell, Some((*text).to_owned())))
            .collect()
    };
    let parent = provider
        .import_record(LAYOUT as u32, None, &cells(&[(COUNT, "3")]), true)
        .unwrap();
    provider
        .update_record(
            LAYOUT as u32,
            parent,
            [("Count".to_owned(), Some("4".to_owned()))].into(),
        )
        .unwrap();
    let refused = provider
        .import_record(NOTES_LAYOUT as u32, None, &cells(&[(JOB_KEY, "9")]), true)
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    provider.revert_transaction().unwrap();
    provider.close();
    // R1: an explicitly typed serial differs from the reserved value. A
    // separate literal relationship key lets the child find its parent
    // without reading another computed field that would mask this bug.
    for scripted in [false, true] {
        let host = Host::new("held-effective-serial", vec![(ROOT, serial_root.clone())]);
        let mut provider = host.open(ROOT);
        provider.begin_transaction(Default::default()).unwrap();
        let child = provider
            .write_record(&om_data::ScriptWrite {
                layout: LAYOUT as u32,
                table: Some(NOTES),
                cells: [(JOB_KEY, Some("101".to_owned()))].into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let parent = if scripted {
            provider
                .write_record(&om_data::ScriptWrite {
                    layout: LAYOUT as u32,
                    cells: [
                        (SELF_KEY, Some("101".to_owned())),
                        (JOB_KEY, Some("99".to_owned())),
                    ]
                    .into(),
                    ..Default::default()
                })
                .unwrap()
                .id
        } else {
            provider
                .create_record(
                    LAYOUT as u32,
                    [
                        ("SelfKey".to_owned(), Some("101".to_owned())),
                        ("JobKey".to_owned(), Some("99".to_owned())),
                    ]
                    .into(),
                )
                .unwrap()
                .id
        };
        let serial_requests = host
            .captures
            .with_opcode(om_protocol::Opcode::HostGetSerial)
            .len();
        provider.commit_transaction(false).unwrap();
        assert_eq!(
            host.captures
                .with_opcode(om_protocol::Opcode::HostGetSerial)
                .len(),
            serial_requests,
            "dependency discovery must not reserve the serial again"
        );
        let parent_page = provider.records(LAYOUT as u32, 0, 100).unwrap();
        let at = parent_page
            .fields
            .iter()
            .position(|f| f.name == "JobKey")
            .unwrap();
        let stored = parent_page
            .records
            .iter()
            .find(|r| r.id == parent)
            .unwrap()
            .values[at]
            .clone();
        assert_eq!(stored.as_deref(), Some(if scripted { "99" } else { "7" }));
        let child_page = provider.records(NOTES_LAYOUT as u32, 0, 100).unwrap();
        let at = child_page
            .fields
            .iter()
            .position(|f| f.name == "Seen")
            .unwrap();
        assert_eq!(
            child_page
                .records
                .iter()
                .find(|r| r.id == child)
                .unwrap()
                .values[at],
            stored,
            "child and final parent must agree about the effective serial (scripted={scripted})"
        );
        provider.close();
    }
    // R1: an unchanged field's validation never reads through RandomKey.
    // With two held rows, another unknown calculation still exists, so
    // accepting the singleton alone cannot conceal an inactive-key demand.
    for records in [&[1][..], &[1, 2][..]] {
        let host = Host::new(
            "held-inactive-validation",
            vec![(ROOT, inactive_root.clone())],
        );
        let mut provider = host.open(ROOT);
        provider.begin_transaction(Default::default()).unwrap();
        for record in records {
            provider
                .update_record(
                    LAYOUT as u32,
                    *record,
                    [("Count".to_owned(), Some("8".to_owned()))].into(),
                )
                .unwrap();
        }
        provider.commit_transaction(false).unwrap();
        let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
        let at = page
            .fields
            .iter()
            .position(|f| f.name == "RandomKey")
            .unwrap();
        for record in records {
            let row = page.records.iter().find(|r| r.id == *record).unwrap();
            let value = row.values[at].as_deref().unwrap().parse::<f64>().unwrap();
            assert!((0.0..1.0).contains(&value), "{value}");
        }
        provider.close();
    }
}

/// A match field the write computes itself (#1035): Notes' `JobKey`
/// auto-enters `Code`, and `Seen` auto-enters `Tasks::Count` through `JobKey
/// = Tasks::JobKey`. The related records are read before the write is
/// computed, with `JobKey` empty; the provider reads them again with the
/// computed 7 and computes the note again, which then reads job 7's Count,
/// 21, as Commit Transaction does in a transaction.
#[test]
fn a_computed_match_key_reads_its_related_record() {
    use fixture::{LAYOUT, NOTES, NOTES_LAYOUT, NOTES_OCCURRENCE};
    const JOB_KEY: u64 = 30;
    const SEEN: u64 = 31;
    const CODE: u64 = 32;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_field(TABLE, JOB_KEY, "JobKey");
    for (id, name) in [(JOB_KEY, "JobKey"), (SEEN, "Seen"), (CODE, "Code")] {
        names.add_field(NOTES, id, name);
    }
    fixture::add_field(&mut root, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
    fixture::add_field(&mut root, NOTES, CODE, "Code", 1, 2, 0, 0, 0);
    fixture::add_field(&mut root, NOTES, JOB_KEY, "JobKey", 1, 2, 0, 0x8, 0);
    fixture::add_field(&mut root, NOTES, SEEN, "Seen", 1, 2, 0, 0x8, 0);
    fixture::add_relationship(
        &mut root,
        11,
        (NOTES_OCCURRENCE, JOB_KEY),
        0,
        (OCCURRENCE, JOB_KEY),
    );
    for (field, formula) in [(JOB_KEY, "Code"), (SEEN, "Tasks::Count")] {
        fixture::add_formula(
            &mut root,
            &names,
            (NOTES, NOTES_OCCURRENCE),
            field,
            5,
            formula,
        );
    }
    let host = Host::new("computed-key", vec![(ROOT, root)]);
    let mut provider = host.open(ROOT);
    provider
        .create_record(
            LAYOUT as u32,
            [
                ("JobKey".to_owned(), Some("7".to_owned())),
                ("Count".to_owned(), Some("21".to_owned())),
            ]
            .into(),
        )
        .unwrap();
    let note = |provider: &mut NativeProvider| {
        provider
            .write_record(&om_data::ScriptWrite {
                layout: LAYOUT as u32,
                table: Some(NOTES),
                cells: [(CODE, Some("7".to_owned()))].into(),
                ..Default::default()
            })
            .unwrap()
    };
    let outside = note(&mut provider);
    provider.begin_transaction(Default::default()).unwrap();
    let inside = note(&mut provider);
    provider.commit_transaction(false).unwrap();
    let page = provider.records(NOTES_LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    for id in [outside.id, inside.id] {
        let record = page.records.iter().find(|r| r.id == id).unwrap();
        assert_eq!(record.values[at("JobKey")].as_deref(), Some("7"));
        assert_eq!(record.values[at("Seen")].as_deref(), Some("21"));
    }
    provider.close();
}

/// An auto-entered calculation that passes a related field to an
/// aggregate reads every related record, not only the first (#1036).
/// Notes' `Seen` is `Max ( Tasks::Count ) & "/" & Count ( Tasks::Count ) &
/// "/" & Last ( Tasks::Count )` through Number JobKeys: for job 7 it reads
/// the two Tasks records of job 7, the last in creation order, and not
/// job 8's. In a transaction a Tasks record of job 7 held before the note
/// counts too. `LookupNext` over a related field, which falls back to the
/// host's index, keeps its table read-only, also wrapped (`Tasks::Count[1]`
/// is `GetRepetition ( Tasks::Count ; 1 )`).
#[test]
fn an_auto_entered_aggregate_reads_every_related_record() {
    use fixture::{LAYOUT, NOTES, NOTES_LAYOUT, NOTES_OCCURRENCE};
    const JOB_KEY: u64 = 30;
    const SEEN: u64 = 31;
    let root = |seen: &str| {
        let mut root = fixture::tree(&[], true);
        let mut names = fixture::names();
        names.add_field(TABLE, JOB_KEY, "JobKey");
        names.add_field(NOTES, JOB_KEY, "JobKey");
        names.add_field(NOTES, SEEN, "Seen");
        fixture::add_field(&mut root, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
        fixture::add_field(&mut root, NOTES, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
        fixture::add_field(&mut root, NOTES, SEEN, "Seen", 1, 1, 0, 0x8, 0);
        fixture::add_relationship(
            &mut root,
            11,
            (NOTES_OCCURRENCE, JOB_KEY),
            0,
            (OCCURRENCE, JOB_KEY),
        );
        fixture::add_formula(&mut root, &names, (NOTES, NOTES_OCCURRENCE), SEEN, 5, seen);
        root
    };
    let note = |provider: &mut NativeProvider, job: &str| {
        provider.write_record(&om_data::ScriptWrite {
            layout: LAYOUT as u32,
            table: Some(NOTES),
            cells: [(JOB_KEY, Some(job.to_owned()))].into(),
            ..Default::default()
        })
    };
    let task = |provider: &mut NativeProvider, job: &str, count: &str| {
        provider
            .create_record(
                LAYOUT as u32,
                [
                    ("JobKey".to_owned(), Some(job.to_owned())),
                    ("Count".to_owned(), Some(count.to_owned())),
                ]
                .into(),
            )
            .unwrap();
    };
    let seen = "Max ( Tasks::Count ) & \"/\" & Count ( Tasks::Count ) & \"/\" & \
                Last ( Tasks::Count )";
    let host = Host::new("aggregate", vec![(ROOT, root(seen))]);
    let mut provider = host.open(ROOT);
    task(&mut provider, "7", "10");
    task(&mut provider, "8", "99");
    task(&mut provider, "7", "4");
    note(&mut provider, "7").unwrap();
    provider.begin_transaction(Default::default()).unwrap();
    task(&mut provider, "7", "20");
    note(&mut provider, "7").unwrap();
    provider.commit_transaction(false).unwrap();
    let page = provider.records(NOTES_LAYOUT as u32, 0, 100).unwrap();
    let at = page.fields.iter().position(|f| f.name == "Seen").unwrap();
    let seen: Vec<_> = page
        .records
        .iter()
        .filter_map(|record| record.values[at].clone())
        .collect();
    assert_eq!(seen, ["10/2/4", "20/3/20"]);
    provider.close();

    let host = Host::new(
        "lookup-next",
        vec![(ROOT, root("LookupNext ( Tasks::Count[1] ; Lower )"))],
    );
    let mut provider = host.open(ROOT);
    let refused = note(&mut provider, "7").unwrap_err();
    assert!(refused.message.contains("LookupNext"), "{refused:?}");
    provider.close();
}

/// A related file's record written through Root's portal (#1000) reads
/// Root's committed record back through number match keys (#187 part B):
/// Related's `Seen` is auto-entered as `Back Tasks::Number` through
/// `Tasks::JobKey = Back Tasks::JobKey` (Number fields), and the row's
/// JobKey `7.0` matches Root's 7 as a number. Related's data source lists a
/// file the host does not have before Root: it still names Root. Set
/// Field's entry check of the row plans it the same way.
#[test]
fn a_routed_record_reads_the_committed_root_through_number_keys() {
    const JOB_KEY: u64 = 30;
    const NUMBER: u64 = 31;
    const SEEN: u64 = 32;
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference_with(
        &mut child,
        (BACK, "Back Tasks"),
        1,
        "Root",
        &[("file", "", "Nowhere"), ("file", "", "Root")],
    );
    for tree in [&mut root, &mut child] {
        fixture::add_field(tree, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
        fixture::add_field(tree, TABLE, NUMBER, "Number", 1, 2, 0, 0, 0);
    }
    fixture::add_field(&mut child, TABLE, SEEN, "Seen", 1, 2, 0, 0x8, 0);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, JOB_KEY), 0, (REMOTE, JOB_KEY));
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, JOB_KEY), 0, (BACK, JOB_KEY));
    let mut names = fixture::names();
    names.add_occurrence(BACK, "Back Tasks", TABLE);
    for (id, name) in [(JOB_KEY, "JobKey"), (NUMBER, "Number"), (SEEN, "Seen")] {
        names.add_field(TABLE, id, name);
    }
    let seen = "Back Tasks::Number";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), SEEN, 5, seen);
    put(&mut root, 1, JOB_KEY, "7");
    put(&mut root, 1, NUMBER, "42");
    let host = Host::new("routed-number-key", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    provider.check_entry(remote, JOB_KEY, "7.0").unwrap();
    let written = provider
        .write_record(&related_write(Some(remote), None, &[(JOB_KEY, "7.0")]))
        .unwrap();
    provider.close();
    let mut related = host.open(RELATED);
    let page = related.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let record = page.records.iter().find(|r| r.id == written.id).unwrap();
    assert_eq!(record.values[at("Seen")].as_deref(), Some("42"));
    related.close();
}

/// A portal row of another file's table carries the calling window's
/// `Get ( RecordNumber )` to the file storing it (#1038). Related's `Number`
/// auto-enters `Get ( RecordNumber )`, and a row the window numbers 3 stores
/// 3. A row written without a number is refused, naming the selector, rather
/// than numbered by a guess. An edit of the stored row, which does not
/// evaluate the creation-only formula, is written.
#[test]
fn a_routed_row_carries_its_windows_record_number() {
    const JOB_KEY: u64 = 30;
    const NUMBER: u64 = 31;
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    for tree in [&mut root, &mut child] {
        fixture::add_field(tree, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
    }
    fixture::add_field(&mut child, TABLE, NUMBER, "Number", 1, 2, 0, 0x8, 0);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, JOB_KEY), 0, (REMOTE, JOB_KEY));
    let mut names = fixture::names();
    names.add_field(TABLE, JOB_KEY, "JobKey");
    names.add_field(TABLE, NUMBER, "Number");
    let number = "Get ( RecordNumber )";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), NUMBER, 5, number);
    let host = Host::new("routed-record-number", vec![(ROOT, root), (RELATED, child)]);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let row = || related_write(Some(remote), None, &[(JOB_KEY, "7")]);
    let refused = provider.write_record(&row()).unwrap_err();
    assert!(refused.message.contains(number), "{refused:?}");
    let written = provider
        .write_record(&om_data::ScriptWrite {
            record_number: Some(3),
            ..row()
        })
        .unwrap();
    provider
        .write_record(&related_write(
            Some(remote),
            Some(written.id),
            &[(JOB_KEY, "8")],
        ))
        .unwrap();
    provider.close();
    let mut related = host.open(RELATED);
    let page = related.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let record = page.records.iter().find(|r| r.id == written.id).unwrap();
    assert_eq!(record.values[at("Number")].as_deref(), Some("3"));
    assert_eq!(record.values[at("JobKey")].as_deref(), Some("8"));
    related.close();
}

/// A portal row's values before the window's commit writes it (#1047):
/// Related's `Stamp` auto-enters `Get ( UUID )`, `Serial` a serial number
/// on creation (next 7), `Doubled2` `JobKey * 2`, `Number`
/// `Get ( RecordNumber )`, `Seen` the root parent's `Back Tasks::Count`
/// through `JobKey`, `SeenPlus` `Seen + 1` and `Draw` `Random`.
/// - The preview shows the UUID, 7 and 14. It leaves out `Number` while the
///   window gives no record number, `Seen` and `SeenPlus`, which another
///   record gives (the window may hold the parent unsaved), and `Draw`. It
///   writes nothing (no op71, op14, op70 or op103) and reserves the serial
///   once (op72).
/// - The commit's write naming it is refused without a record number,
///   before anything is sent. Previewed again over a changed cell, it keeps
///   its id, UUID and serial.
/// - With a record number, the write stores the previewed UUID, serial and
///   record number without reserving again, also after a later write of the
///   held row, and the parent's Count as the commit reads it.
/// - A used-up or discarded preview is refused; a reverted commit leaves the
///   preview for the next attempt. Root's own record written outside a
///   transaction stores its preview's UUID too.
#[test]
fn a_previewed_portal_row_stores_the_values_its_preview_showed() {
    use om_protocol::Opcode;
    const JOB_KEY: u64 = 30;
    const NUMBER: u64 = 31;
    const STAMP: u64 = 32;
    const SERIAL: u64 = 33;
    const DOUBLED: u64 = 34;
    const SEEN: u64 = 35;
    const SEEN_PLUS: u64 = 36;
    const DRAW: u64 = 37;
    const MINE: u64 = 38;
    const TWICE: u64 = 39;
    const TRIPLED: u64 = 40;
    const THRICE: u64 = 41;
    const MODIFIED: u64 = 42;
    const STAMPED: u64 = 43;
    const PEERS: u64 = 0xd0_000a;
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference_with(
        &mut child,
        (BACK, "Back Tasks"),
        1,
        "Root",
        &[("file", "", "Root")],
    );
    let mut names = fixture::names();
    names.add_occurrence(BACK, "Back Tasks", TABLE);
    names.add_occurrence(PEERS, "Peers", TABLE);
    for (id, name) in [
        (JOB_KEY, "JobKey"),
        (NUMBER, "Number"),
        (STAMP, "Stamp"),
        (SERIAL, "Serial"),
        (DOUBLED, "Doubled2"),
        (SEEN, "Seen"),
        (SEEN_PLUS, "SeenPlus"),
        (DRAW, "Draw"),
        (MINE, "Mine"),
        (TWICE, "Twice"),
        (TRIPLED, "Tripled"),
        (THRICE, "Thrice"),
        (MODIFIED, "Modified"),
        (STAMPED, "Stamped"),
    ] {
        names.add_field(TABLE, id, name);
    }
    for tree in [&mut root, &mut child] {
        fixture::add_field(tree, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
        fixture::add_field(tree, TABLE, STAMP, "Stamp", 1, 1, 0, 0x8, 0);
        fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), STAMP, 5, "Get ( UUID )");
    }
    fixture::add_field(&mut child, TABLE, NUMBER, "Number", 1, 2, 0, 0x8, 0);
    fixture::add_field(&mut child, TABLE, SERIAL, "Serial", 1, 2, 0, 0x2, 0);
    // Root's Modified auto-enters the modification timestamp.
    fixture::add_field(&mut root, TABLE, MODIFIED, "Modified", 1, 5, 7, 0x101, 0);
    // Root's Tripled auto-enters `Count * 3`, replacing.
    fixture::add_field(&mut root, TABLE, TRIPLED, "Tripled", 1, 2, 0, 0x88, 0);
    let tripled = "Count * 3";
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), TRIPLED, 5, tripled);
    fixture::add_field(&mut child, TABLE, DOUBLED, "Doubled2", 1, 2, 0, 0x8, 0);
    fixture::set_field_serial_increment(&mut child, TABLE, SERIAL, 1);
    child.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(SERIAL),
            &[0x0c],
        ],
        om_scripts::text_node(&[], "7"),
    );
    let number = "Get ( RecordNumber )";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), NUMBER, 5, number);
    let doubled = "JobKey * 2";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), DOUBLED, 5, doubled);
    for (field, name, formula) in [
        (SEEN, "Seen", "Back Tasks::Count"),
        (SEEN_PLUS, "SeenPlus", "Seen + 1"),
        (DRAW, "Draw", "Random"),
        (MINE, "Mine", "Peers::Count"),
        (TWICE, "Twice", "Back Tasks::Doubled"),
        (THRICE, "Thrice", "Back Tasks::Tripled"),
        (STAMPED, "Stamped", "Back Tasks::Modified"),
    ] {
        fixture::add_field(&mut child, TABLE, field, name, 1, 2, 0, 0x8, 0);
        fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), field, 5, formula);
    }
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, JOB_KEY), 0, (REMOTE, JOB_KEY));
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, JOB_KEY), 0, (BACK, JOB_KEY));
    // Related's own records by Name: its record 1, "Write tests", Count 37.
    fixture::add_occurrence(&mut child, PEERS, "Peers", TABLE);
    fixture::add_relationship(&mut child, 10, (OCCURRENCE, NAME), 0, (PEERS, NAME));
    put(&mut child, 1, COUNT, "37");
    for (record, key, count) in [(1, "7", "42"), (2, "8", "50")] {
        put(&mut root, record, JOB_KEY, key);
        put(&mut root, record, COUNT, count);
        put(&mut root, record, MODIFIED, "1/1/2020 1:00:00 AM");
    }
    let host = Host::new("previewed-row", vec![(ROOT, root), (RELATED, child)]);
    let written = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::TransactInfo,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    let reserved = || host.captures.with_opcode(Opcode::HostGetSerial).len();
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let row = || related_write(Some(remote), None, &[(JOB_KEY, "7")]);
    let named = |id, record_number| om_data::ScriptWrite {
        prepared: Some(id),
        record_number,
        ..row()
    };
    let stored = |host: &Host, file: &str, id: i64| {
        let mut reader = host.open(file);
        let page = reader.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
        let record = page.records.iter().find(|r| r.id == id).unwrap();
        let value = |name: &str| record.values[at(name)].clone().unwrap_or_default();
        let values = if file == RELATED {
            ["Stamp", "Serial", "Doubled2", "Number", "Seen", "SeenPlus"]
                .map(value)
                .to_vec()
        } else {
            vec![value("Stamp")]
        };
        reader.close();
        values
    };

    let (before, serials) = (written(), reserved());
    let preview = provider.preview_new_record(&row()).unwrap().unwrap();
    let uuid = preview.values[&STAMP].clone().unwrap();
    assert_eq!(uuid.len(), 36, "{uuid}");
    assert_eq!(preview.values[&SERIAL].as_deref(), Some("7"));
    assert_eq!(preview.values[&DOUBLED].as_deref(), Some("14"));
    for left_out in [NUMBER, SEEN, SEEN_PLUS, DRAW] {
        assert!(!preview.values.contains_key(&left_out), "{preview:?}");
    }
    assert!(
        !preview.values.contains_key(&JOB_KEY),
        "the write's own cell"
    );
    assert_eq!(written(), before, "a preview writes nothing");
    assert_eq!(reserved(), serials + 1, "its serial is reserved once");

    // The formula the preview left out refuses the write, before op71.
    let refused = provider.write_record(&named(preview.id, None)).unwrap_err();
    assert!(refused.message.contains(number), "{refused:?}");
    assert_eq!(written(), before);
    // Computed again over a changed cell (a trigger's, a vetoed commit's):
    // the same id, UUID and serial, nothing reserved again.
    let again = provider
        .preview_new_record(&om_data::ScriptWrite {
            cells: [(JOB_KEY, Some("9".to_owned()))].into(),
            ..named(preview.id, Some(3))
        })
        .unwrap()
        .unwrap();
    assert_eq!(again.id, preview.id);
    assert_eq!(again.values[&STAMP].as_deref(), Some(uuid.as_str()));
    assert_eq!(again.values[&SERIAL].as_deref(), Some("7"));
    assert_eq!(again.values[&DOUBLED].as_deref(), Some("18"));
    assert_eq!(again.values[&NUMBER].as_deref(), Some("3"));
    assert_eq!((written(), reserved()), (before, serials + 1));
    // A record of another table naming it is refused (3).
    let own = related_write(None, None, &[(NAME, "Own")]);
    let other = om_data::ScriptWrite {
        prepared: Some(preview.id),
        ..own.clone()
    };
    assert_eq!(provider.write_record(&other).unwrap_err().code, Some(3));

    // Reverted, then committed: the same UUID and serial, reserved once.
    // A later write of the held row, giving no record number, keeps the
    // preview's; its changed input is computed again (8 * 2).
    provider.begin_record_commit().unwrap();
    provider.write_record(&named(preview.id, Some(3))).unwrap();
    provider.revert_transaction().unwrap();
    provider.begin_record_commit().unwrap();
    let made = provider.write_record(&named(preview.id, Some(3))).unwrap();
    let edit = related_write(Some(remote), Some(made.id), &[(JOB_KEY, "8")]);
    provider.write_record(&edit).unwrap();
    provider.commit_transaction(false).unwrap();
    assert_eq!(reserved(), serials + 1, "no second reservation");
    assert_eq!(
        stored(&host, RELATED, made.id),
        [uuid.as_str(), "7", "16", "3", "50", "51"]
    );
    // Used up by the commit.
    let again = provider.write_record(&named(preview.id, Some(3)));
    assert_eq!(again.unwrap_err().code, Some(3));

    // Discarded: its serial stays used, and it names nothing.
    let discarded = provider.preview_new_record(&row()).unwrap().unwrap();
    assert_eq!(discarded.values[&SERIAL].as_deref(), Some("8"));
    provider.discard_prepared(discarded.id);
    let gone = provider.write_record(&named(discarded.id, Some(4)));
    assert_eq!(gone.unwrap_err().code, Some(3));
    let next = provider.preview_new_record(&row()).unwrap().unwrap();
    assert_eq!(next.values[&SERIAL].as_deref(), Some("9"));

    // Root's own record, written outside a transaction.
    let mine = provider.preview_new_record(&own).unwrap().unwrap();
    let mine_uuid = mine.values[&STAMP].clone().unwrap();
    assert_ne!(mine_uuid, uuid);
    let record = provider
        .write_record(&om_data::ScriptWrite {
            prepared: Some(mine.id),
            ..own
        })
        .unwrap();
    assert_eq!(stored(&host, ROOT, record.id), [mine_uuid]);

    // A preview numbered 3 whose commit no longer gives a number (a trigger
    // changed the row out of the measured shape, #1084): refused before op71,
    // not given the preview's 3.
    let before = written();
    let numbered = om_data::ScriptWrite {
        record_number: Some(3),
        ..row()
    };
    let numbered = provider.preview_new_record(&numbered).unwrap().unwrap();
    assert_eq!(numbered.values[&NUMBER].as_deref(), Some("3"));
    let refused = provider
        .write_record(&named(numbered.id, None))
        .unwrap_err();
    assert!(refused.message.contains(number), "{refused:?}");
    assert_eq!(written(), before);
    provider.discard_prepared(numbered.id);

    // Over the window's unsaved records (#1063): the parent's typed Count,
    // read through Root's session as its commit computes the parent (its
    // stored Doubled, `Count * 2`, is 198, not the stored 6); nothing is
    // written.
    let before = written();
    let parent = om_data::PendingRecord {
        table: TABLE,
        record: Some(1),
        cells: [(COUNT, Some("99".to_owned()))].into(),
        typed: [COUNT].into(),
    };
    let sibling = om_data::PendingRecord {
        table: remote,
        record: None,
        cells: [(JOB_KEY, Some("7".to_owned()))].into(),
        typed: [JOB_KEY].into(),
    };
    let over = provider
        .preview_new_record_over(&row(), std::slice::from_ref(&parent))
        .unwrap()
        .unwrap();
    assert_eq!(over.values[&SEEN].as_deref(), Some("99"), "{over:?}");
    assert_eq!(over.values[&SEEN_PLUS].as_deref(), Some("100"));
    assert_eq!(over.values[&TWICE].as_deref(), Some("198"), "{over:?}");
    assert_eq!(over.values[&THRICE].as_deref(), Some("297"), "{over:?}");
    // The parent's modification timestamp is its commit's: unknown now, so
    // only what reads it is left out.
    assert!(!over.values.contains_key(&STAMPED), "{over:?}");
    // Under a transaction skipping auto-enter options, the parent's commit
    // would keep its Tripled: so does the preview (its stored calculation
    // Doubled is still computed).
    let skipping = om_data::runtime::TransactionOptions {
        skip_auto_enter: true,
        ..Default::default()
    };
    provider.begin_transaction(skipping).unwrap();
    let skipped = provider
        .preview_new_record_over(&row(), std::slice::from_ref(&parent))
        .unwrap()
        .unwrap();
    assert_eq!(
        skipped.values[&TWICE].as_deref(),
        Some("198"),
        "{skipped:?}"
    );
    assert!(!skipped.values.contains_key(&THRICE), "{skipped:?}");
    provider.discard_prepared(skipped.id);
    provider.revert_transaction().unwrap();
    assert!(!over.values.contains_key(&DRAW), "{over:?}");
    // A row begun beside it, whose own Draw (`Random`) and record number
    // cannot be known before its commit: unknown, and not what this row
    // reads of the parent.
    let beside = provider
        .preview_new_record_over(&row(), &[parent.clone(), sibling])
        .unwrap()
        .unwrap();
    assert_eq!(beside.values[&SEEN].as_deref(), Some("99"), "{beside:?}");
    // Both files' base table is 0x8001: Root's record 1 Count 99 is Root's
    // alone, never Related's own record 1, which `Mine` reads (37).
    let named = related_write(Some(remote), None, &[(JOB_KEY, "7"), (NAME, "Write tests")]);
    let both = provider
        .preview_new_record_over(&named, std::slice::from_ref(&parent))
        .unwrap()
        .unwrap();
    assert_eq!(both.values[&SEEN].as_deref(), Some("99"), "{both:?}");
    assert_eq!(both.values[&MINE].as_deref(), Some("37"), "{both:?}");
    // Nothing else unsaved: the stored parent.
    let plain = provider
        .preview_new_record_over(&row(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(plain.values[&SEEN].as_deref(), Some("42"));
    // The unknown timestamp is job 7's parent's, record by record: a row of
    // job 8 reads its own parent's stored one, as with nothing unsaved.
    let eighth = related_write(Some(remote), None, &[(JOB_KEY, "8")]);
    let stored8 = provider
        .preview_new_record_over(&eighth, &[])
        .unwrap()
        .unwrap();
    let other = provider
        .preview_new_record_over(&eighth, std::slice::from_ref(&parent))
        .unwrap()
        .unwrap();
    assert_eq!(other.values[&SEEN].as_deref(), Some("50"), "{other:?}");
    assert!(stored8.values.contains_key(&STAMPED), "{stored8:?}");
    assert_eq!(other.values.get(&STAMPED), stored8.values.get(&STAMPED));
    // A parent the window has not created: nothing of another record.
    let new_parent = om_data::PendingRecord {
        record: None,
        ..parent
    };
    let unknown = provider
        .preview_new_record_over(&row(), &[new_parent])
        .unwrap()
        .unwrap();
    assert!(!unknown.values.contains_key(&SEEN), "{unknown:?}");
    // Root's transaction holds the parent's Count 99, the window an unsaved
    // Name: read over each other, as a commit of both would merge them.
    provider.begin_transaction(Default::default()).unwrap();
    provider
        .update_record(
            fixture::LAYOUT as u32,
            1,
            [("Count".to_owned(), Some("99".to_owned()))].into(),
        )
        .unwrap();
    let renamed = om_data::PendingRecord {
        table: TABLE,
        record: Some(1),
        cells: [(NAME, Some("Renamed".to_owned()))].into(),
        typed: [NAME].into(),
    };
    let held = provider
        .preview_new_record_over(&row(), &[renamed])
        .unwrap()
        .unwrap();
    assert_eq!(held.values[&SEEN].as_deref(), Some("99"), "{held:?}");
    assert_eq!(held.values[&TWICE].as_deref(), Some("198"), "{held:?}");
    // Held by the transaction alone, the window giving nothing: computed
    // as its commit computes it (its Doubled 198, not the stored 84), its
    // modification timestamp unknown.
    let alone = provider
        .preview_new_record_over(&row(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(alone.values[&TWICE].as_deref(), Some("198"), "{alone:?}");
    assert!(!alone.values.contains_key(&STAMPED), "{alone:?}");
    provider.revert_transaction().unwrap();
    for preview in [
        over, beside, both, plain, stored8, other, unknown, held, alone,
    ] {
        provider.discard_prepared(preview.id);
    }
    assert_eq!(written(), before);

    // A parent the transaction holds new (as a window's record session
    // does, #892), job 9 with Count 25, and the window's typed Count 26 over
    // it: computed as its commit would create it, its clock and UUID those
    // drawn when it was held.
    provider.begin_transaction(Default::default()).unwrap();
    let job = provider
        .create_record(
            fixture::LAYOUT as u32,
            [
                ("JobKey".to_owned(), Some("9".to_owned())),
                ("Count".to_owned(), Some("25".to_owned())),
            ]
            .into(),
        )
        .unwrap();
    let held_before = written();
    let new_job = om_data::PendingRecord {
        table: TABLE,
        record: Some(job.id),
        cells: [(COUNT, Some("26".to_owned()))].into(),
        typed: [COUNT].into(),
    };
    let ninth = related_write(Some(remote), None, &[(JOB_KEY, "9")]);
    let on_new = provider
        .preview_new_record_over(&ninth, &[new_job])
        .unwrap()
        .unwrap();
    assert_eq!(on_new.values[&SEEN].as_deref(), Some("26"), "{on_new:?}");
    assert_eq!(on_new.values[&TWICE].as_deref(), Some("52"), "{on_new:?}");
    assert_eq!(written(), held_before, "the preview writes nothing");
    provider.discard_prepared(on_new.id);
    // The window's own record before its commit (#1104): the held new job
    // with the window's typed Count 26, as its commit computes it.
    let own = |record, count: &str| related_write(None, Some(record), &[(COUNT, count)]);
    let held_new = provider.compute_pending(&own(job.id, "26"), &[]);
    let held_new = held_new.unwrap().unwrap();
    assert_eq!(held_new.values[&COUNT].as_deref(), Some("26"));
    assert_eq!(held_new.values[&fixture::DOUBLED].as_deref(), Some("52"));
    provider.revert_transaction().unwrap();
    // A stored record: Root's job 7 with the window's Count 99. Its stored
    // Doubled and auto-entered Tripled are its commit's (198, 297); its
    // modification timestamp is drawn by the commit: unknown. Nothing is
    // written or reserved.
    let (before, serials) = (written(), reserved());
    let edited = provider
        .compute_pending(&own(1, "99"), &[])
        .unwrap()
        .unwrap();
    assert_eq!(edited.values[&fixture::DOUBLED].as_deref(), Some("198"));
    assert_eq!(edited.values[&TRIPLED].as_deref(), Some("297"));
    assert_eq!(edited.values[&JOB_KEY].as_deref(), Some("7"));
    assert_eq!(edited.unknown, [MODIFIED].into(), "{edited:?}");
    // A value typed that does not read as its field's type: none.
    let due = related_write(None, Some(1), &[(DUE, "lots")]);
    let typed = provider.compute_pending(&due, &[]).unwrap();
    assert_eq!(typed, None);
    assert_eq!((written(), reserved()), (before, serials));
    provider.close();
}

/// A related-values list of an unstored calculation reading the opened
/// file, checked by the related file that writes the record (#1058, #1000):
/// Related's `Mark` must be one of `S::Shout`, related through `S`, a
/// self-join on `JobKey`, where `Shout` is `Back Tasks::Name &
/// Back Tasks::Setting`, the root record of the same `JobKey` and Root's
/// global, "!" in Root's session only. A row of Root's portal written there
/// reads Root's records and session globals through Root's session: job
/// 7's is "Write tests", job 8's "Ship". The written record is in its own
/// list with the values the write gives it (moved to job 8, its `Shout` is
/// "Ship!").
#[test]
fn a_related_files_list_of_a_calculation_reads_the_opened_file() {
    const JOB_KEY: u64 = 30;
    const SHOUT: u64 = 33;
    const MARK: u64 = 34;
    const MARK2: u64 = 35;
    const TAG: u64 = 36;
    const MODIFIED: u64 = 37;
    const STAMPED: u64 = 38;
    const MARK3: u64 = 39;
    const COPIED: u64 = 40;
    const CHECKED: u64 = 41;
    const SELF_JOIN: u64 = 0xd0_0009;
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    reference_with(
        &mut child,
        (BACK, "Back Tasks"),
        1,
        "Root",
        &[("file", "", "Root")],
    );
    for tree in [&mut root, &mut child] {
        fixture::add_field(tree, TABLE, JOB_KEY, "JobKey", 1, 2, 0, 0, 0);
    }
    let mut names = fixture::names();
    names.add_occurrence(BACK, "Back Tasks", TABLE);
    names.add_field(TABLE, JOB_KEY, "JobKey");
    names.add_field(TABLE, TAG, "Tag");
    names.add_field(TABLE, MODIFIED, "Modified");
    names.add_field(TABLE, CHECKED, "Checked");
    fixture::add_field(&mut child, TABLE, TAG, "Tag", 1, 1, 0, 0, 0);
    // Root's Modified auto-enters the modification timestamp; Related's
    // Stamped reads it, and Mark3 is validated against S::Stamped.
    fixture::add_field(&mut root, TABLE, MODIFIED, "Modified", 1, 5, 7, 0x101, 0);
    fixture::add_field(&mut child, TABLE, STAMPED, "Stamped", 2, 1, 0, 0x2_0000, 0);
    let stamped = "Back Tasks::Modified";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), STAMPED, 5, stamped);
    fixture::add_field(&mut child, TABLE, MARK3, "Mark3", 1, 1, 0, 0, 0x500);
    // Related's Copied auto-enters its job's Root timestamp, evaluated again
    // when its JobKey is set.
    fixture::add_field(&mut child, TABLE, COPIED, "Copied", 1, 1, 0, 0x88, 0);
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), COPIED, 5, stamped);
    // Related's Checked is validated always by a calculation reading it.
    fixture::add_field(&mut child, TABLE, CHECKED, "Checked", 1, 1, 0, 0, 0x401);
    let checked = "IsEmpty ( Back Tasks::Modified ) or not IsEmpty ( Back Tasks::Modified )";
    fixture::add_formula(
        &mut child,
        &names,
        (TABLE, OCCURRENCE),
        CHECKED,
        0x19,
        checked,
    );
    fixture::add_field(&mut child, TABLE, SHOUT, "Shout", 2, 1, 0, 0x2_0000, 0);
    let shout = "Tag & Back Tasks::Name & Back Tasks::Setting";
    fixture::add_formula(&mut child, &names, (TABLE, OCCURRENCE), SHOUT, 5, shout);
    fixture::add_field(&mut child, TABLE, MARK, "Mark", 1, 1, 0, 0, 0x500);
    // Validated only during data entry, against the same list.
    fixture::add_field(&mut child, TABLE, MARK2, "Mark2", 1, 1, 0, 0, 0x100);
    fixture::add_occurrence(&mut child, SELF_JOIN, "S", TABLE);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, JOB_KEY), 0, (REMOTE, JOB_KEY));
    fixture::add_relationship(&mut child, 9, (OCCURRENCE, JOB_KEY), 0, (BACK, JOB_KEY));
    fixture::add_relationship(
        &mut child,
        10,
        (OCCURRENCE, JOB_KEY),
        0,
        (SELF_JOIN, JOB_KEY),
    );
    // Value list 7, "Include only related values starting from" Tasks
    // (0x0001): S::Shout, no second field.
    child.put(
        &[&[0x21], &[5], &[7], &[0x10]],
        om_scripts::text_node(&[], "Shouts"),
    );
    let mut list = vec![
        1,
        0,
        1,
        4,
        3,
        0xd0,
        0,
        9,
        2,
        1,
        SHOUT as u8,
        0,
        1,
        0,
        0,
        0,
        0,
    ];
    list.extend([4, 3, 0xd0, 0, 1, 0, 0]);
    list.extend(om_scsu::encode_counted(" "));
    // Value list 8, the same over S::Stamped.
    let mut stamps = list.clone();
    stamps[10] = STAMPED as u8;
    child.put(
        &[&[0x21], &[5], &[7], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
    child.put(
        &[&[0x21], &[5], &[8], &[0x10]],
        om_scripts::text_node(&[], "Stamps"),
    );
    child.put(
        &[&[0x21], &[5], &[8], &[2]],
        OwnedNode::leaf(Vec::new(), stamps),
    );
    child.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(MARK3),
            &[0x07],
        ],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 8, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    child.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(MARK),
            &[0x07],
        ],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 7, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    child.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(MARK2),
            &[0x07],
        ],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 7, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    for (record, key) in [(1, "7"), (2, "8")] {
        put(&mut root, record, JOB_KEY, key);
        put(&mut child, record, JOB_KEY, key);
    }
    let host = Host::new(
        "routed-calculated-list",
        vec![(ROOT, root), (RELATED, child)],
    );
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    provider.set_global(TABLE, SETTING, 1, "!").unwrap();
    let mut write = |cells: &[(u64, &str)]| {
        provider
            .write_record(&related_write(Some(remote), Some(1), cells))
            .map(|_| ())
            .map_err(|e| (e.code, e.message))
    };
    assert_eq!(write(&[(MARK, "Write tests!")]), Ok(()));
    let refused = write(&[(MARK, "Ship!")]).unwrap_err();
    assert_eq!(refused.0, Some(506), "{refused:?}");
    assert_eq!(write(&[(JOB_KEY, "8"), (MARK, "Ship!")]), Ok(()));
    // Mark2, validated only during data entry (#1074): what the user typed
    // is checked by a user's rules at the commit, in a record commit too,
    // unless the commit skips data entry validation; a script's value is not.
    let typed = |text: &str| related_write(Some(remote), Some(1), &[(MARK2, text)]);
    let code = |result: Result<om_data::Record, om_data::DataError>| {
        result.map(|_| ()).map_err(|e| e.code)
    };
    assert_eq!(
        code(provider.write_record(&typed("Write tests!"))),
        Err(Some(506))
    );
    assert_eq!(code(provider.write_record(&typed("Ship!"))), Ok(()));
    let set_field = om_data::ScriptWrite {
        typed: Default::default(),
        ..typed("Write tests!")
    };
    assert_eq!(code(provider.write_record(&set_field)), Ok(()));
    provider.begin_record_commit().unwrap();
    provider.write_record(&typed("Nope!")).unwrap();
    let refused = provider.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(506), "{refused:?}");
    let skipping = om_data::ScriptWrite {
        skip_validation: true,
        ..typed("Nope!")
    };
    assert_eq!(code(provider.write_record(&skipping)), Ok(()));
    // The transaction's other records (#1063): `Shout` reads each related
    // record's own Tag with its parent's Name, as a list field over a row
    // and its parent does. Record 2, moved to job 7 and tagged "Bo", is read as
    // the commit computes it, when it computes it before record 1:
    // "BoWrite tests!" is then in record 1's list, as it is not in the
    // stored one. Computed after record 1, it is not known yet: refused.
    let moved = related_write(Some(remote), Some(2), &[(JOB_KEY, "7"), (TAG, "Bo")]);
    let marked = |text: &str| related_write(Some(remote), Some(1), &[(JOB_KEY, "7"), (MARK, text)]);
    let mut commit = |writes: &[&om_data::ScriptWrite]| {
        provider.begin_transaction(Default::default()).unwrap();
        for write in writes {
            provider.write_record(write).unwrap();
        }
        let result = provider.commit_transaction(false).map_err(|e| e.code);
        if result.is_err() {
            let _ = provider.revert_transaction();
        }
        result
    };
    assert_eq!(commit(&[&marked("BoWrite tests!"), &moved]), Err(Some(3)));
    assert_eq!(commit(&[&moved, &marked("Nope!")]), Err(Some(506)));
    assert_eq!(commit(&[&moved, &marked("BoWrite tests!")]), Ok(()));
    // Root's record in the same save (#1006): Related's commit comes first,
    // and its list reads Root's job 7 as Root's commit will write it.
    // Renamed "Ships", "BoShips!" is in record 1's list, as it is not over
    // the stored "Write tests". Root's modification timestamp is drawn by
    // its own commit, after Related's: a list reading it is refused (3),
    // and over the stored record it is only not listed (506).
    let mut save = |name: Option<&str>, row: i64, cells: &[(u64, &str)]| {
        provider.begin_record_commit().unwrap();
        if let Some(name) = name {
            let name = [("Name".to_owned(), Some(name.to_owned()))].into();
            provider
                .update_record(fixture::LAYOUT as u32, 1, name)
                .unwrap();
        }
        let row = related_write(Some(remote), Some(row), cells);
        provider.write_record(&row).unwrap();
        let result = provider.commit_transaction(false).map_err(|e| e.code);
        if result.is_err() {
            let _ = provider.revert_transaction();
        }
        result
    };
    assert_eq!(
        save(Some("Docks"), 1, &[(MARK, "BoShips!")]),
        Err(Some(506))
    );
    assert_eq!(save(Some("Ships"), 1, &[(MARK3, "x")]), Err(Some(3)));
    assert_eq!(save(None, 1, &[(MARK3, "x")]), Err(Some(506)));
    assert_eq!(save(Some("Ships"), 1, &[(MARK, "BoShips!")]), Ok(()));
    // Related's own rules: Copied, evaluated when record 1 moves, reads its
    // job's Root timestamp. Job 8's Root record is not in the save; job 7's,
    // renamed, gets its timestamp from its own commit (3).
    assert_eq!(save(Some("Harbor"), 1, &[(JOB_KEY, "8")]), Ok(()));
    assert_eq!(save(Some("Quay"), 1, &[(JOB_KEY, "7")]), Err(Some(3)));
    // A validation calculation reading it too: record 2 (job 7) is checked
    // over job 7's timestamp, its commit's while it is renamed (3).
    assert_eq!(save(Some("Pier"), 2, &[(CHECKED, "y")]), Err(Some(3)));
    assert_eq!(save(None, 2, &[(CHECKED, "y")]), Ok(()));
    // A Root record the save deletes is no record for Related's commit:
    // record 1 (job 8) reads no Root name, so "Ship!" is not listed (506).
    provider.begin_record_commit().unwrap();
    provider.delete_record(fixture::LAYOUT as u32, 2).unwrap();
    let ship = related_write(Some(remote), Some(1), &[(MARK, "Ship!")]);
    provider.write_record(&ship).unwrap();
    let deleted = provider.commit_transaction(false).map_err(|e| e.code);
    assert_eq!(deleted, Err(Some(506)));
    let _ = provider.revert_transaction();
    provider.close();
    let mut related = host.open(RELATED);
    let page = related.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let record = page.records.iter().find(|r| r.id == 1).unwrap();
    assert_eq!(record.values[at("Mark")].as_deref(), Some("BoShips!"));
    assert_eq!(record.values[at("JobKey")].as_deref(), Some("8"));
    let record = page.records.iter().find(|r| r.id == 2).unwrap();
    assert_eq!(record.values[at("Tag")].as_deref(), Some("Bo"));
    related.close();
}

/// A new record's auto-entry reading its own table through a self-join that
/// no other record matches, by the key the record was given, reads the
/// relationship as empty, as #1035's two native cases measured: Count 0,
/// List empty, the first related record's fields empty. Where another record
/// matches, or the commit holds another record of the table, the record's
/// own membership is unmeasured: refused (3).
#[test]
fn a_new_records_auto_entry_reads_an_unmatched_self_join_as_empty() {
    const KEY: u64 = 30;
    const TALLY: u64 = 31;
    const FIRST: u64 = 32;
    const SELF_JOIN: u64 = 0xd0_0009;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_occurrence(SELF_JOIN, "S", TABLE);
    for (id, name) in [(KEY, "Key"), (TALLY, "Tally"), (FIRST, "First")] {
        names.add_field(TABLE, id, name);
    }
    fixture::add_field(&mut root, TABLE, KEY, "Key", 1, 1, 0, 0, 0);
    fixture::add_occurrence(&mut root, SELF_JOIN, "S", TABLE);
    fixture::add_relationship(&mut root, 10, (OCCURRENCE, KEY), 0, (SELF_JOIN, KEY));
    // As the native Count/List case's witness and the first-record case's
    // (auto-entered calculations, 0xa8).
    let tally = "Count & \"|\" & Count ( S::Key ) & \"|\" & List ( S::Count ) & \"|\" & \
                 List ( S::Doubled )";
    let first = "\"s=\" & S::Name & \"|\" & S::Count";
    for (id, name, formula) in [(TALLY, "Tally", tally), (FIRST, "First", first)] {
        fixture::add_field(&mut root, TABLE, id, name, 1, 1, 0, 0xa8, 0);
        fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), id, 5, formula);
    }
    let host = Host::new("self-join-unmatched", vec![(ROOT, root)]);
    let mut provider = host.open(ROOT);
    fn create(
        provider: &mut impl DataProvider,
        key: &str,
    ) -> Result<om_data::Record, om_data::DataError> {
        let values = [
            ("Key".to_owned(), Some(key.to_owned())),
            ("Count".to_owned(), Some("5".to_owned())),
        ];
        provider.create_record(fixture::LAYOUT as u32, values.into())
    }
    let made = create(&mut provider, "x2").unwrap();
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let record = page.records.iter().find(|r| r.id == made.id).unwrap();
    assert_eq!(record.values[at("Tally")].as_deref(), Some("5|0||"));
    assert_eq!(record.values[at("First")].as_deref(), Some("s=|"));
    // Another record matches now: whether the new one is among its own
    // related records is unmeasured.
    let refused = create(&mut provider, "x2").unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    // Two new records in one commit: the other could match by then.
    provider.begin_transaction(Default::default()).unwrap();
    create(&mut provider, "y1").unwrap();
    create(&mut provider, "y2").unwrap();
    let refused = provider.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    provider.close();
    // A key computed on the related side (Code, auto-entered from Key):
    // the record's own membership is unmeasured, so refused.
    const CODE: u64 = 33;
    const BY_CODE: u64 = 0xd0_000a;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_occurrence(BY_CODE, "C", TABLE);
    for (id, name) in [(KEY, "Key"), (CODE, "Code2"), (TALLY, "Tally")] {
        names.add_field(TABLE, id, name);
    }
    fixture::add_field(&mut root, TABLE, KEY, "Key", 1, 1, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, CODE, "Code2", 1, 1, 0, 0x88, 0);
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), CODE, 5, "Key");
    fixture::add_occurrence(&mut root, BY_CODE, "C", TABLE);
    fixture::add_relationship(&mut root, 11, (OCCURRENCE, KEY), 0, (BY_CODE, CODE));
    fixture::add_field(&mut root, TABLE, TALLY, "Tally", 1, 1, 0, 0xa8, 0);
    let tally = "Count & \"|\" & Count ( C::Key )";
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), TALLY, 5, tally);
    let host = Host::new("self-join-computed", vec![(ROOT, root)]);
    let mut provider = host.open(ROOT);
    let refused = create(&mut provider, "z1").unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    provider.close();
}

/// A self-join whose key on the record's side the write computes empty
/// relates nothing, the record among none (#1035): an empty match value
/// never matches (file-metadata.md 4.3). A new group's first record reads
/// its `Lead` as the first related record's Name, empty, and `Tally` then
/// counts nothing through `Lead`, though `C`'s own key is auto-entered.
/// Computed non-empty, a later record reads the other records where its
/// final `Lead` differs from its own final `Code2`. Equal final keys read
/// one older match during new-row aggregate auto-entry (C126); with two
/// older matches the self-dependent answer remains unmeasured/refused (3).
#[test]
fn a_self_join_by_an_empty_computed_key_relates_nothing() {
    const KEY: u64 = 30;
    const LEAD: u64 = 31;
    const CODE: u64 = 32;
    const TALLY: u64 = 33;
    const BY_KEY: u64 = 0xd0_0009;
    const BY_LEAD: u64 = 0xd0_000a;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_occurrence(BY_KEY, "S", TABLE);
    names.add_occurrence(BY_LEAD, "C", TABLE);
    for (id, name) in [
        (KEY, "Key"),
        (LEAD, "Lead"),
        (CODE, "Code2"),
        (TALLY, "Tally"),
    ] {
        names.add_field(TABLE, id, name);
    }
    fixture::add_field(&mut root, TABLE, KEY, "Key", 1, 1, 0, 0, 0);
    fixture::add_occurrence(&mut root, BY_KEY, "S", TABLE);
    fixture::add_occurrence(&mut root, BY_LEAD, "C", TABLE);
    fixture::add_relationship(&mut root, 10, (OCCURRENCE, KEY), 0, (BY_KEY, KEY));
    fixture::add_relationship(&mut root, 11, (OCCURRENCE, LEAD), 0, (BY_LEAD, CODE));
    // Lead: the group's first record's Name; Code2: its own Key.
    for (id, name, formula, options) in [
        (LEAD, "Lead", "S::Name", 0x8),
        (CODE, "Code2", "Key", 0x88),
        (TALLY, "Tally", "\"t=\" & Count ( C::Key )", 0xa8),
    ] {
        fixture::add_field(&mut root, TABLE, id, name, 1, 1, 0, options, 0);
        fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), id, 5, formula);
    }
    let host = Host::new("self-join-empty-key", vec![(ROOT, root)]);
    let mut provider = host.open(ROOT);
    let mut create = |key: &str, name: &str| {
        let values = [
            ("Key".to_owned(), Some(key.to_owned())),
            ("Name".to_owned(), Some(name.to_owned())),
        ];
        provider.create_record(fixture::LAYOUT as u32, values.into())
    };
    let first = create("g1", "g1").unwrap();
    // Equal computed keys, one older match: Count sees that older row
    // (C126). More than one older match gains no new allowance.
    let matching = create("g1", "x").unwrap();
    let refused = create("g1", "z").unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    // Lead "x" differs from its own Code2 "h1": it cannot satisfy its own
    // predicate, so it reads the other records (none).
    create("h1", "x").unwrap();
    let later = create("h1", "y").unwrap();
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
    let value = |id: i64, name: &str| {
        let record = page.records.iter().find(|r| r.id == id).unwrap();
        record.values[at(name)].clone()
    };
    assert_eq!(value(first.id, "Lead"), None);
    assert_eq!(value(first.id, "Tally").as_deref(), Some("t=0"));
    assert_eq!(value(matching.id, "Lead").as_deref(), Some("g1"));
    assert_eq!(value(matching.id, "Tally").as_deref(), Some("t=1"));
    assert_eq!(value(later.id, "Lead").as_deref(), Some("x"));
    assert_eq!(value(later.id, "Tally").as_deref(), Some("t=0"));
    provider.close();
}

/// Root's files with a third: Related's Tasks has Entered, auto-entered by
/// `Third Tasks::Name` through Name = Name, where Third is a file Related
/// names and Root does not; Root has a layout of Related's Tasks (layout 2,
/// routed to Related, #1000). A record named "Write tests" created there
/// enters "Write tests" (#1160).
fn third_files() -> Vec<(&'static str, OwnedNode)> {
    third_files_at(&[("file", "", "Third")])
}

/// [`third_files`], Related's data source of Third naming `paths`.
fn third_files_at(paths: &[(&str, &str, &str)]) -> Vec<(&'static str, OwnedNode)> {
    const THIRD: u64 = 0xd0_0050;
    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    {
        let child = &mut files[1].1;
        reference_with(child, (THIRD, "Third Tasks"), 3, "Third", paths);
        fixture::add_relationship(child, 10, (OCCURRENCE, NAME), 0, (THIRD, NAME));
        fixture::add_field(child, TABLE, THIRD_ENTERED, "Entered", 1, 1, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_occurrence(THIRD, "Third Tasks", TABLE);
        names.add_field(TABLE, THIRD_ENTERED, "Entered");
        fixture::add_formula(
            child,
            &names,
            (TABLE, OCCURRENCE),
            THIRD_ENTERED,
            5,
            "Third Tasks::Name",
        );
    }
    files.push(("Third.fmp12", fixture::tree(&[], true)));
    files
}

/// [`third_files`]' Entered field id.
const THIRD_ENTERED: u64 = 21;

/// A record named "Write tests" created on Root's layout of Related's
/// Tasks: its Entered, or the create's error.
fn create_through_related(provider: &mut NativeProvider) -> Result<Option<String>, String> {
    use om_data::ScriptWrite;
    let layout = fixture::NOTES_LAYOUT as u32;
    let record = provider
        .write_record(&ScriptWrite {
            layout,
            table: None,
            record: None,
            cells: [(NAME, Some("Write tests".to_owned()))].into(),
            fields: Default::default(),
            typed: Default::default(),
            skip_validation: false,
            privilege_set: None,
            record_number: None,
            prepared: None,
        })
        .map_err(|e| e.message)?;
    let page = provider.records(layout, 0, 10).unwrap();
    let column = page
        .fields
        .iter()
        .position(|f| f.name == "Entered")
        .unwrap();
    Ok(page
        .records
        .iter()
        .find(|r| r.id == record.id)
        .and_then(|r| r.values[column].clone()))
}

/// A related file's own write, routed from the opened file, whose rule
/// reads a third file the opened file has open (#1160): the owner's plan
/// imports that file, and the rule reads it through its session. Before,
/// the plan left it unresolved and the write was refused ("a related
/// occurrence it reads is missing or of another file"), New Record not
/// offered.
#[test]
fn a_routed_writes_rule_reads_a_third_file_the_opened_file_has_open() {
    let host = Host::new("routed-third", third_files());
    let mut provider = host.open(ROOT);
    let capabilities = provider
        .record_capabilities(fixture::NOTES_LAYOUT as u32)
        .unwrap();
    assert!(
        capabilities.create && capabilities.update,
        "{capabilities:?}"
    );
    assert_eq!(
        create_through_related(&mut provider),
        Ok(Some("Write tests".to_owned()))
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Third.fmp12"), Some(&1), "{opens:?}");
    provider.close();
}

/// On demand, the third file a related file's own write reads is not open
/// when the layout is (#1160): the window's record capabilities offer New
/// Record without opening it, and the first routed write opens it, once,
/// and enters its value.
#[test]
fn a_routed_writes_rule_opens_the_third_file_it_reads_on_demand() {
    let host = Host::new("routed-third-demand", third_files());
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection(ROOT)).unwrap();
    let capabilities = provider
        .record_capabilities(fixture::NOTES_LAYOUT as u32)
        .unwrap();
    assert!(
        capabilities.create && capabilities.update,
        "{capabilities:?}"
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Third.fmp12"), None, "deferred: {opens:?}");
    assert_eq!(
        create_through_related(&mut provider),
        Ok(Some("Write tests".to_owned()))
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Third.fmp12"), Some(&1), "{opens:?}");
    provider.close();
}

/// A related file's data source of the third file with alternative paths
/// (a missing file first): the owner's plan binds it to the file the
/// opened file opened through the second path (#1160), as hydration does.
#[test]
fn a_routed_writes_rule_reads_a_third_file_named_by_a_later_path() {
    let host = Host::new(
        "routed-third-paths",
        third_files_at(&[("file", "", "Missing"), ("file", "", "Third")]),
    );
    let mut provider = host.open(ROOT);
    assert_eq!(
        create_through_related(&mut provider),
        Ok(Some("Write tests".to_owned()))
    );
    provider.close();
}

/// A related file's data source of the third file whose first path names
/// a file that refuses the account, the third file open all the same
/// through Root's own data source: the search stops at the refusing file,
/// as hydration's does, so the owner's plan leaves the occurrence
/// unresolved rather than bind it to the third file a later path names
/// (#1160), and the write reading it is refused.
#[test]
fn a_routed_writes_rule_does_not_read_a_third_file_past_a_refusing_path() {
    let mut files = third_files_at(&[("file", "", "Denied"), ("file", "", "Third")]);
    reference(&mut files[0].1, (ROOT_THIRD, "Root Third"), 4, "Third");
    let mut denied = fixture::tree(&[], true);
    // Its only account is another one.
    denied.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    files.push(("Denied.fmp12", denied));
    let host = Host::new("routed-third-refusing-path", files);
    let mut provider = host.open(ROOT);
    let refused = create_through_related(&mut provider).unwrap_err();
    assert!(refused.contains("Entered"), "{refused}");
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Third.fmp12"), Some(&1), "{opens:?}");
    provider.close();
}

/// Root's occurrence of the third file's Tasks
/// ([`a_routed_writes_rule_does_not_read_a_third_file_the_save_holds`]).
const ROOT_THIRD: u64 = 0xd0_0060;

/// A held third participant is read from its final preflight, in either
/// edit order (#1198). Third's stored Doubled must be recomputed from the
/// held Count; the raw overlay still contains its old value. Submission
/// stays in edit order, so preparing Third first is not enough without the
/// immutable snapshot when Related was edited first. A deletion hides its
/// stored row too, and the provisional create does not enter stale data.
#[test]
fn a_routed_writes_rule_reads_a_held_third_participants_final_rows() {
    let mut files = third_files();
    reference(&mut files[0].1, (ROOT_THIRD, "Root Third"), 4, "Third");
    fixture::add_relationship(
        &mut files[0].1,
        11,
        (OCCURRENCE, NAME),
        0,
        (ROOT_THIRD, NAME),
    );
    let mut names = fixture::names();
    names.add_occurrence(0xd0_0050, "Third Tasks", TABLE);
    names.add_field(TABLE, THIRD_ENTERED, "Entered");
    fixture::add_formula(
        &mut files[1].1,
        &names,
        (TABLE, OCCURRENCE),
        THIRD_ENTERED,
        5,
        "Third Tasks::Name & \":\" & Third Tasks::Doubled",
    );
    // The only outgoing Third dependency in this variant is through a
    // validation list's unstored value, reached through a local self-join.
    let mut listed = files.clone();
    let child = &mut listed[1].1;
    fixture::set_field_options(child, TABLE, THIRD_ENTERED, 0);
    const SHOUT: u64 = 32;
    const MARK: u64 = 34;
    const SELF_JOIN: u64 = 0xd0_0009;
    let mut list_names = names.clone();
    list_names.add_field(TABLE, SHOUT, "Shout");
    fixture::add_field(child, TABLE, SHOUT, "Shout", 2, 1, 0, 0x2_0000, 0);
    fixture::add_formula(
        child,
        &list_names,
        (TABLE, OCCURRENCE),
        SHOUT,
        5,
        "Third Tasks::Doubled",
    );
    fixture::add_field(child, TABLE, MARK, "Mark", 1, 1, 0, 0, 0x500);
    fixture::add_occurrence(child, SELF_JOIN, "S", TABLE);
    fixture::add_relationship(child, 12, (OCCURRENCE, NAME), 0, (SELF_JOIN, NAME));
    child.put(
        &[&[0x21], &[5], &[7], &[0x10]],
        om_scripts::text_node(&[], "Peer values"),
    );
    let mut list = vec![
        1,
        0,
        1,
        4,
        3,
        0xd0,
        0,
        9,
        2,
        1,
        SHOUT as u8,
        0,
        1,
        0,
        0,
        0,
        0,
    ];
    list.extend([4, 3, 0xd0, 0, 1, 0, 0]);
    list.extend(om_scsu::encode_counted(" "));
    child.put(&[&[0x21], &[5], &[7], &[2]], OwnedNode::leaf([], list));
    child.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(MARK), &[7]],
        OwnedNode::leaf([], vec![2, 1, 7, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff]),
    );
    // Mark starts valid for the stored Third row. The save changes it to
    // 8, which only Third's final Count=4 makes valid; related-list
    // validation is deferred from the held write to final preflight.
    put(child, 1, MARK, "6");
    let mut cyclic = files.clone();
    let third_file = &mut cyclic[2].1;
    reference(third_file, (REMOTE, "Peer Tasks"), 9, "Related");
    fixture::add_relationship(third_file, 12, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::add_field(third_file, TABLE, THIRD_ENTERED, "Mirror", 1, 1, 0, 0x88, 0);
    let mut peer_names = fixture::names();
    peer_names.add_occurrence(REMOTE, "Peer Tasks", TABLE);
    peer_names.add_field(TABLE, THIRD_ENTERED, "Mirror");
    fixture::add_formula(
        third_file,
        &peer_names,
        (TABLE, OCCURRENCE),
        THIRD_ENTERED,
        5,
        "Peer Tasks::Name",
    );
    let host = Host::new("routed-third-held", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let third = schema.table_of(ROOT_THIRD).unwrap().id;
    assert_ne!(third, schema.table_of(REMOTE).unwrap().id);
    let create = om_data::ScriptWrite {
        layout: fixture::NOTES_LAYOUT as u32,
        cells: [(NAME, Some("Write tests".to_owned()))].into(),
        ..Default::default()
    };
    let preview_write = om_data::ScriptWrite {
        layout: fixture::LAYOUT as u32,
        table: Some(schema.table_of(REMOTE).unwrap().id),
        ..create.clone()
    };
    let entered = |provider: &mut NativeProvider, id| {
        let page = provider
            .records(fixture::NOTES_LAYOUT as u32, 0, 100)
            .unwrap();
        let field = page
            .fields
            .iter()
            .position(|f| f.name == "Entered")
            .unwrap();
        page.records.iter().find(|r| r.id == id).unwrap().values[field].clone()
    };
    for (third_first, count, expected) in
        [(true, "4", "Write tests:8"), (false, "5", "Write tests:10")]
    {
        let sent = host
            .captures
            .with_opcode(om_protocol::Opcode::HostCommit)
            .len();
        let before = (commits_to(&host, RELATED), commits_to(&host, "Third.fmp12"));
        let change = related_write(Some(third), Some(1), &[(COUNT, count)]);
        provider.begin_record_commit().unwrap();
        if third_first {
            provider.write_record(&change).unwrap();
        }
        if third_first {
            let preview = provider
                .preview_new_record_over(&preview_write, &[])
                .unwrap()
                .unwrap();
            assert!(!preview.values.contains_key(&THIRD_ENTERED), "{preview:?}");
            provider.discard_prepared(preview.id);
        }
        let made = provider.write_record(&create).unwrap();
        if !third_first {
            provider.write_record(&change).unwrap();
        }
        let pending = om_data::PendingRecord {
            table: schema.table_of(REMOTE).unwrap().id,
            record: Some(made.id),
            ..Default::default()
        };
        let preview = provider
            .preview_new_record_over(&preview_write, &[pending])
            .unwrap()
            .unwrap();
        assert!(!preview.values.contains_key(&THIRD_ENTERED), "{preview:?}");
        provider.discard_prepared(preview.id);
        assert_eq!(
            entered(&mut provider, made.id),
            None,
            "provisional related auto-entry"
        );
        assert_eq!(
            (commits_to(&host, RELATED), commits_to(&host, "Third.fmp12")),
            before
        );
        provider.commit_record_session(false).unwrap();
        let commits = host.captures.with_opcode(om_protocol::Opcode::HostCommit);
        let submitted: Vec<_> = commits[sent..]
            .iter()
            .map(|(file, _)| file.as_str())
            .collect();
        assert_eq!(
            submitted,
            if third_first {
                vec!["Third.fmp12", RELATED]
            } else {
                vec![RELATED, "Third.fmp12"]
            }
        );
        assert_eq!(entered(&mut provider, made.id), Some(expected.to_owned()));
        assert_eq!(
            (commits_to(&host, RELATED), commits_to(&host, "Third.fmp12")),
            (before.0 + 1, before.1 + 1)
        );
        assert_eq!(
            rows_of(&host, "Third.fmp12")[0].3,
            if third_first { "8" } else { "10" }
        );
    }
    provider.begin_record_commit().unwrap();
    let made = provider.write_record(&create).unwrap();
    provider.delete_related_record(third, 1).unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(entered(&mut provider, made.id), Some(":".to_owned()));
    assert!(rows_of(&host, "Third.fmp12").iter().all(|row| row.0 != 1));
    provider.close();

    // A genuine peer cycle is still refused before any upload or commit,
    // keeping the session's held rows and reservations available to revert.
    let host = Host::new("routed-third-cycle", cyclic);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let third = schema.table_of(ROOT_THIRD).unwrap().id;
    provider.begin_record_commit().unwrap();
    let made = provider.write_record(&create).unwrap();
    provider
        .write_record(&related_write(Some(third), Some(1), &[(COUNT, "6")]))
        .unwrap();
    let sent = host.captures.transactions().len();
    let refused = provider.commit_record_session(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused}");
    assert!(refused.message.contains("cyclic"), "{refused}");
    let after = host.captures.transactions();
    assert!(after[sent..]
        .iter()
        .all(|(_, txn)| !matches!(txn.first(), Some(14 | 70))));
    assert_eq!(entered(&mut provider, made.id), None);
    provider.revert_transaction().unwrap();
    assert!(!rows_of(&host, RELATED).iter().any(|row| row.0 == made.id));
    assert_eq!(rows_of(&host, "Third.fmp12")[0].2, "3");
    // Edit then delete: historical nondeleted entries must not add the
    // deleted row's reverse dependency to the effective save.
    provider.begin_record_commit().unwrap();
    let made = provider.write_record(&create).unwrap();
    provider
        .write_record(&related_write(Some(third), Some(1), &[(COUNT, "6")]))
        .unwrap();
    provider.delete_related_record(third, 1).unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(entered(&mut provider, made.id), Some(":".to_owned()));
    provider.close();
    for third_first in [true, false] {
        let host = Host::new("routed-third-list", listed.clone());
        let mut provider = host.open(ROOT);
        let schema = provider.calc_schema().unwrap().unwrap();
        let third = schema.table_of(ROOT_THIRD).unwrap().id;
        let related = schema.table_of(REMOTE).unwrap().id;
        let peer = related_write(Some(third), Some(1), &[(COUNT, "4")]);
        provider.begin_record_commit().unwrap();
        if third_first {
            provider.write_record(&peer).unwrap();
        }
        provider
            .write_record(&related_write(
                Some(related),
                Some(1),
                &[(COUNT, "9"), (MARK, "8")],
            ))
            .unwrap();
        if !third_first {
            provider.write_record(&peer).unwrap();
        }
        provider.commit_record_session(false).unwrap();
        assert_eq!(rows_of(&host, RELATED)[0].2, "9");
        assert_eq!(rows_of(&host, "Third.fmp12")[0].3, "8");
        provider.close();
    }
}

/// Root's fields of a related file's table, one relationship away
/// (`routed_related_fields`), are offered for editing while a third file
/// that table's rules read waits to open on demand (#1160), as the
/// layout's own table's are (#1146): the file stays closed.
#[test]
fn a_routed_related_fields_capabilities_defer_a_third_file() {
    const THIRD: u64 = 0xd0_0050;
    let mut files = portal_files_with(|child| {
        reference(child, (THIRD, "Third Tasks"), 3, "Third");
        fixture::add_relationship(child, 10, (OCCURRENCE, NAME), 0, (THIRD, NAME));
        fixture::add_field(child, TABLE, THIRD_ENTERED, "Entered", 1, 1, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_occurrence(THIRD, "Third Tasks", TABLE);
        names.add_field(TABLE, THIRD_ENTERED, "Entered");
        fixture::add_formula(
            child,
            &names,
            (TABLE, OCCURRENCE),
            THIRD_ENTERED,
            5,
            "Third Tasks::Name",
        );
    });
    files.push(("Third.fmp12", fixture::tree(&[], true)));
    let host = Host::new("routed-portal-third", files);
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection(ROOT)).unwrap();
    let capabilities = provider
        .record_capabilities(fixture::LAYOUT as u32)
        .unwrap();
    let remote = capabilities.related.get(&REMOTE);
    assert!(
        remote.is_some_and(|fields| fields.contains(&NAME)),
        "{capabilities:?}"
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Third.fmp12"), None, "{opens:?}");
    provider.close();
}

/// A window's save across files reads the related records it changes as
/// their file's commit writes them (#1167). The opened file's commit comes
/// after its related files' (#1006) and computed from their stored
/// records, so it committed a value from before the save. Root's Tasks has
/// Seen, auto-entered as `Remote Tasks::Name` through Count = Count, and
/// Copied, looked up from `Named Tasks::Doubled` (Related's stored
/// calculation, twice Count) through Name = Name. Related's record 1 is
/// "Write tests", Count 3, in Root's record session each time:
/// - Renamed through the portal, then a Root record with Count 3 created:
///   Seen reads the new name (Commit Records).
/// - A Root record named as record 1 created, then record 1's Count set to
///   4 through the portal: Copied reads 8, as Related's commit computed it
///   (the window's record session commit).
/// - Record 1 deleted through the portal and a Root record created that
///   both relate to it: nothing is read of it.
#[test]
fn a_save_across_files_reads_the_related_records_it_changes() {
    use om_data::field_options::{Lookup, NoMatch};
    const SEEN: u64 = 21;
    const COPIED: u64 = 22;
    const NAMED: u64 = 0xd0_0045;
    let mut files = portal_files();
    {
        let root = &mut files[0].1;
        reference(root, (NAMED, "Named Tasks"), 3, "Related");
        fixture::add_relationship(root, 11, (OCCURRENCE, NAME), 0, (NAMED, NAME));
        fixture::add_field(root, TABLE, SEEN, "Seen", 1, 1, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
        names.add_field(TABLE, SEEN, "Seen");
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            SEEN,
            5,
            "Remote Tasks::Name",
        );
        fixture::add_field(root, TABLE, COPIED, "Copied", 1, 1, 0, 0x400, 0);
        let lookup = Lookup {
            from: OCCURRENCE,
            to: NAMED,
            copied: fixture::DOUBLED,
            copy_empty: true,
            no_match: NoMatch::DoNotCopy,
        };
        root.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(COPIED),
                &[8],
            ],
            OwnedNode::leaf([], lookup.encode().unwrap()),
        );
    }
    let host = Host::new("save-reads-related-changes", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let change = |field: u64, value: &str| related_write(Some(remote), Some(1), &[(field, value)]);
    let create =
        |name: &str, count: &str| related_write(None, None, &[(NAME, name), (COUNT, count)]);
    let read = |provider: &mut NativeProvider, id: i64| {
        let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let at = |name: &str| page.fields.iter().position(|f| f.name == name).unwrap();
        let record = page.records.iter().find(|r| r.id == id).unwrap();
        (
            record.values[at("Seen")].clone(),
            record.values[at("Copied")].clone(),
        )
    };
    let text = |value: &str| Some(value.to_owned());

    provider.begin_record_commit().unwrap();
    provider.write_record(&change(NAME, "Renamed")).unwrap();
    let first = provider.write_record(&create("First", "3")).unwrap();
    provider.commit_transaction(false).unwrap();
    assert_eq!(read(&mut provider, first.id), (text("Renamed"), None));

    provider.begin_record_commit().unwrap();
    let second = provider.write_record(&create("Renamed", "9")).unwrap();
    provider.write_record(&change(COUNT, "4")).unwrap();
    provider.commit_record_session(false).unwrap();
    assert_eq!(read(&mut provider, second.id), (None, text("8")));

    provider.begin_record_commit().unwrap();
    provider.delete_related_record(remote, 1).unwrap();
    let third = provider.write_record(&create("Renamed", "4")).unwrap();
    provider.commit_transaction(false).unwrap();
    assert_eq!(read(&mut provider, third.id), (None, None));
    assert!(rows_of(&host, RELATED).iter().all(|row| row.0 != 1));
    provider.close();
}

/// The two files' rules reading each other's records in one save (#1167).
/// Root's new record's Seen reads Related's record 1, which the save
/// renames, and record 1's Mirror, auto-entered over its Name, reads Seen
/// back. Related's commit comes first, and it cannot know Seen as Root's
/// commit will write it from Related's own changes. So the save is refused
/// (3) before any record data, rather than Related storing a Seen from
/// before the save next to Root's new one.
#[test]
fn a_save_across_files_refuses_rules_reading_each_others_changes() {
    const SEEN: u64 = 21;
    const MIRROR: u64 = 23;
    let mut files = portal_files_with(|related| {
        reference(related, (BACK, "Back Tasks"), 0x8081, ROOT);
        fixture::add_relationship(related, 9, (OCCURRENCE, COUNT), 0, (BACK, COUNT));
        fixture::add_field(related, TABLE, MIRROR, "Mirror", 1, 1, 0, 0x88, 0);
        let mut names = fixture::names();
        names.add_occurrence(BACK, "Back Tasks", TABLE);
        names.add_field(TABLE, SEEN, "Seen");
        names.add_field(TABLE, MIRROR, "Mirror");
        fixture::add_formula(
            related,
            &names,
            (TABLE, OCCURRENCE),
            MIRROR,
            5,
            "Name & \"|\" & Back Tasks::Seen",
        );
    });
    {
        let root = &mut files[0].1;
        fixture::add_field(root, TABLE, SEEN, "Seen", 1, 1, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
        names.add_field(TABLE, SEEN, "Seen");
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            SEEN,
            5,
            "Remote Tasks::Name",
        );
    }
    let host = Host::new("save-rules-read-each-other", files);
    let mut provider = host.open(ROOT);
    let schema = provider.calc_schema().unwrap().unwrap();
    let remote = schema.table_of(REMOTE).unwrap().id;
    let sent = (commits_to(&host, ROOT), commits_to(&host, RELATED));
    provider.begin_record_commit().unwrap();
    provider
        .write_record(&related_write(
            Some(remote),
            Some(1),
            &[(NAME, "Renamed"), (COUNT, "7")],
        ))
        .unwrap();
    provider
        .write_record(&related_write(
            None,
            None,
            &[(NAME, "New root"), (COUNT, "7")],
        ))
        .unwrap();
    let refused = provider.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused}");
    assert_eq!((commits_to(&host, ROOT), commits_to(&host, RELATED)), sent);
    provider.close();
}

/// The first find must carry table demand through alternative-path resolution,
/// before Browse or a prior occurrence read can repair a missing field catalog.
#[test]
fn first_related_find_loads_the_catalog_selected_by_alternative_paths() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    for (fallback, already_open) in [(false, false), (true, false), (false, true)] {
        let mut root = fixture::tree(&[], true);
        let paths = if fallback {
            vec![("file", "", "Missing"), ("file", "", "Related")]
        } else {
            vec![("file", "", "Related"), ("file", "", "Unused")]
        };
        reference_with(&mut root, (REMOTE, "Remote Tasks"), 1, "Related", &paths);
        const REMOTE_NOTES: u64 = REMOTE + 1;
        if already_open {
            reference(&mut root, (REMOTE_NOTES, "Remote Notes"), 2, "Related");
            root.put(
                &[&[3], &[17], &[5], &uint_segment(REMOTE_NOTES), &[2]],
                OwnedNode::leaf([], [related::key(2), related::key(fixture::NOTES)].concat()),
            );
        }
        root.put(
            &[&[4], &[5], &[2], &[2]],
            OwnedNode::leaf([], layout_def(REMOTE)),
        );
        let mut peer = fixture::tree(&[], true);
        put(&mut peer, 1, NAME, "Other");
        put(&mut peer, 2, NAME, "Write tests");
        let host = Host::new("first-find-paths", vec![(ROOT, root), (RELATED, peer)]);
        let mut provider = host.provider().with_related_on_demand(true);
        provider.open(&host.connection(ROOT)).unwrap();
        assert!(!host.captures.opens_and_logins().0.contains_key(RELATED));
        let previous_table = already_open.then(|| {
            provider
                .calc_data(&[CalcNeed::Occurrence {
                    occurrence: REMOTE_NOTES,
                }])
                .unwrap();
            let schema = provider.calc_schema().unwrap().unwrap();
            assert!(schema.is_pending(REMOTE), "Tasks fields remain unread");
            schema.table_of(REMOTE_NOTES).unwrap().id
        });
        let related_reads = || {
            host.captures
                .transactions()
                .iter()
                .filter(|(file, _)| file == RELATED)
                .count()
        };
        let before = related_reads();
        provider
            .record_capabilities(fixture::LAYOUT as u32)
            .unwrap();
        assert_eq!(
            related_reads(),
            before,
            "passive capabilities perform no related IO"
        );
        let mut request = FindRequest::default();
        request.set("Name", "==Write tests");
        let result = provider
            .change_found_set(
                2,
                FoundSetChange::Find {
                    requests: vec![request],
                    kind: FindKind::Replace,
                },
            )
            .unwrap_or_else(|e| panic!("first find (fallback={fallback}): {e:?}"));
        assert_eq!((result.found, result.total), (1, 2));
        let page = provider.records(2, 0, 10).unwrap();
        assert_eq!(page.records.iter().map(|r| r.id).collect::<Vec<_>>(), [2]);
        let schema = provider.calc_schema().unwrap().unwrap();
        let table = schema.table_of(REMOTE).unwrap().id;
        assert!(!schema.is_pending(REMOTE));
        if let Some(previous) = previous_table {
            assert_eq!(
                schema.table_of(REMOTE_NOTES).unwrap().id,
                previous,
                "catalog growth preserves the existing table ID"
            );
        }
        let catalogs = || {
            host.captures
                .transactions()
                .iter()
                .filter(|(file, t)| {
                    file == RELATED
                        && t.first().copied() == Some(om_protocol::Opcode::DownloadWithLock as u8)
                        && om_protocol::parse_download_request(t).unwrap().key_path
                            == vec![uint_segment(TABLE), vec![3]]
                })
                .count()
        };
        assert_eq!(catalogs(), 1, "the selected table catalog is read once");
        provider
            .calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
            .unwrap();
        assert_eq!(
            provider
                .calc_schema()
                .unwrap()
                .unwrap()
                .table_of(REMOTE)
                .unwrap()
                .id,
            table
        );
        assert_eq!(catalogs(), 1);
        assert_eq!(host.captures.opens_and_logins().0.get(RELATED), Some(&1));
        provider.close();
    }
}

/// The tables of `file` whose field catalog `[T, 3]` was downloaded, in
/// order, one per download (#772: read only when needed).
fn field_catalog_reads(host: &Host, file: &str) -> Vec<u64> {
    host.captures
        .transactions()
        .into_iter()
        .filter(|(name, _)| name == file)
        .filter_map(|(_, transaction)| {
            let path = match transaction.first().copied() {
                Some(op) if op == om_protocol::Opcode::DownloadWithLock as u8 => {
                    om_protocol::parse_download_request(&transaction)
                        .ok()?
                        .key_path
                }
                Some(op) if op == om_protocol::Opcode::DownloadList as u8 => {
                    om_protocol::parse_download_list_request(&transaction)
                        .ok()?
                        .key_path
                }
                _ => return None,
            };
            match path.as_slice() {
                [table, catalog] if catalog.as_slice() == [3] => om_hbam::slot::be_uint(table),
                _ => None,
            }
        })
        .collect()
}

/// Root's layout of Related's Tasks (#1000), whose Entered is auto-entered
/// from Related's own Notes (`Notes::Body` through Name = Task) and whose
/// Notes record 1 is the note of "Write tests".
fn own_table_rule_files() -> Vec<(&'static str, OwnedNode)> {
    let mut files = files();
    files[0].1.put(
        &[&[4], &[5], &[2], &[2]],
        OwnedNode::leaf([], layout_def(REMOTE)),
    );
    let child = &mut files[1].1;
    fixture::add_field(child, fixture::NOTES, fixture::TASK, "Task", 1, 1, 0, 0, 0);
    fixture::add_relationship(
        child,
        1,
        (OCCURRENCE, NAME),
        0,
        (fixture::NOTES_OCCURRENCE, fixture::TASK),
    );
    for (field, text) in [(fixture::BODY, "noted"), (fixture::TASK, "Write tests")] {
        child.put(
            &[
                &uint_segment(fixture::NOTES),
                &[5],
                &serial::encode(1),
                &uint_segment(field),
            ],
            om_scripts::text_node(&[], text),
        );
    }
    fixture::add_field(child, TABLE, THIRD_ENTERED, "Entered", 1, 1, 0, 0x8, 0);
    let mut names = fixture::names();
    names.add_field(fixture::NOTES, fixture::TASK, "Task");
    names.add_field(TABLE, THIRD_ENTERED, "Entered");
    fixture::add_formula(
        child,
        &names,
        (TABLE, OCCURRENCE),
        THIRD_ENTERED,
        5,
        "Notes::Body",
    );
    files
}

/// Stage C of #772: a related file's own write reads the field catalog of
/// another table of its file that its rules read, when that catalog is read
/// only on demand. The window's record capabilities read none of it; the
/// routed write reads it once, then plans, and enters the note.
#[test]
fn a_routed_writes_rule_reads_its_files_other_table_first() {
    for nested in [false, true] {
        let mut files = own_table_rule_files();
        let expected = if nested {
            // Notes' stored Body is itself a calculation whose dependency
            // is only exposed when Notes' catalog is read. Demand must
            // continue through that new definition before planning.
            const THIRD: u64 = 0xd0_0050;
            let child = &mut files[1].1;
            reference(child, (THIRD, "Third Tasks"), 3, "Third");
            fixture::add_relationship(
                child,
                10,
                (fixture::NOTES_OCCURRENCE, fixture::TASK),
                0,
                (THIRD, NAME),
            );
            fixture::add_field(
                child,
                fixture::NOTES,
                fixture::BODY,
                "Body",
                2,
                1,
                0,
                0x20,
                0,
            );
            let mut names = fixture::names();
            names.add_occurrence(THIRD, "Third Tasks", TABLE);
            fixture::add_formula(
                child,
                &names,
                (fixture::NOTES, fixture::NOTES_OCCURRENCE),
                fixture::BODY,
                5,
                "Third Tasks::Name",
            );
            child.put(
                &[
                    &uint_segment(fixture::NOTES),
                    &[5],
                    &serial::encode(1),
                    &uint_segment(fixture::BODY),
                ],
                om_scripts::text_node(&[], "Write tests"),
            );
            files.push(("Third.fmp12", fixture::tree(&[], true)));
            "Write tests"
        } else {
            "noted"
        };
        let host = Host::new("routed-own-table", files);
        let mut provider = host.provider().with_related_on_demand(true);
        provider.open(&host.connection(ROOT)).unwrap();
        let capabilities = provider
            .record_capabilities(fixture::NOTES_LAYOUT as u32)
            .unwrap();
        assert!(capabilities.create, "{capabilities:?}");
        assert!(
            !field_catalog_reads(&host, RELATED).contains(&fixture::NOTES),
            "capabilities read no write-only catalog: {:?}",
            field_catalog_reads(&host, RELATED)
        );
        assert!(field_catalog_reads(&host, "Third.fmp12").is_empty());
        assert_eq!(
            create_through_related(&mut provider),
            Ok(Some(expected.to_owned()))
        );
        let notes = field_catalog_reads(&host, RELATED)
            .into_iter()
            .filter(|t| *t == fixture::NOTES)
            .count();
        assert_eq!(notes, 1, "{:?}", field_catalog_reads(&host, RELATED));
        if nested {
            assert_eq!(field_catalog_reads(&host, "Third.fmp12"), [TABLE]);
        }
        provider.close();
    }
}

/// Stage C of #772: either operation on Root's layout of Related's Tasks,
/// before anything shows that layout, reads Tasks' fields first rather
/// than reading its criterion or sort key as a missing field.
#[test]
fn a_find_or_sort_on_a_related_files_table_reads_its_fields_first() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange, SortField, SortOrder};
    let mut request = FindRequest::default();
    request.set("Name", "Write tests");
    for (change, expected, sorted) in [
        (
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Replace,
            },
            vec![1],
            false,
        ),
        (
            FoundSetChange::Sort(vec![SortField::new("Name", SortOrder::Descending)]),
            vec![1, 2],
            true,
        ),
    ] {
        let host = Host::new("related-find-sort-fields", own_table_rule_files());
        let mut provider = host.provider().with_related_on_demand(true);
        provider.open(&host.connection(ROOT)).unwrap();
        let found = provider
            .change_found_set(fixture::NOTES_LAYOUT as u32, change)
            .unwrap();
        assert_eq!(found.found, expected.len() as u64, "{found:?}");
        assert_eq!(found.sorted, sorted, "{found:?}");
        assert_eq!(imported_ids(&mut provider), expected);
        assert_eq!(
            field_catalog_reads(&host, RELATED)
                .iter()
                .filter(|t| **t == TABLE)
                .count(),
            1,
            "{:?}",
            field_catalog_reads(&host, RELATED)
        );
        assert!(
            !field_catalog_reads(&host, RELATED).contains(&fixture::NOTES),
            "find and sort read no write-only catalog"
        );
        provider.close();
    }
}

/// A data request naming only a table must read its pending field catalog
/// on the already-open owner before downloading the record. Re-importing
/// the schema must keep the table and occurrence ids the caller already has.
#[test]
fn a_record_need_reads_an_open_files_pending_table_first() {
    let host = Host::new("related-record-fields", own_table_rule_files());
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection(ROOT)).unwrap();
    provider
        .record_capabilities(fixture::NOTES_LAYOUT as u32)
        .unwrap();
    let before = provider.calc_schema().unwrap().unwrap();
    let table = before
        .tables()
        .find(|table| {
            before.table_context(table.id).is_some_and(|file| {
                file.file_name == "Related" && file.file_id(table.id) == fixture::NOTES
            })
        })
        .unwrap()
        .id;
    let occurrence = before.occurrences().find(|o| o.table == table).unwrap().id;
    assert!(before.are_fields_pending(table));
    assert!(!field_catalog_reads(&host, RELATED).contains(&fixture::NOTES));
    let data = provider
        .calc_data(&[CalcNeed::Record { table, record: 1 }])
        .unwrap();
    assert!(data.records[&(table, 1)].is_some());
    let after = provider.calc_schema().unwrap().unwrap();
    assert!(!after.are_fields_pending(table));
    assert_eq!(after.occurrence(occurrence).unwrap().table, table);
    let body = after
        .table(table)
        .unwrap()
        .fields
        .values()
        .find(|f| f.name == "Body")
        .unwrap()
        .id;
    assert!(matches!(
        data.records[&(table, 1)].as_ref().unwrap().get(body, 1),
        Some(om_data::calc::Value::Text(text)) if text.as_str() == "noted"
    ));
    assert_eq!(
        field_catalog_reads(&host, RELATED)
            .iter()
            .filter(|t| **t == fixture::NOTES)
            .count(),
        1
    );
    provider.close();
}

/// A stored calculation on each side of a lookup joins by the final
/// source key, including creation, edits, same-value input writes and
/// clearing an input. A general related read uses the same final key,
/// then its consumer runs; stored target auto-entry is not re-evaluated.
#[test]
fn a_lookup_uses_its_stored_calculation_match_keys() {
    use om_data::field_options::{Lookup, NoMatch};
    const KEY: u64 = 30;
    const SEEN: u64 = 20;
    const CONSUMER: u64 = 19;
    const COPY: u64 = 21;
    const AFTER: u64 = 22;
    let make = || {
        let mut files = files();
        for (index, (_, tree)) in files.iter_mut().enumerate() {
            fixture::add_field(tree, TABLE, KEY, "Key", 2, 1, 0, 0xa000_0000, 0);
            let mut names = fixture::names();
            names.add_field(TABLE, KEY, "Key");
            names.add_field(TABLE, COPY, "Copied");
            names.add_field(TABLE, AFTER, "After");
            names.add_field(TABLE, SEEN, "Seen");
            names.add_field(TABLE, CONSUMER, "Consumer");
            names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
            fixture::add_formula(
                tree,
                &names,
                (TABLE, OCCURRENCE),
                KEY,
                5,
                if index == 0 {
                    "\"K\" & Name"
                } else {
                    "\"K\" & Count"
                },
            );
            if index == 0 {
                // Force this reader to follow even an empty final key;
                // only the match calculations skip all-empty inputs.
                fixture::add_field(tree, TABLE, SEEN, "Seen", 1, 2, 0, 0xa8, 0);
                fixture::add_formula(
                    tree,
                    &names,
                    (TABLE, OCCURRENCE),
                    SEEN,
                    5,
                    "Remote Tasks::Count",
                );
                fixture::add_field(tree, TABLE, CONSUMER, "Consumer", 2, 2, 0, 0x20, 0);
                fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), CONSUMER, 5, "Seen + 1");
                fixture::add_relationship(tree, 9, (OCCURRENCE, KEY), 0, (REMOTE, KEY));
                fixture::add_field(tree, TABLE, COPY, "Copied", 1, 1, 0, 0x400, 0);
                fixture::add_field(tree, TABLE, AFTER, "After", 2, 1, 0, 0x20, 0);
                fixture::add_formula(
                    tree,
                    &names,
                    (TABLE, OCCURRENCE),
                    AFTER,
                    5,
                    "Copied & \"/\"",
                );
                let lookup = Lookup {
                    from: OCCURRENCE,
                    to: REMOTE,
                    copied: NAME,
                    copy_empty: true,
                    no_match: NoMatch::DoNotCopy,
                };
                tree.put(
                    &[&uint_segment(TABLE), &[3], &[5], &uint_segment(COPY), &[8]],
                    OwnedNode::leaf([], lookup.encode().unwrap()),
                );
            } else {
                // Reading a stored auto-entered value must not evaluate its
                // formula. The stored numbers below deliberately differ.
                fixture::add_field(tree, TABLE, COUNT, "Count", 1, 2, 0, 0x8, 0);
                fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), COUNT, 5, "99");
                for (record, input, payload) in [(1, "1", "first"), (2, "2", "second")] {
                    put(tree, record, COUNT, input);
                    put(tree, record, KEY, &format!("K{input}"));
                    put(tree, record, NAME, payload);
                }
            }
        }
        files
    };
    let host = Host::new("calculated-lookup-key", make());
    let mut provider = host.open(ROOT);
    let created = provider
        .write_record(&related_write(None, None, &[]))
        .unwrap();
    let read = |provider: &mut NativeProvider| {
        let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
        let row = page.records.iter().find(|r| r.id == created.id).unwrap();
        ["Key", "Copied", "After", "Seen", "Consumer"].map(|name| {
            row.values[page.fields.iter().position(|f| f.name == name).unwrap()].clone()
        })
    };
    // With 0x20 clear, the skipped all-empty key is final empty, not
    // missing or deferred. The related read and its consumer still finish.
    assert_eq!(
        read(&mut provider),
        [None, None, Some("/".into()), None, Some("1".into())]
    );
    provider
        .write_record(&related_write(None, Some(created.id), &[(NAME, "1")]))
        .unwrap();
    assert_eq!(
        read(&mut provider),
        [
            Some("K1".into()),
            Some("first".into()),
            Some("first/".into()),
            Some("1".into()),
            Some("2".into())
        ]
    );
    for (input, key, copied) in [
        ("2", "K2", "second"),
        ("2", "K2", "second"),
        ("", "", "second"),
    ] {
        provider
            .write_record(&related_write(None, Some(created.id), &[(NAME, input)]))
            .unwrap();
        assert_eq!(
            read(&mut provider),
            [
                (!key.is_empty()).then(|| key.into()),
                Some(copied.into()),
                Some(format!("{copied}/")),
                (!input.is_empty()).then(|| input.to_owned()),
                Some(if input.is_empty() {
                    "1".into()
                } else {
                    "3".into()
                })
            ]
        );
    }
    provider.close();

    // An unrelated edit refreshes an audit input during evaluation, not in
    // the original change set. Its final-key demand must still be read.
    let mut files = make();
    const AUDIT: u64 = 32;
    let root = &mut files[0].1;
    fixture::add_field(root, TABLE, AUDIT, "Audit", 1, 1, 9, 1, 0);
    let mut names = fixture::names();
    names.add_field(TABLE, AUDIT, "Audit");
    fixture::add_formula(root, &names, (TABLE, OCCURRENCE), KEY, 5, "\"K\" & Audit");
    for (field, value) in [
        (AUDIT, "before"),
        (KEY, "Kbefore"),
        (COPY, "previous"),
        (AFTER, "previous/"),
    ] {
        put(root, 1, field, value);
    }
    let child = &mut files[1].1;
    fixture::add_formula(
        child,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        KEY,
        5,
        "\"K\" & Name",
    );
    put(child, 1, NAME, ACCOUNT);
    put(child, 1, KEY, &format!("K{ACCOUNT}"));
    let host = Host::new("audit-calculated-lookup-key", files);
    let mut provider = host.open(ROOT);
    provider
        .write_record(&related_write(None, Some(1), &[(COUNT, "4")]))
        .unwrap();
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let row = page.records.iter().find(|r| r.id == 1).unwrap();
    for (field, expected) in [
        ("Key", format!("K{ACCOUNT}")),
        ("Copied", ACCOUNT.into()),
        ("After", format!("{ACCOUNT}/")),
    ] {
        assert_eq!(
            row.values[page.fields.iter().position(|f| f.name == field).unwrap()],
            Some(expected)
        );
    }
    provider.close();

    // Unsupported storage and a lookup/calculation cycle refuse before writes.
    for (options, cycle) in [(0x2_0020, false), (0x1_0020, false), (0x20, true)] {
        let mut files = make();
        let root = &mut files[0].1;
        fixture::set_field_options(root, TABLE, KEY, options);
        if cycle {
            let mut names = fixture::names();
            names.add_field(TABLE, COPY, "Copied");
            fixture::add_formula(root, &names, (TABLE, OCCURRENCE), KEY, 5, "Copied");
        }
        let host = Host::new("unsupported-calculated-lookup-key", files);
        let mut provider = host.open(ROOT);
        let before = commits_to(&host, ROOT);
        assert!(provider
            .write_record(&related_write(None, None, &[(NAME, "1")]))
            .is_err());
        assert_eq!(commits_to(&host, ROOT), before);
        provider.close();
    }
}
