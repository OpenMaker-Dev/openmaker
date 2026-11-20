//! The desktop client's finds with FileMaker's find operators, script
//! transactions, field index values and serial numbers against an
//! `om-server` on loopback, over the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs` (so they run without the git-ignored
//! copy of test project).
//!
//! Finds run on the host's indexes where the finders express a criterion
//! and on the candidates' stored values where they do not; every result is
//! compared with the client's own evaluation of the same requests over all
//! records (`om_data::found::find`) and, where the Claris pages give one,
//! with the expected records. Transactions span both tables of the file:
//! Tasks (Code validated only during data entry here, Creator auto-entered)
//! and Notes (Body validated always).

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fixture::{
    TempFile, ACCOUNT, CODE, COUNT, DOUBLED, DUE, FILE, NAME, OCCURRENCE, PASSWORD, TABLE,
};
use om_data::found::{self, FindKind, FindRequest, FoundSetChange};
use om_data::runtime::TransactionOptions;
use om_data::{Connection, DataError, DataProvider, FieldChanges, Record, RecordPage};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_protocol::{LoginEnvironment, MachineInfo};
use om_provider::{NativeConfig, NativeProvider};
use om_scripts::text_node;
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

/// A server hosting `path` on an ephemeral port, and the CA to trust.
fn host(path: &Path) -> (u16, Vec<u8>) {
    host_recording(path, None)
}

/// [`host`], recording its connections in `record_dir`.
fn host_recording(path: &Path, record_dir: Option<PathBuf>) -> (u16, Vec<u8>) {
    host_with(path, record_dir, |server| server)
}

/// [`host_recording`], the server configured by `configure` first.
fn host_with(
    path: &Path,
    record_dir: Option<PathBuf>,
    configure: impl FnOnce(Server) -> Server,
) -> (u16, Vec<u8>) {
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: path.to_path_buf(),
        }],
        insecure_files: false,
        record_dir,
    };
    let server = configure(Server::bind("127.0.0.1:0", config, identity).unwrap());
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    (port, ca_pem)
}

fn open(port: u16, ca_pem: &[u8]) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem.to_vec(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    provider
}

/// A session of its own, for what the provider does not expose.
fn raw_session(port: u16) -> om_session::Session {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut session = om_session::Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.key_pair.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.open_file("127.0.0.1", FILE).unwrap();
    let creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    assert!(session
        .login(ACCOUNT, PASSWORD, creation, &environment)
        .unwrap()
        .is_success());
    session
}

/// The Tasks file with Code validated only during data entry (by
/// calculation, strict), a value index on Name, and `rows` (Name, Count,
/// Due) as Tasks records 3, 4, ... (the client writes only text fields).
fn file(tag: &str, rows: &[(String, String, String)]) -> TempFile {
    file_with(tag, rows, |_| {})
}

/// [`file`], its tree changed by `extend` first.
fn file_with(
    tag: &str,
    rows: &[(String, String, String)],
    extend: impl FnOnce(&mut OwnedNode),
) -> TempFile {
    let mut root = fixture::tree(&[], true);
    extend(&mut root);
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x5);
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    let table = uint_segment(TABLE);
    let mut ids: RidList = [1, 2].into_iter().collect();
    for (i, (name, count, due)) in rows.iter().enumerate() {
        let id = i as u64 + 3;
        let record = serial::encode(id);
        let doubled = count.parse::<f64>().map(|n| (n * 2.0).to_string());
        let cells = [
            (NAME, Some(name.clone())),
            (COUNT, Some(count.clone())),
            (DUE, Some(due.clone())),
            (DOUBLED, doubled.ok()),
        ];
        for (field, value) in cells {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                root.put(
                    &[&table, &[5], &record, &uint_segment(field)],
                    text_node(&[], &value),
                );
            }
        }
        ids.insert(id);
    }
    root.put(&[&table, &[0x0d]], ids.to_node(Vec::new()));
    // The last issued record id, `[u8 len][serial]`.
    let last = serial::encode(rows.len() as u64 + 2);
    let mut key = vec![last.len() as u8];
    key.extend(last);
    root.put(&[&table, &[0]], OwnedNode::leaf(Vec::new(), key));
    TempFile::from_tree(tag, &root)
}

fn layout(provider: &mut NativeProvider, name: &str) -> u32 {
    provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|layout| layout.name == name)
        .unwrap_or_else(|| panic!("no layout {name}"))
        .id
}

fn all_records(provider: &mut NativeProvider, layout: u32) -> RecordPage {
    provider.records(layout, 0, 1000).unwrap()
}

fn text<'p>(page: &'p RecordPage, id: i64, field: &str) -> Option<&'p str> {
    let column = page.fields.iter().position(|f| f.name == field).unwrap();
    page.records.iter().find(|r| r.id == id)?.values[column].as_deref()
}

fn ids_of(page: &RecordPage) -> Vec<i64> {
    page.records.iter().map(|r| r.id).collect()
}

fn values(pairs: &[(&str, &str)]) -> FieldChanges {
    pairs
        .iter()
        .map(|(field, value)| {
            (
                (*field).to_owned(),
                (!value.is_empty()).then(|| (*value).to_owned()),
            )
        })
        .collect()
}

/// Calculated source keys must be computed by the downloading source,
/// including its authenticated account, before the target index is used.
#[test]
fn related_data_fetch_evaluates_source_keys_with_connection_metadata() {
    use om_data::host::{CalcNeed, CalcSource};

    let file = file_with("calculated-match-keys", &[], |root| {
        fixture::add_calculated(root);
        fixture::set_field_options(root, fixture::NOTES, fixture::TASK, 0x8000_0000);
        let names = fixture::calculated_names();
        let mut context = om_formula::Context::new(&names);
        context.occurrence = Some(OCCURRENCE);
        let compiled = om_formula::compile(
            "Case ( not IsEmpty ( Code ) ; Code ; Get ( AccountName ) )",
            &context,
        )
        .unwrap();
        let table = uint_segment(TABLE);
        let key = uint_segment(fixture::KEY);
        for (child, bytes) in [
            (5, om_formula::formatted::encode(&compiled.tokens)),
            (6, om_formula::polish::encode(&compiled.expr)),
        ] {
            root.put(
                &[&table, &[3], &[5], &key, &[5], &[child]],
                OwnedNode::leaf(Vec::new(), bytes),
            );
        }
        for (id, code) in [(1, "Ship"), (2, "")] {
            root.put(
                &[&table, &[5], &serial::encode(id), &uint_segment(CODE)],
                text_node(&[], code),
            );
        }
        let notes = uint_segment(fixture::NOTES);
        for (id, task, body) in [
            (1, "Write tests", "First"),
            (2, "Ship", "Second"),
            (3, ACCOUNT, "Account"),
        ] {
            for (field, value) in [(fixture::TASK, task), (fixture::BODY, body)] {
                root.put(
                    &[&notes, &[5], &serial::encode(id), &uint_segment(field)],
                    text_node(&[], value),
                );
            }
        }
        let ids: RidList = [1, 2, 3].into_iter().collect();
        root.put(&[&notes, &[0x0d]], ids.to_node(Vec::new()));
    });
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let schema = provider.calc_schema().unwrap().unwrap();
    let data = provider
        .calc_data(&[
            CalcNeed::Related {
                from: OCCURRENCE,
                record: 1,
                to: fixture::NOTES_BY_KEY,
            },
            CalcNeed::Related {
                from: OCCURRENCE,
                record: 2,
                to: fixture::NOTES_BY_KEY,
            },
        ])
        .unwrap();
    assert_eq!(
        data.related[&(OCCURRENCE, 1, fixture::NOTES_BY_KEY)].as_ref(),
        &[2]
    );
    assert_eq!(
        data.related[&(OCCURRENCE, 2, fixture::NOTES_BY_KEY)].as_ref(),
        &[3]
    );
    let cx = om_data::calc::Ctx::new(
        &schema.locale,
        om_data::calc::Date::from_ymd(2026, 1, 2).unwrap(),
    );
    for (record, expected) in [(2, "Second"), (3, "Account")] {
        assert_eq!(
            data.records[&(fixture::NOTES, record)]
                .as_ref()
                .unwrap()
                .get(fixture::BODY, 1)
                .unwrap()
                .to_text(&cx)
                .as_str(),
            expected
        );
    }
    // Results fetched through Recorder remain usable without another read.
    let cache = std::cell::RefCell::new(data);
    let cached = om_data::host::CachedSource::new(&cache);
    assert_eq!(
        cached
            .related(&schema, OCCURRENCE, 2, fixture::NOTES_BY_KEY, &cx)
            .unwrap()
            .as_ref(),
        &[3]
    );
    assert!(cached.missing().is_empty());
}

/// Needs the provider cannot read (here of a table the calculation schema
/// lacks) are left out of the answer, which says why, and the other needs
/// of the same batch are still read, including the records of tables
/// downloaded after the failed one. Only a batch with nothing readable
/// fails as a whole.
#[test]
fn calculation_data_survives_a_table_it_cannot_read() {
    use om_data::host::CalcNeed;
    let file = file("unreadable-table", &[]);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    // Sorts before both tables of the file in the batch of records read
    // ahead, so its failure comes first.
    let unknown = 0x7fff;
    let unreadable = [
        CalcNeed::Record {
            table: unknown,
            record: 1,
        },
        CalcNeed::Records { table: unknown },
        CalcNeed::Globals { table: unknown },
    ];
    assert!(provider.calc_data(&unreadable).is_err());
    let mut needs = unreadable.to_vec();
    needs.extend([
        CalcNeed::Record {
            table: TABLE,
            record: 1,
        },
        CalcNeed::Record {
            table: fixture::NOTES,
            record: 1,
        },
        CalcNeed::Records {
            table: fixture::NOTES,
        },
        CalcNeed::Globals { table: TABLE },
    ]);
    let data = provider.calc_data(&needs).expect("the readable needs");
    for need in &unreadable {
        assert!(!data.has(need), "{need:?}");
    }
    for need in &needs[unreadable.len()..] {
        assert!(data.has(need), "{need:?}");
    }
    assert!(data.records[&(TABLE, 1)].is_some());
    assert!(data.records[&(fixture::NOTES, 1)].is_some());
    assert_eq!(
        failures(&data),
        [(unknown, "The calculation schema has no table 0x7fff.")]
    );
}

/// A table whose records the host refuses to download fails as one the
/// schema lacks: its needs are left out of the answer, which says why, and
/// the records of the table downloaded after it are still read.
#[test]
fn calculation_data_reports_a_refused_download() {
    use om_data::host::CalcNeed;
    const REFUSED: &str = "Server record download failed (lock error 0x00c8).";
    let file = file("refused-table", &[]);
    let tasks = uint_segment(TABLE);
    let (port, ca_pem) = host_with(&file.0, None, |server| {
        server.refusing_downloads(&[&tasks, &[0x05]])
    });
    let mut provider = open(port, &ca_pem);
    let refused = [1, 2].map(|record| CalcNeed::Record {
        table: TABLE,
        record,
    });
    let error = provider.calc_data(&refused).expect_err("nothing readable");
    assert_eq!(error.message, REFUSED);
    let mut needs = refused.to_vec();
    needs.extend([
        CalcNeed::Record {
            table: fixture::NOTES,
            record: 1,
        },
        CalcNeed::Records {
            table: fixture::NOTES,
        },
    ]);
    let data = provider.calc_data(&needs).expect("the readable needs");
    for need in &refused {
        assert!(!data.has(need), "{need:?}");
    }
    for need in &needs[refused.len()..] {
        assert!(data.has(need), "{need:?}");
    }
    assert!(data.records[&(fixture::NOTES, 1)].is_some());
    assert_eq!(failures(&data), [(TABLE, REFUSED)]);
}

/// Why an answer leaves out each table's needs.
fn failures(data: &om_data::host::CalcData) -> Vec<(u64, &str)> {
    data.failures
        .iter()
        .map(|(table, error)| (*table, error.message.as_str()))
        .collect()
}

/// Another occurrence of Tasks, related to Notes like Tasks.
const SAME_TASKS: u64 = 0xd0_0010;

/// Tasks (Name; 301 records besides the fixture's 2, record 3 "Ship")
/// related to Notes (Body; note 2 "Ship", 298 others) by relationship 9,
/// and Notes to [`SAME_TASKS`] by relationship 10, both Name = Body. Both
/// fields are marked value-indexed, without index nodes.
fn multi_hop_file(tag: &str) -> TempFile {
    let rows: Vec<_> = std::iter::once(("Ship".to_string(), "7".to_string(), String::new()))
        .chain((0..300).map(|i| (format!("Other {i}"), "1".into(), String::new())))
        .collect();
    file_with(tag, &rows, |root| {
        use fixture::{BODY, NOTES, NOTES_OCCURRENCE};
        fixture::add_occurrence(root, SAME_TASKS, "Same tasks", TABLE);
        fixture::add_relationship(root, 9, (OCCURRENCE, NAME), 0, (NOTES_OCCURRENCE, BODY));
        fixture::add_relationship(root, 10, (NOTES_OCCURRENCE, BODY), 0, (SAME_TASKS, NAME));
        fixture::set_field_options(root, NOTES, BODY, 0x8000_0000);
        // Note 1 ("Hi") is the fixture's; note 2 is the only other match.
        let notes = uint_segment(NOTES);
        for id in 2..=300 {
            let body = if id == 2 { "Ship" } else { "Unrelated" };
            root.put(
                &[&notes, &[5], &serial::encode(id), &uint_segment(BODY)],
                text_node(&[], body),
            );
        }
        let ids: RidList = (1..=300).collect();
        root.put(&[&notes, &[0x0d]], ids.to_node(Vec::new()));
    })
}

/// Give `fields` (table, field) the value indexes a file's indexed fields
/// have: the first find on each makes the host build it (query-program.md
/// 7.1), which the provider sees missing and does not use; the next one
/// answers from it.
fn build_indexes(provider: &mut NativeProvider, fields: &[(u64, u64)]) {
    use om_data::host::CalcNeed;
    let needs: Vec<CalcNeed> = fields
        .iter()
        .map(|&(table, field)| CalcNeed::Lookup {
            table,
            field,
            text: "Ship".into(),
        })
        .collect();
    for answered in [false, true] {
        let data = provider.calc_data(&needs).unwrap();
        assert_eq!(data.lookups.len(), fields.len());
        assert!(
            data.lookups.values().all(|ids| ids.is_some() == answered),
            "{:?}",
            data.lookups
        );
    }
}

/// The QueryProgram (op86) requests of every connection `dir` recorded.
fn queries(dir: &Path) -> usize {
    let mut queries = 0;
    for connection in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(connection.unwrap().path().join("c2s.bin")).unwrap();
        let mut rest = bytes.as_slice();
        while let Ok((_, length)) = om_giop::parse_header(rest) {
            let (message, after) = rest.split_at(length);
            rest = after;
            let Ok(request) = om_giop::Request::parse(message) else {
                continue;
            };
            if request.operation == b"Perform" {
                let perform = om_protocol::parse_perform_request(request.body).unwrap();
                queries += usize::from(
                    perform.transaction.first() == Some(&(om_protocol::Opcode::QueryProgram as u8)),
                );
            }
        }
    }
    queries
}

/// A value list past one reply page (`0xFFF00` bytes, query-program.md
/// 11.1) comes whole: the session asks again with an empty program and the
/// reply's resume block until its cursor is 0, as FileMaker Pro did for a
/// pop-up's list on a hosted file (#464, one capture), and the host's list
/// is taken.
#[test]
fn a_value_list_past_one_reply_page_comes_whole_through_continuations() {
    const VALUES: usize = 30_000;
    let rows: Vec<_> = (0..VALUES)
        .map(|i| (format!("Item {i:05}"), String::new(), String::new()))
        .collect();
    let file = file("pages", &rows);
    let dir = record_dir("pages");
    let (port, ca_pem) = host_recording(&file.0, Some(dir.clone()));
    let mut provider = open(port, &ca_pem);
    build_indexes(&mut provider, &[(TABLE, NAME)]);
    let before = queries(&dir);
    let listed = provider.host_field_values(TABLE, NAME).unwrap().unwrap();
    let pages = queries(&dir) - before;
    let items: Vec<&String> = listed.iter().filter(|v| v.starts_with("Item ")).collect();
    assert_eq!(items.len(), VALUES);
    assert_eq!(
        (items[0].as_str(), items[VALUES - 1].as_str()),
        ("Item 00000", "Item 29999")
    );
    assert!(pages > 1, "{pages} request(s): the list fit one page");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder for a server's recordings, unique to the test.
fn record_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "om-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// A path of two relationships, Tasks → Notes → another occurrence of
/// Tasks, narrows each hop with the host's value index: only the parent
/// and the matching rows of each hop are read, never the unrelated rows a
/// scan of either table would download.
#[test]
fn multi_hop_relationships_read_only_matching_rows() {
    use om_data::host::CalcNeed;
    let file = multi_hop_file("multi-hop-index");
    let record_dir = record_dir("multi-hop");
    let (port, ca_pem) = host_recording(&file.0, Some(record_dir.clone()));
    let mut provider = open(port, &ca_pem);
    build_indexes(
        &mut provider,
        &[(fixture::NOTES, fixture::BODY), (TABLE, NAME)],
    );
    let data = provider
        .calc_data(&[CalcNeed::Related {
            from: OCCURRENCE,
            record: 2,
            to: SAME_TASKS,
        }])
        .unwrap();
    let rows = row_downloads(&record_dir);
    let _ = std::fs::remove_dir_all(&record_dir);
    assert_eq!(data.related[&(OCCURRENCE, 2, SAME_TASKS)].as_ref(), &[2, 3]);
    // Tasks 2 (the parent) and 3, and note 2.
    assert_eq!(rows, BTreeMap::from([(TABLE, 2), (fixture::NOTES, 1)]));
}

/// FileMaker Server answers a find on a field without the index it reads
/// with no records and builds the index afterwards (query-program.md 7.1),
/// and a field marked indexed can lack its index node (schema-path.md 6),
/// as both fields of [`multi_hop_file`] do. Against a server answering so,
/// no related set takes that first answer for "nothing related": each is
/// the scan's until the finds it sent have built the indexes of every hop,
/// and from then on only matching rows are read.
#[test]
fn related_sets_do_not_trust_a_find_on_a_missing_index() {
    use om_data::host::CalcNeed;
    let file = multi_hop_file("late-index");
    let record_dir = record_dir("late-index");
    let (port, ca_pem) = host_with(
        &file.0,
        Some(record_dir.clone()),
        Server::with_late_index_builds,
    );
    let mut downloaded = BTreeMap::new();
    let mut related = |to: u64| {
        // A new connection each time: nothing cached from before.
        let data = open(port, &ca_pem)
            .calc_data(&[CalcNeed::Related {
                from: OCCURRENCE,
                record: 2,
                to,
            }])
            .unwrap();
        let rows = row_downloads(&record_dir);
        let new: BTreeMap<u64, usize> = rows
            .iter()
            .map(|(table, n)| (*table, n - downloaded.get(table).copied().unwrap_or(0)))
            .filter(|(_, n)| *n > 0)
            .collect();
        downloaded = rows;
        (data.related[&(OCCURRENCE, 2, to)].to_vec(), new)
    };
    // One hop: Notes::Body had no index, so every note is read; the find
    // built the index.
    let (records, rows) = related(fixture::NOTES_OCCURRENCE);
    assert_eq!(records, [2]);
    assert_eq!(rows[&fixture::NOTES], 300);
    // Two hops: the second one's Tasks::Name had no index.
    let (records, rows) = related(SAME_TASKS);
    assert_eq!(records, [2, 3]);
    assert!(rows[&TABLE] > 300, "{rows:?}");
    // Both indexes exist now.
    let (records, rows) = related(SAME_TASKS);
    let _ = std::fs::remove_dir_all(&record_dir);
    assert_eq!(records, [2, 3]);
    assert_eq!(rows, BTreeMap::from([(TABLE, 2), (fixture::NOTES, 1)]));
}

/// How many records the row downloads (op92 over `[table, 05]`) recorded in
/// `dir` asked for, by table.
fn row_downloads(dir: &Path) -> BTreeMap<u64, usize> {
    let mut rows = BTreeMap::new();
    for connection in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(connection.unwrap().path().join("c2s.bin")).unwrap();
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            // The provider can still be recording its final Disconnect
            // when this snapshot is read. Completed download requests have
            // already been answered; an unfinished trailing message is not
            // another row request.
            let (_, length) = match om_giop::parse_header(rest) {
                Ok(header) => header,
                Err(om_giop::ParseError::Incomplete) => break,
                Err(error) => panic!("invalid captured GIOP message: {error}"),
            };
            let (message, after) = rest.split_at(length);
            rest = after;
            let Ok(request) = om_giop::Request::parse(message) else {
                continue;
            };
            if request.operation != b"Perform" {
                continue;
            }
            let transaction = om_protocol::parse_perform_request(request.body)
                .unwrap()
                .transaction;
            if transaction.first() != Some(&(om_protocol::Opcode::DownloadList as u8)) {
                continue;
            }
            let download = om_protocol::parse_download_list_request(transaction).unwrap();
            if let [table, records] = download.key_path.as_slice() {
                if records == &[5] {
                    let keys = om_protocol::key_list_keys(&download.payload).unwrap();
                    *rows
                        .entry(om_hbam::slot::be_uint(table).unwrap())
                        .or_default() += keys.len();
                }
            }
        }
    }
    rows
}

/// Stored style streams travel from a synthetic hosted file into calculation
/// values, using that file's font catalog. No customer file is needed.
#[test]
fn calculation_reads_keep_record_global_and_date_styles() {
    use om_data::calc::eval::{evaluate_text, Context, Variables};
    use om_data::calc::{Ctx, Date};
    use om_data::host::{CachedSource, CalcNeed, FileCalc, FileHost, SessionInfo};

    fn styled_cell(text: &str) -> OwnedNode {
        let bytes = om_scsu::encode_scsu(text);
        let header = [
            &(bytes.len() as u32).to_be_bytes()[..],
            &(text.encode_utf16().count() as u32).to_be_bytes(),
        ]
        .concat();
        let style = [0, 0x17, 0, 22, 1, 0, 0, 9, 221, 0, 0, 255];
        let mut runs = vec![];
        for word in [1u32, 0, 0, 1, 1, style.len() as u32] {
            runs.extend(word.to_be_bytes());
        }
        runs.extend(style);
        let mut node = OwnedNode::default();
        node.put(&[&[0]], OwnedNode::leaf(Vec::new(), header));
        node.put(&[&[1]], OwnedNode::leaf(Vec::new(), bytes));
        node.put(&[&[0xff], &[1]], OwnedNode::leaf(Vec::new(), runs));
        node
    }
    let file = file_with("stored-styles", &[], |root| {
        root.put(&[&[25], &[5], &[22], &[16]], text_node(&[], "Helvetica"));
        let table = uint_segment(TABLE);
        let record = serial::encode(1);
        root.put(
            &[&table, &[5], &record, &uint_segment(NAME)],
            styled_cell("aβ"),
        );
        root.put(
            &[&table, &[5], &record, &uint_segment(COUNT)],
            styled_cell("012"),
        );
        root.put(
            &[&table, &[5], &record, &uint_segment(DUE)],
            styled_cell("4/4/2017"),
        );
        root.put(
            &[&table, &[8], &uint_segment(fixture::SETTING)],
            styled_cell("Global"),
        );
    });
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema.font_families.get(&22).map(String::as_str),
        Some("Helvetica")
    );
    let data = provider
        .calc_data(&[
            CalcNeed::Record {
                table: TABLE,
                record: 1,
            },
            CalcNeed::Globals { table: TABLE },
        ])
        .unwrap();
    let cache = std::cell::RefCell::new(data);
    let source = CachedSource::new(&cache);
    let today = Date::from_ymd(2026, 1, 2).unwrap();
    let cx = Ctx::new(&schema.locale, today);
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let session = SessionInfo::default();
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, today),
        &session,
        &mut locals,
        &mut globals,
    );
    let context = Context::record(OCCURRENCE, 1);
    for (formula, expected) in [
        ("Middle ( Name ; 2 ; 1 )", "β"),
        ("GetAsText ( Count )", "012"),
        ("GetAsText ( Due )", "4/4/2017"),
        ("Setting", "Global"),
    ] {
        let result = evaluate_text(formula, &mut host, &context)
            .value
            .to_text(&cx);
        assert_eq!(result.as_str(), expected, "{formula}");
        let runs = result.styled_runs();
        assert_eq!(runs.len(), 1, "{formula}");
        assert_eq!(runs[0].1.color, Some(0xdd0000), "{formula}");
        assert_eq!(runs[0].1.font.as_deref(), Some("Helvetica"), "{formula}");
        assert_eq!(runs[0].1.size, Some(9), "{formula}");
        assert_eq!(runs[0].1.flags, 256, "{formula}");
    }
    let normalized = evaluate_text("GetAsDate ( Due )", &mut host, &context)
        .value
        .to_text(&cx);
    assert!(normalized.styled_runs().is_empty());
    provider.close();
}

/// Tasks for the find operators: (Name, Count, Due). Records 1 "Write
/// tests" (3, 10/1/2026) and 2 "Ship" (5) are the fixture's.
const TASKS: [(&str, &str, &str); 30] = [
    ("Chris Smith", "3", "1/5/2026"),
    ("Christopher", "30", "1/15/2026"),
    ("Smithson", "300", "2/1/2026"),
    ("Gray", "-2", "12/31/2025"),
    ("Grey", "2.5", "3/3/2026"),
    ("Jon", "0", ""),
    ("John", "", "3/4/2026"),
    ("John Smith", "10", "6/30/2026"),
    ("Junior", "11", "7/1/2026"),
    ("Stephanie", "12", "10/1/2026"),
    ("Phan", "1000", "10/2/2026"),
    ("Market Services", "5", "10/31/2026"),
    ("Marketing Inc", "5", "11/1/2026"),
    ("Supermarket", "7", "1/1/2027"),
    ("Acme, Ltd.", "8", "2/28/2026"),
    ("Acme Ltd.", "9", "2/29/2028"),
    ("joey@abc.net", "13", ""),
    ("a   b", "", ""),
    ("òpera", "14", ""),
    ("opera", "15", ""),
    ("3", "16", ""),
    ("53", "17", ""),
    ("Äpfel", "18", ""),
    ("Zeta", "19", ""),
    ("alpha", "20", ""),
    ("", "21", ""),
    ("Jr.", "22", ""),
    ("ColdSpring Harbor", "23", ""),
    ("Johnson & Phillips", "24", ""),
    ("Paulson & Philson", "25", ""),
];

#[test]
fn find_operators_over_many_records_match_the_clients_evaluation() {
    find_operators_match_the_clients_evaluation("operators", |server| server);
}

/// FileMaker Server answers a find on a field without the index it reads
/// with no records and builds the index afterwards (query-program.md 7.1):
/// the Tasks file has no index nodes, so the first criterion on each index
/// (Name's word and value indexes, Count's and Due's value indexes) reads
/// a missing one, and its find is evaluated on the client.
#[test]
fn find_operators_match_the_clients_evaluation_when_indexes_are_built_late() {
    find_operators_match_the_clients_evaluation("late-operators", Server::with_late_index_builds);
}

/// Every find operator, run by the provider against a server configured
/// by `configure`, finds what the client's own evaluation of the records
/// finds (`found::find_on`).
fn find_operators_match_the_clients_evaluation(tag: &str, configure: fn(Server) -> Server) {
    let mut rows: Vec<(String, String, String)> = TASKS
        .iter()
        .map(|(n, c, d)| (n.to_string(), c.to_string(), d.to_string()))
        .collect();
    // Forty numbered items due through 2024, and one due today.
    for i in 1..=40 {
        rows.push((
            format!("Item {i:03}"),
            (100 + i).to_string(),
            format!("{}/{}/2024", i % 12 + 1, i % 28 + 1),
        ));
    }
    let (year, month, day) = found::today(local_offset());
    let today = format!("{month}/{day}/{year}");
    rows.push(("Today".into(), String::new(), today.clone()));
    // Fixture records that happen to fall on the day the test runs match //
    // as well.
    let mut due_today = vec!["Today"];
    due_today.extend(
        [("Write tests", "10/1/2026")]
            .into_iter()
            .chain(TASKS.iter().map(|&(name, _, due)| (name, due)))
            .filter(|&(_, due)| due == today)
            .map(|(name, _)| name),
    );
    let file = file(tag, &rows);
    let (port, ca_pem) = host_with(&file.0, None, configure);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let all = all_records(&mut provider, tasks);
    assert_eq!(all.records.len(), 2 + TASKS.len() + 41);
    let name = |id: i64| text(&all, id, "Name").unwrap_or("").to_owned();

    let mut find = |requests: Vec<FindRequest>| -> Result<Vec<i64>, Option<i32>> {
        let host = provider.change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: requests.clone(),
                kind: FindKind::Replace,
            },
        );
        // Compare with the provider's local calendar, rather than the
        // reference evaluator's default UTC date.
        let memory = found::find_on(
            &all.fields,
            &all.records,
            &requests,
            found::today(local_offset()),
        );
        match (host, memory) {
            (Ok(_), Ok(ids)) => {
                let page = all_records(&mut provider, tasks);
                assert_eq!(ids_of(&page), ids, "{requests:?}");
                Ok(ids)
            }
            (Err(h), Err(m)) => {
                assert_eq!(h.code, m.code, "{requests:?}");
                Err(h.code)
            }
            // No match: the host's find is 401, the evaluation is empty.
            (Err(h), Ok(ids)) if ids.is_empty() => {
                assert_eq!(h.code, Some(401), "{requests:?}");
                Ok(Vec::new())
            }
            (h, m) => panic!("{requests:?}: host {h:?}, client {m:?}"),
        }
    };
    let mut check = |field: &str, criterion: &str, expected: Option<&[&str]>| {
        let mut request = FindRequest::default();
        request.set(field, criterion);
        let ids = find(vec![request]).unwrap_or_else(|e| panic!("{field} {criterion:?}: {e:?}"));
        if let Some(expected) = expected {
            // The record due today matches date criteria depending on the
            // day the test runs.
            let mut names: Vec<String> = ids
                .iter()
                .map(|id| name(*id))
                .filter(|n| n != "Today" || expected.contains(&"Today"))
                .collect();
            names.sort();
            let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            expected.sort();
            assert_eq!(names, expected, "{field} {criterion:?}");
        }
    };
    // Words: each starts a word; whole words; whole values.
    check("Name", "Chris", Some(&["Chris Smith", "Christopher"]));
    check("Name", "smith chris", Some(&["Chris Smith"]));
    check("Name", "=John", Some(&["John", "John Smith"]));
    check("Name", "==John", Some(&["John"]));
    check("Name", "==John Smith", Some(&["John Smith"]));
    // Wildcards: one character, one digit, any characters, literal.
    check("Name", "Gr@y", Some(&["Gray", "Grey"]));
    check("Name", "J@n", Some(&["Jon"]));
    check("Name", "#", Some(&["3"]));
    check("Name", "##", Some(&["53"]));
    check("Name", "Item 00#", None);
    check("Name", "Jo*n", None);
    check("Name", "*phan*", Some(&["Stephanie", "Phan"]));
    check("Name", "joey\\@abc.net", Some(&["joey@abc.net"]));
    // Phrases from a word's start, anywhere, and accents.
    check("Name", "\"Acme, Ltd.\"", Some(&["Acme, Ltd."]));
    check(
        "Name",
        "\"market\"",
        Some(&["Market Services", "Marketing Inc"]),
    );
    check(
        "Name",
        "*\"son & Phil\"*",
        Some(&["Johnson & Phillips", "Paulson & Philson"]),
    );
    check("Name", "\"a   b\"", Some(&["a   b"]));
    check("Name", "òpera", Some(&["òpera", "opera"]));
    check("Name", "\"òpera\"", Some(&["òpera"]));
    check("Name", "~opera", Some(&["òpera", "opera"]));
    // Empty, not empty, invalid, duplicates.
    check("Name", "=", Some(&[""]));
    check("Name", "==", Some(&[""]));
    check("Name", "*", None);
    check("Name", "?", Some(&[]));
    check(
        "Count",
        "!",
        Some(&[
            "Chris Smith",
            "Write tests",
            "Ship",
            "Market Services",
            "Marketing Inc",
        ]),
    );
    check(
        "Count",
        "!5",
        Some(&["Ship", "Market Services", "Marketing Inc"]),
    );
    check("Count", "=", Some(&["John", "a   b", "Today"]));
    // Numbers by value and ranges.
    check("Count", "2.5", Some(&["Grey"]));
    check("Count", "<3", Some(&["Gray", "Grey", "Jon"]));
    check(
        "Count",
        "<=3",
        Some(&["Gray", "Grey", "Jon", "Chris Smith", "Write tests"]),
    );
    check("Count", ">=1000", Some(&["Phan"]));
    check("Count", "3...8", None);
    check("Count", "≥ 300", Some(&["Smithson", "Phan"]));
    check("Name", "Y...Zz", Some(&["Zeta"]));
    // Dates: whole days, wildcards, months, years, ranges and today.
    check("Due", "10/1/2026", Some(&["Write tests", "Stephanie"]));
    check(
        "Due",
        "10/*/2026",
        Some(&["Write tests", "Stephanie", "Phan", "Market Services"]),
    );
    check("Due", "1/2026", None);
    check("Due", "2027", Some(&["Supermarket"]));
    check("Due", "<1/1/2026", None);
    check("Due", "12/2025", Some(&["Gray"]));
    check(
        "Due",
        "12/1/2025...1/10/2026",
        Some(&["Gray", "Chris Smith"]),
    );
    check("Due", "2/28/2026...3/4/2026", None);
    check("Due", "//", Some(&due_today));
    // A criterion a date field cannot read.
    let mut unreadable = FindRequest::default();
    unreadable.set("Due", "someday");
    assert_eq!(find(vec![unreadable]), Err(Some(3)));

    // Several requests and omits combine with the client's filters.
    let mut first = FindRequest::default();
    first.set("Name", "*Ltd*");
    let mut second = FindRequest::default();
    second.set("Count", "<1");
    let mut omit = FindRequest::default();
    omit.set("Name", "==Acme Ltd.");
    omit.omit = true;
    let ids = find(vec![first, second, omit]).unwrap();
    let mut names: Vec<String> = ids.iter().map(|id| name(*id)).collect();
    names.sort();
    assert_eq!(names, ["Acme, Ltd.", "Gray", "Jon"]);
    provider.close();
}

/// FileMaker Server answers a find on a field without the index it reads
/// with no records and builds the index afterwards (query-program.md 7.1),
/// and none of the Tasks fields has an index node. The first find on each
/// index evaluates its criterion on every record instead of reporting
/// that nothing matches; the same find again answers from the index that
/// first find had the host build, reading no record. A find that matches
/// nothing is still 401.
#[test]
fn first_finds_on_missing_indexes_find_the_matching_records() {
    // Records 3 to 5, after 1 "Write tests" (Count 3, Due 10/1/2026,
    // Doubled 6) and 2 "Ship" (Count 5, Doubled 10).
    let rows: Vec<(String, String, String)> = [
        ("Ship it", "5", "1/2/2026"),
        ("Other", "7", "10/1/2026"),
        ("Shipping", "", ""),
    ]
    .iter()
    .map(|(n, c, d)| (n.to_string(), c.to_string(), d.to_string()))
    .collect();
    let file = file("late-finds", &rows);
    let record_dir = record_dir("late-finds");
    let (port, ca_pem) = host_with(
        &file.0,
        Some(record_dir.clone()),
        Server::with_late_index_builds,
    );
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let mut read = 0;
    // What one request finds, and how many Tasks rows the find read.
    let mut find = |field: &str, criterion: &str, omit: bool| {
        let mut request = FindRequest::default();
        request.set(field, criterion);
        request.omit = omit;
        let found = provider.change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Replace,
            },
        );
        let rows = row_downloads(&record_dir).get(&TABLE).copied().unwrap_or(0) - read;
        let found = found
            .map(|_| ids_of(&all_records(&mut provider, tasks)))
            .map_err(|e| e.code);
        read = row_downloads(&record_dir).get(&TABLE).copied().unwrap_or(0);
        (found, rows)
    };
    for (field, criterion, omit, expected) in [
        // Each on an index no find read before: the word index, number and
        // date values, a value index's "no value" finder and an omit
        // request.
        ("Name", "ship", false, Ok(vec![2, 3, 5])),
        ("Count", "5", false, Ok(vec![2, 3])),
        ("Due", "10/1/2026", false, Ok(vec![1, 4])),
        ("Doubled", "=", false, Ok(vec![5])),
        ("Name", "==Ship it", true, Ok(vec![1, 2, 4, 5])),
        ("Code", "zzz", false, Err(Some(401))),
    ] {
        let first = find(field, criterion, omit);
        assert_eq!(first, (expected.clone(), 5), "{field} {criterion:?}");
        let again = find(field, criterion, omit);
        assert_eq!(again, (expected, 0), "{field} {criterion:?} again");
    }
    let _ = std::fs::remove_dir_all(&record_dir);
}

/// A find on a related field against a server that builds indexes late
/// (query-program.md 7.1): [`multi_hop_file`]'s Notes::Body has no word
/// index for the criterion, and Tasks::Name no value index for the join
/// back to the layout's records (9.2). Both are evaluated on the client
/// the first time, reading every note and every task, and answered by the
/// host's indexes after.
#[test]
fn related_field_finds_do_not_trust_a_find_on_a_missing_index() {
    let file = multi_hop_file("late-related-find");
    let record_dir = record_dir("late-related-find");
    let (port, ca_pem) = host_with(
        &file.0,
        Some(record_dir.clone()),
        Server::with_late_index_builds,
    );
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let mut downloaded = BTreeMap::new();
    // What the find finds, and the rows it read by table.
    let mut find = |criterion: &str| {
        let mut request = FindRequest::default();
        request.set("Notes::Body", criterion);
        let found = provider.change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Replace,
            },
        );
        let rows: BTreeMap<u64, usize> = row_downloads(&record_dir)
            .into_iter()
            .map(|(table, n)| (table, n - downloaded.get(&table).copied().unwrap_or(0)))
            .filter(|(_, n)| *n > 0)
            .collect();
        let found = found
            .map(|_| ids_of(&all_records(&mut provider, tasks)))
            .map_err(|e| e.code);
        downloaded = row_downloads(&record_dir);
        (found, rows)
    };
    // Every note, then note 2's keys for the join; every task.
    let (found, rows) = find("ship");
    assert_eq!(found, Ok(vec![2, 3]));
    assert_eq!(rows, BTreeMap::from([(fixture::NOTES, 301), (TABLE, 303)]));
    let (found, rows) = find("ship");
    assert_eq!(found, Ok(vec![2, 3]));
    assert_eq!(rows, BTreeMap::from([(fixture::NOTES, 1)]));
    assert_eq!(find("zzz").0, Err(Some(401)));
    let _ = std::fs::remove_dir_all(&record_dir);
}

/// A request's keep finder and remove finder read different indexes.
/// Both must reach the host on the first find, so its second find uses
/// both built indexes and does not download the table again.
#[test]
fn a_first_mixed_find_checks_all_groups_before_streaming() {
    let file = file(
        "cold-mixed-groups",
        &[("Ship".into(), "5".into(), String::new())],
    );
    let record_dir = record_dir("cold-mixed-groups");
    let (port, ca_pem) = host_with(
        &file.0,
        Some(record_dir.clone()),
        Server::with_late_index_builds,
    );
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let mut request = request("Name", "ship");
    request.set("Due", "=");
    for expected_downloads in [3, 0] {
        let before = row_downloads(&record_dir).get(&TABLE).copied().unwrap_or(0);
        provider
            .change_found_set(
                tasks,
                FoundSetChange::Find {
                    requests: vec![request.clone()],
                    kind: FindKind::Replace,
                },
            )
            .unwrap();
        let after = row_downloads(&record_dir).get(&TABLE).copied().unwrap_or(0);
        assert_eq!(after - before, expected_downloads);
        assert_eq!(ids_of(&all_records(&mut provider, tasks)), [2, 3]);
    }
    provider.close();
    let _ = std::fs::remove_dir_all(&record_dir);
}

/// The records a find found, or its error code.
type Found = Result<Vec<i64>, Option<i32>>;

/// What `requests` find on `layout` (`kind` of find), or the find's error.
fn find_ids(
    provider: &mut NativeProvider,
    layout: u32,
    requests: Vec<FindRequest>,
    kind: FindKind,
) -> Found {
    provider
        .change_found_set(layout, FoundSetChange::Find { requests, kind })
        .map_err(|e| e.code)?;
    Ok(ids_of(&all_records(provider, layout)))
}

/// The same find twice against a server that builds indexes late: first
/// on indexes the host does not have, answered from indexes the client
/// builds, then from the indexes the first find had the host build.
fn cold_and_warm(
    provider: &mut NativeProvider,
    layout: u32,
    requests: Vec<FindRequest>,
    kind: FindKind,
) -> (Found, Found) {
    let cold = find_ids(provider, layout, requests.clone(), kind);
    let warm = find_ids(provider, layout, requests, kind);
    (cold, warm)
}

/// One request with one criterion.
fn request(field: &str, criterion: &str) -> FindRequest {
    let mut request = FindRequest::default();
    request.set(field, criterion);
    request
}

/// Stored values and the criteria run on them, by field type (1 text, 2
/// number, 3 date, 4 time, 5 timestamp): values with white space around
/// or inside them, several lines, text the type cannot read, and empty.
const GENERATED: [(u8, &[&str], &[&str]); 5] = [
    (
        1,
        &[
            "",
            "Ship",
            "Shipping",
            "ship it",
            "John Smith",
            "Jon",
            "Gray",
            "Grey",
            "òpera",
            "opera",
            "red\nblue",
            "red",
            "blue",
            " Ship ",
            " ",
            "\n",
            "x\ny",
            "x",
            "3",
            "53",
        ],
        &[
            "ship",
            "=Ship",
            "==Ship",
            "==red",
            "== Ship ",
            "=",
            "==",
            "*",
            "!",
            "!red",
            "Gr@y",
            "Jo*n",
            "*hip*",
            "#",
            "##",
            "\"ship\"",
            "~opera",
            "red...ship",
            "<Ship",
            "<=Ship",
            ">Ship",
            ">=Ship",
            "?",
        ],
    ),
    (
        2,
        &[
            "", "0", "3", "5", "10", "-2", "2.5", "05", "foo", " ", "5\n", "5\n6", "+5", "0.5",
        ],
        &[
            "0", "3", "5", "10", "-2", "2.5", "=", "==", "*", "!", "!5", "<3", "<=3", ">3", ">=3",
            "3...10", "?",
        ],
    ),
    (
        3,
        &[
            "",
            "10/1/2026",
            "10/2/2026",
            "1/1/2027",
            "2/29/2028",
            "foo",
            " ",
            "10/1/2026\n",
            "2/30/2026",
        ],
        &[
            "10/1/2026",
            "10/*/2026",
            "10/2026",
            "2026",
            "=",
            "==",
            "*",
            "!",
            "<10/2/2026",
            ">=10/1/2026",
            "10/1/2026...1/1/2027",
            "?",
        ],
    ),
    (
        4,
        &[
            "",
            "9:00:00",
            "9:30:00",
            "10:00:00",
            "23:59:59",
            "foo",
            " ",
            "9:00:00\n",
        ],
        &[
            "9",
            "9:00",
            "9:30:00",
            "9:*:00",
            "=",
            "==",
            "*",
            "!",
            "<10:00:00",
            ">=9:00:00",
            "9:00:00...10:00:00",
            "?",
        ],
    ),
    (
        5,
        &[
            "",
            "10/1/2026 9:00:00",
            "10/1/2026 9:30:00",
            "10/2/2026 10:00:00",
            "foo",
            " ",
            "10/1/2026 9:00:00\n",
        ],
        &[
            "10/1/2026",
            "10/1/2026 9",
            "10/*/2026 9:*:00",
            "=",
            "==",
            "*",
            "!",
            "<10/2/2026 10:00:00",
            ">=10/1/2026 9:00:00",
            "?",
        ],
    ),
];

/// A find on an index the host has not built yet finds what the same find
/// finds once the host has the index: every criterion of [`GENERATED`] on
/// a field of its own, of two repetitions, over 66 records whose values
/// cycle through the type's values (some in the second repetition).
#[test]
fn cold_and_warm_finds_agree_on_generated_values_and_criteria() {
    let mut probes = Vec::new();
    let rows: Vec<_> = (0..64)
        .map(|i| (format!("record-{i}"), String::new(), String::new()))
        .collect();
    let file = file_with("cold-warm-generated", &rows, |root| {
        let t = uint_segment(TABLE);
        for (data_type, values, criteria) in GENERATED {
            for criterion in criteria {
                let field = 100 + probes.len() as u64;
                let name = format!("Probe{field}");
                fixture::add_field(root, TABLE, field, &name, 1, data_type, 0, 0, 0);
                fixture::set_field_repetitions(root, TABLE, field, 2);
                for id in 1..=66u64 {
                    let first = values[(id as usize * 17 + 3) % values.len()];
                    let second = values[(id as usize * 7 + 5) % values.len()];
                    for (key, value) in [(field, first), (field * 256 + 2, second)] {
                        if !value.is_empty() && (key == field || id % 3 == 0) {
                            root.put(
                                &[&t, &[5], &serial::encode(id), &uint_segment(key)],
                                text_node(&[], value),
                            );
                        }
                    }
                }
                probes.push((name, data_type, *criterion));
            }
        }
    });
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let mut mismatches = Vec::new();
    for (name, data_type, criterion) in &probes {
        let (cold, warm) = cold_and_warm(
            &mut provider,
            tasks,
            vec![request(name, criterion)],
            FindKind::Replace,
        );
        if cold != warm {
            mismatches.push(format!(
                "type {data_type} {criterion:?}: cold {cold:?}, warm {warm:?}"
            ));
        }
    }
    provider.close();
    assert_eq!(probes.len(), 74);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// A find on a related field joins its matches back to the layout's
/// records through a value index the host has not built yet as it does
/// once it has: for every relationship operator, from a field of two
/// repetitions holding "Hi", " Hi ", other text, nothing, or "Hi" in the
/// second repetition only.
#[test]
fn cold_and_warm_related_finds_agree_for_every_join_operator() {
    let rows = vec![(String::new(), "1".into(), String::new()); 4];
    let file = file_with("cold-warm-joins", &rows, |root| {
        let t = uint_segment(TABLE);
        for op in 0..=6u8 {
            let field = 40 + u64::from(op);
            let occurrence = 0xd0_0030 + u64::from(op);
            fixture::add_field(root, TABLE, field, &format!("Join{op}"), 1, 1, 0, 0, 0);
            fixture::set_field_repetitions(root, TABLE, field, 2);
            fixture::add_occurrence(
                root,
                occurrence,
                &format!("Join notes {op}"),
                fixture::NOTES,
            );
            fixture::add_relationship(
                root,
                40 + op,
                (OCCURRENCE, field),
                op,
                (occurrence, fixture::BODY),
            );
            for (id, value) in [(2, "Hi"), (4, " Hi "), (5, "Beta"), (6, "Zebra")] {
                root.put(
                    &[&t, &[5], &serial::encode(id), &uint_segment(field)],
                    text_node(&[], value),
                );
            }
            root.put(
                &[&t, &[5], &serial::encode(1), &uint_segment(field * 256 + 2)],
                text_node(&[], "Hi"),
            );
        }
    });
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let mut mismatches = Vec::new();
    for op in 0..=6 {
        let criterion = request(&format!("Join notes {op}::Body"), "==Hi");
        let (cold, warm) = cold_and_warm(&mut provider, tasks, vec![criterion], FindKind::Replace);
        if cold != warm {
            mismatches.push(format!("operator {op}: cold {cold:?}, warm {warm:?}"));
        }
    }
    provider.close();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Against a server that builds indexes late, `rows` (Name, Count, Due)
/// after the fixture's records, the Tasks file changed by `extend`: what
/// `requests` find on Tasks before and after the host has the indexes
/// they read.
fn cold_and_warm_in(
    tag: &str,
    rows: &[(&str, &str, &str)],
    extend: impl FnOnce(&mut OwnedNode),
    requests: Vec<FindRequest>,
) -> (Found, Found) {
    let rows: Vec<(String, String, String)> = rows
        .iter()
        .map(|(n, c, d)| (n.to_string(), c.to_string(), d.to_string()))
        .collect();
    let file = file_with(tag, &rows, extend);
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let answers = cold_and_warm(&mut provider, tasks, requests, FindKind::Replace);
    provider.close();
    answers
}

/// `!` finds the records that share a value-index key, one per line
/// (query-program.md 6.3, 7.1): "red\nblue" and "red" share "red".
#[test]
fn a_first_duplicates_find_compares_the_keys_of_each_line() {
    let rows = [("red\nblue", "", ""), ("red", "", "")];
    let answers = cold_and_warm_in("cold-duplicates", &rows, |_| {}, vec![request("Name", "!")]);
    assert_eq!(answers, (Ok(vec![3, 4]), Ok(vec![3, 4])));
}

/// A find matches any repetition (the value index keys each one): record
/// 1's Name holds "Ship" in its second repetition only.
#[test]
fn a_first_find_reads_every_repetition() {
    let answers = cold_and_warm_in(
        "cold-repetitions",
        &[],
        |root| {
            fixture::set_field_repetitions(root, TABLE, NAME, 2);
            let record = serial::encode(1);
            let key = uint_segment(NAME * 256 + 2);
            root.put(
                &[&uint_segment(TABLE), &[5], &record, &key],
                text_node(&[], "Ship"),
            );
        },
        vec![request("Name", "ship")],
    );
    assert_eq!(answers, (Ok(vec![1, 2]), Ok(vec![1, 2])));
}

/// `==Ship` compares whole-value keys (query-program.md 7.1, 7.2): " Ship "
/// keys its spaces, so only "Ship" matches.
#[test]
fn a_first_whole_value_find_compares_index_keys() {
    let rows = [(" Ship ", "1", "")];
    let answers = cold_and_warm_in("cold-whole", &rows, |_| {}, vec![request("Name", "==Ship")]);
    assert_eq!(answers, (Ok(vec![2]), Ok(vec![2])));
}

/// A join back on a value index the host has not built yet relates the
/// keys that index holds (query-program.md 7.1, 9.2): empty text is the
/// key `01`, which relates to nothing through Tasks::Name ≠ Notes::Body
/// (file-metadata.md 4), so a task without a name is not related to note 1
/// "Hi"; " Hi " keys its spaces, so it is ≠ "Hi" and not = "Hi".
#[test]
fn a_first_join_back_relates_the_keys_of_the_value_index() {
    let relate = |op: u8| {
        move |root: &mut OwnedNode| {
            fixture::add_relationship(
                root,
                40,
                (OCCURRENCE, NAME),
                op,
                (fixture::NOTES_OCCURRENCE, fixture::BODY),
            );
        }
    };
    let rows = [("", "1", ""), (" Hi ", "1", "")];
    let find = || vec![request("Notes::Body", "==Hi")];
    let not_equal = cold_and_warm_in("cold-join-ne", &rows, relate(1), find());
    assert_eq!(not_equal, (Ok(vec![1, 2, 4]), Ok(vec![1, 2, 4])));
    let equal = cold_and_warm_in("cold-join-eq", &rows, relate(0), find());
    assert_eq!(equal, (Err(Some(401)), Err(Some(401))));
}

/// Give the file the French locale record `[2, 0d]` (file-metadata.md 2),
/// whose dates are day/month/year, and Tasks record 1 the date
/// 17/02/2014.
fn day_month_year(root: &mut OwnedNode) {
    let locale = om_calc::Locale::fr_fr();
    root.put(
        &[&[2], &[13]],
        OwnedNode::leaf(Vec::new(), locale.to_record()),
    );
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(1),
            &uint_segment(DUE),
        ],
        text_node(&[], "17/02/2014"),
    );
}

/// A file's stored dates are in its own date order, which its indexes key
/// (query-program.md 7.3): a day/month/year file's 17/02/2014 is February
/// 17, which Find mode reads `2/17/2014` as.
#[test]
fn a_first_find_keys_stored_dates_in_the_files_date_order() {
    for (name, order, stored) in [
        (
            "cold-day-month-year",
            om_collate::DateOrder::DayMonthYear,
            "17/02/2014",
        ),
        (
            "cold-year-month-day",
            om_collate::DateOrder::YearMonthDay,
            "2014/02/17",
        ),
    ] {
        let answers = cold_and_warm_in(
            name,
            &[],
            |root| {
                let mut locale = om_calc::Locale::fr_fr();
                locale.date_order = order;
                root.put(
                    &[&[2], &[13]],
                    OwnedNode::leaf(Vec::new(), locale.to_record()),
                );
                root.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(1),
                        &uint_segment(DUE),
                    ],
                    text_node(&[], stored),
                );
                fixture::add_field(root, TABLE, 90, "Moment", 1, 5, 0, 0, 0);
                root.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(1),
                        &uint_segment(90),
                    ],
                    text_node(&[], &format!("{stored} 21:26:57")),
                );
            },
            vec![{
                let mut find = request("Due", "2/17/2014");
                find.set("Moment", "2/17/2014 21:26:57");
                find
            }],
        );
        assert_eq!(answers, (Ok(vec![1]), Ok(vec![1])), "{order:?}");
    }
}

/// The join back from a related field compares the dates of both sides in
/// the file's date order: Tasks::Due = Notes::When, where note 1 "Hi" is
/// dated 17/02/2014 in a day/month/year file, relates task 1.
#[test]
fn a_join_back_keys_stored_dates_in_the_files_date_order() {
    const WHEN: u64 = 2;
    let answers = cold_and_warm_in(
        "cold-join-day-month-year",
        &[],
        |root| {
            day_month_year(root);
            fixture::add_field(root, fixture::NOTES, WHEN, "When", 1, 3, 0, 0, 0);
            root.put(
                &[
                    &uint_segment(fixture::NOTES),
                    &[5],
                    &serial::encode(1),
                    &uint_segment(WHEN),
                ],
                text_node(&[], "17/02/2014"),
            );
            fixture::add_relationship(
                root,
                40,
                (OCCURRENCE, DUE),
                0,
                (fixture::NOTES_OCCURRENCE, WHEN),
            );
        },
        vec![request("Notes::Body", "==Hi")],
    );
    assert_eq!(answers, (Ok(vec![1]), Ok(vec![1])));
}

/// An index language whose keys are not established (25, one of the
/// localized templates', query-program.md 7.2) is not keyed in English.
/// Without the host's index the find fails, saying why; the host, which
/// read its own index in English for that find, does not store it, so the
/// next find fails the same way (#159). A change of the field's text is
/// refused before it reaches the host, which would refuse it too, while
/// another field of the record changes.
#[test]
fn a_field_in_an_unknown_index_language_is_neither_found_nor_changed_by_guessing() {
    let rows: Vec<(String, String, String)> = Vec::new();
    let file = file_with("cold-language", &rows, |root| {
        fixture::set_field_language(root, TABLE, NAME, 25);
    });
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let cold = provider
        .change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request("Name", "ship")],
                kind: FindKind::Replace,
            },
        )
        .unwrap_err();
    assert_eq!(cold.code, Some(3), "{cold:?}");
    assert!(cold.message.contains("index language (25)"), "{cold:?}");
    let warm = find_ids(
        &mut provider,
        tasks,
        vec![request("Name", "ship")],
        FindKind::Replace,
    );
    assert_eq!(warm, Err(Some(3)));

    let before = all_records(&mut provider, tasks);
    let refused = provider
        .update_record(tasks, 2, values(&[("Name", "Äpfel")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("#1279"), "{refused:?}");
    assert_eq!(all_records(&mut provider, tasks), before);
    provider
        .update_record(tasks, 2, values(&[("Count", "7")]))
        .unwrap();
    let after = all_records(&mut provider, tasks);
    assert_eq!(text(&after, 2, "Count"), Some("7"));
    assert_eq!(text(&after, 2, "Name"), text(&before, 2, "Name"));
    provider.close();
}

/// In a language FileMaker leaves untailored (French, 24), whose value keys
/// are English's (query-program.md 7.2), a whole-value find runs without
/// the host's index, the first time and again; a word find, whose word
/// breaks are French's, still fails rather than guessing
/// ([#46](https://github.com/OpenMaker-Dev/openmaker/issues/46),
/// [#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)).
#[test]
fn an_untailored_language_finds_whole_values_without_the_hosts_index() {
    let french = |root: &mut OwnedNode| fixture::set_field_language(root, TABLE, NAME, 24);
    let values = cold_and_warm_in("cold-french", &[], french, vec![request("Name", "==Ship")]);
    assert_eq!(values, (Ok(vec![2]), Ok(vec![2])));
    let words = cold_and_warm_in(
        "cold-french-words",
        &[],
        french,
        vec![request("Name", "ship")],
    );
    assert_eq!(words, (Err(Some(3)), Err(Some(3))));
}

/// The missing target index of a related find must obey its own language
/// too, rather than using English because the source field is English.
#[test]
fn a_first_join_in_an_unknown_index_language_fails_instead_of_guessing() {
    let file = file_with("cold-join-language", &[], |root| {
        fixture::set_field_language(root, TABLE, NAME, 25);
        fixture::add_relationship(
            root,
            40,
            (OCCURRENCE, NAME),
            0,
            (fixture::NOTES_OCCURRENCE, 1),
        );
    });
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let error = provider
        .change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request("Notes::Body", "==Hi")],
                kind: FindKind::Replace,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, Some(3), "{error:?}");
    assert!(error.message.contains("index language (25)"), "{error:?}");
    provider.close();
}

/// A related find keys the related records' match values in the form of
/// the near field's value index they are compared with (#573): Tasks::Name,
/// indexed in Unicode raw (2), relates to a note whose Body holds "Ship" in
/// its second repetition, with the host's index and without, as "Ship"
/// relates "Ship" (every stored repetition is a match value).
#[test]
fn a_related_find_keys_related_values_in_the_near_fields_form() {
    let answers = cold_and_warm_in(
        "join-raw",
        &[],
        |root| {
            fixture::set_field_language(root, TABLE, NAME, 2);
            fixture::set_field_repetitions(root, fixture::NOTES, fixture::BODY, 2);
            fixture::add_relationship(
                root,
                40,
                (OCCURRENCE, NAME),
                0,
                (fixture::NOTES_OCCURRENCE, fixture::BODY),
            );
            let notes = uint_segment(fixture::NOTES);
            root.put(
                &[
                    &notes,
                    &[5],
                    &serial::encode(2),
                    &uint_segment(fixture::BODY * 256 + 2),
                ],
                text_node(&[], "Ship"),
            );
            let mut list = root
                .at(&[&notes, &[0x0d]])
                .and_then(|n| RidList::from_owned(n).ok())
                .unwrap_or_default();
            list.insert(2);
            root.put(&[&notes, &[0x0d]], list.to_node(Vec::new()));
        },
        vec![request("Notes::Body", "==Ship")],
    );
    assert_eq!(answers, (Ok(vec![2]), Ok(vec![2])));
}

/// Tasks::Name indexed in Japanese (31), with the value index FileMaker
/// built (`Ship`, record 2), relates to the note whose Body is `body`.
/// Japanese keys text without kana or kanji as English does
/// (query-program.md 7.2), so a related `Ship` is keyed there; `シップ` is
/// not, and fails the find (3), as no key OpenMaker builds is Japanese's.
fn japanese_join(tag: &str, body: &str, requests: Vec<FindRequest>) -> (Found, Found) {
    use om_query::keys::{record_value_entries, TextKeys};
    let body = body.to_owned();
    cold_and_warm_in(
        tag,
        &[],
        move |root| {
            fixture::set_field_options(root, TABLE, NAME, 0x8000_0000);
            fixture::set_field_language(root, TABLE, NAME, 31);
            let (t, f) = (uint_segment(TABLE), uint_segment(NAME));
            let ship = record_value_entries(
                om_hbam::DataType::Text,
                &["Ship".to_owned()],
                om_collate::DateOrder::MonthDayYear,
                TextKeys::Collated,
            );
            for key in &ship {
                let records: RidList = [2].into_iter().collect();
                root.put(&[&t, &[1], &f, &[0], key], records.to_node(Vec::new()));
            }
            // Its counts (records indexed, distinct keys; write-path.md
            // 5.5), whose GetKey tells a client the host has the index
            // (query-program.md 8.5).
            let count = (ship.len() as u64).to_be_bytes();
            root.put(
                &[&t, &[1], &f, &[0x10]],
                OwnedNode::leaf(Vec::new(), [count, count].concat()),
            );
            fixture::add_relationship(
                root,
                40,
                (OCCURRENCE, NAME),
                0,
                (fixture::NOTES_OCCURRENCE, fixture::BODY),
            );
            let notes = uint_segment(fixture::NOTES);
            root.put(
                &[
                    &notes,
                    &[5],
                    &serial::encode(2),
                    &uint_segment(fixture::BODY),
                ],
                text_node(&[], &body),
            );
            let mut list = root
                .at(&[&notes, &[0x0d]])
                .and_then(|n| RidList::from_owned(n).ok())
                .unwrap_or_default();
            list.insert(2);
            root.put(&[&notes, &[0x0d]], list.to_node(Vec::new()));
        },
        requests,
    )
}

/// [`japanese_join`]: `Ship` relates through the host's Japanese index;
/// `シップ` fails (3); and a request whose related find finds nothing
/// relates nothing, leaving the other request's record 2 (Count 5) found,
/// with no value to key.
#[test]
fn a_related_find_keys_japanese_text_without_kana_as_english() {
    let ship = vec![request("Notes::Body", "==Ship")];
    assert_eq!(
        japanese_join("join-ja", "Ship", ship),
        (Ok(vec![2]), Ok(vec![2]))
    );
    let kana = vec![request("Notes::Body", "==シップ")];
    assert_eq!(
        japanese_join("join-ja-kana", "シップ", kana),
        (Err(Some(3)), Err(Some(3)))
    );
    let either = vec![request("Count", "5"), request("Notes::Body", "==Nothing")];
    assert_eq!(
        japanese_join("join-ja-empty", "シップ", either),
        (Ok(vec![2]), Ok(vec![2]))
    );
}

/// A cartesian predicate does not read its displayed fields' indexes.
/// An unsupported language there must not prevent fallback for a different
/// predicate whose numeric index is missing (file-metadata.md 4.2).
#[test]
fn a_cartesian_join_field_does_not_need_supported_index_keys() {
    let answers = cold_and_warm_in(
        "cold-cartesian-language",
        &[],
        |root| {
            fixture::set_field_language(root, TABLE, NAME, 25);
            fixture::add_field(root, fixture::NOTES, 90, "Number", 1, 2, 0, 0, 0);
            root.put(
                &[
                    &uint_segment(fixture::NOTES),
                    &[5],
                    &serial::encode(1),
                    &uint_segment(90),
                ],
                text_node(&[], "3"),
            );
            fixture::add_relationship(
                root,
                40,
                (OCCURRENCE, COUNT),
                0,
                (fixture::NOTES_OCCURRENCE, 90),
            );
            let mut predicate = vec![6];
            for field in [NAME, 1] {
                let key = uint_segment(field);
                predicate.extend([key.len() as u8 + 1, key.len() as u8]);
                predicate.extend(key);
            }
            root.put(
                &[&[3], &[0xfb], &[5], &[40], &[3], &[2]],
                OwnedNode::leaf(Vec::new(), predicate),
            );
        },
        vec![request("Notes::Body", "==Hi")],
    );
    assert_eq!(answers, (Ok(vec![1]), Ok(vec![1])));
}

/// Several requests with an omit, in a find, a constrain and an extend
/// from records 1 to 3, each on a field the host has no index for yet.
#[test]
fn cold_and_warm_finds_agree_on_compound_requests() {
    let rows: Vec<_> = (0..24)
        .map(|i| (format!("row-{i}"), "1".into(), String::new()))
        .collect();
    let kinds = [FindKind::Replace, FindKind::Constrain, FindKind::Extend];
    let file = file_with("cold-warm-compound", &rows, |root| {
        for i in 0..kinds.len() as u64 {
            let field = 80 + i;
            fixture::add_field(
                root,
                TABLE,
                field,
                &format!("Compound{field}"),
                1,
                1,
                0,
                0,
                0,
            );
            for id in 1..=26u64 {
                let value = if id % 2 == 0 { "Ship" } else { "Other" };
                root.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(id),
                        &uint_segment(field),
                    ],
                    text_node(&[], value),
                );
            }
        }
    });
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    for (i, kind) in kinds.into_iter().enumerate() {
        let name = format!("Compound{}", 80 + i);
        let mut omit = request(&name, "==Other");
        omit.omit = true;
        let requests = vec![request(&name, "ship"), request(&name, "other"), omit];
        let mut answers = Vec::new();
        for _ in 0..2 {
            provider
                .change_found_set(tasks, FoundSetChange::Records(vec![1, 2, 3]))
                .unwrap();
            answers.push(find_ids(&mut provider, tasks, requests.clone(), kind));
        }
        let even: Vec<i64> = (2..=26).step_by(2).collect();
        let expected = match kind {
            FindKind::Replace => even,
            FindKind::Constrain => vec![2],
            FindKind::Extend => {
                let mut ids = even;
                ids.extend([1, 3]);
                ids.sort();
                ids
            }
        };
        assert_eq!(answers, [Ok(expected.clone()), Ok(expected)], "{kind:?}");
    }
    provider.close();
}

/// A checked find whose check fails gets the host's error slot (query-program.md
/// 11.2), not an answer: the GetKey names a file id this connection has not
/// opened (error 18).
#[test]
fn a_check_that_fails_answers_with_the_error_slot() {
    let file = file("check-error", &[]);
    let (port, _) = host(&file.0);
    let mut session = raw_session(port);
    let file_id = session.opened_file().unwrap().reply.file_id;
    let finders = om_query::find::finders(
        TABLE,
        NAME,
        om_hbam::DataType::Text,
        "ship",
        om_collate::DateOrder::MonthDayYear,
    )
    .unwrap();
    let program = om_query::find::checked_find_program(
        file_id,
        TABLE,
        &[(NAME, om_query::find::IndexKind::Word)],
        &[finders[0].bytes.clone()],
        None,
    );
    let mut program = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&program))
        .unwrap()
        .root
        .to_owned_node();
    program.put(
        &[&[1], &[1], &[4]],
        OwnedNode::leaf(Vec::new(), file_id.wrapping_add(1).to_be_bytes().to_vec()),
    );
    let reply = session.query(&program.to_wire().unwrap()).unwrap();
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&reply.slots)).unwrap();
    assert_eq!(tree.node(&[&[0]]).unwrap().value, Some(&[2, 0, 18][..]));
    session.disconnect().unwrap();
}

/// `om_query::find::join_on_keys` relates what JoinProjection relates over
/// the same keys in a file's value indexes (query-program.md 9.2), for every
/// pair of operators over two predicates, from two, one and no source rows.
#[test]
fn client_joins_relate_as_join_projection_on_generated_keys() {
    use om_query::find::{self as finders, JoinRow};
    let mut root = fixture::tree(&[], true);
    let t = uint_segment(TABLE);
    let targets: Vec<JoinRow> = (1..=40u64)
        .map(|id| {
            let fields = [40u64, 41]
                .into_iter()
                .map(|field| {
                    let keys = match (id * field) % 7 {
                        0 => vec![],
                        1 => vec![vec![1]],
                        2 => vec![vec![1, 2], vec![1, 6]],
                        n => vec![vec![1, n as u8]],
                    };
                    (field, keys)
                })
                .collect();
            (id, fields)
        })
        .collect();
    for field in [40, 41] {
        fixture::add_field(&mut root, TABLE, field, "Join key", 1, 1, 0, 0, 0);
        let mut index: BTreeMap<Vec<u8>, RidList> = BTreeMap::new();
        for (id, fields) in &targets {
            for (_, keys) in fields.iter().filter(|(f, _)| *f == field) {
                for key in keys {
                    index.entry(key.clone()).or_default().insert(*id);
                }
            }
        }
        for (key, ids) in index {
            root.put(
                &[&t, &[1], &uint_segment(field), &[0], &key],
                ids.to_node(Vec::new()),
            );
        }
    }
    root.put(
        &[&t, &[13]],
        (1..=40u64).collect::<RidList>().to_node(Vec::new()),
    );
    let temp = TempFile::from_tree("join-on-keys", &root);
    let file = om_hbam::HbamFile::open(&temp.0).unwrap();
    let rows: Vec<JoinRow> = vec![
        (
            91,
            vec![(1, vec![vec![1, 2], vec![1, 5]]), (2, vec![vec![1]])],
        ),
        (92, vec![(1, vec![vec![1, 3]]), (2, vec![vec![1, 4]])]),
    ];
    let mut cases = 0;
    for op in 0..=6u8 {
        for other in 0..=6u8 {
            for source in [&rows[..], &rows[..1], &rows[..0]] {
                let predicates = [(op, 40, 1), (other, 41, 2)];
                let program = finders::join_program(1, TABLE, source, &predicates, None).unwrap();
                let context = om_query::Context {
                    file_id: 1,
                    connection_key: None,
                };
                let outcome = om_query::execute(&file, &program, &context);
                assert!(outcome.error.is_none());
                let host = finders::found(&outcome.slots).unwrap();
                let client = finders::join_on_keys(&targets, source, &predicates);
                assert_eq!(client, host, "{predicates:?} from {} rows", source.len());
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 147);
}

/// The machine's offset from UTC, as the provider's finds read today.
fn local_offset() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
}

#[test]
fn browse_pages_defer_calculations_and_requested_fields_still_compute() {
    let file = file_with("deferred-page", &[], fixture::add_calculated);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");

    // The opening page has its stored data immediately. Neither the
    // off-layout summaries nor other unstored calculations are requested.
    let page = provider
        .records_with_calculations(tasks, 0, 1, &[])
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(text(&page, 1, "Name"), Some("Write tests"));
    assert_eq!(text(&page, 1, "Label"), None);
    assert_eq!(text(&page, 1, "Total"), None);

    let page = provider
        .records_with_calculations(tasks, 0, 1, &["Label".into()])
        .unwrap();
    assert_eq!(text(&page, 1, "Label"), Some("Write tests (3)"));
    assert_eq!(text(&page, 1, "Total"), None);

    // A requested summary still covers the whole found set, even though
    // this page contains only its first record.
    let page = provider
        .records_with_calculations(tasks, 0, 1, &["Total".into()])
        .unwrap();
    assert_eq!(text(&page, 1, "Total"), Some("8"));
    assert_eq!(text(&page, 1, "Label"), None);

    // Preserve the full-page contract for exports and existing callers.
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, 2, "Label"), Some("Ship (5)"));
    assert_eq!(text(&page, 2, "Total"), Some("8"));
    provider.close();
}

#[test]
fn projected_pages_keep_sort_context_for_lazy_summaries() {
    use std::cell::RefCell;

    use om_data::calc::eval::{Context, FieldId};
    use om_data::calc::Date;
    use om_data::found::{SortField, SortOrder};
    use om_data::host::{CachedSource, CalcNeed, CalcSource, FileCalc};

    let file = file_with("projected-sort", &[], |root| {
        fixture::add_calculated(root);
        let table = uint_segment(TABLE);
        let status = uint_segment(fixture::STATUS);
        for (id, group) in [(1, "A"), (2, "B")] {
            root.put(
                &[&table, &[5], &serial::encode(id), &status],
                text_node(&[], group),
            );
        }
    });
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let schema = provider.calc_schema().unwrap().unwrap();
    provider
        .change_found_set(
            tasks,
            FoundSetChange::Sort(vec![SortField::new("Status", SortOrder::Ascending)]),
        )
        .unwrap();
    let page = provider
        .records_with_calculations(tasks, 0, 1, &[])
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(text(&page, 1, "Running"), None);
    let data = provider
        .calc_data(&[
            CalcNeed::FoundSet {
                occurrence: OCCURRENCE,
                table: TABLE,
            },
            CalcNeed::Record {
                table: TABLE,
                record: 1,
            },
            CalcNeed::Record {
                table: TABLE,
                record: 2,
            },
        ])
        .unwrap();
    assert_eq!(data.sorts[&OCCURRENCE].as_ref(), &[fixture::STATUS]);
    let cache = RefCell::new(data);
    let today = Date::from_ymd(2026, 10, 29).unwrap();
    let summary = FieldId {
        occurrence: OCCURRENCE,
        field: fixture::TOTAL,
    };
    let group = FieldId {
        occurrence: OCCURRENCE,
        field: fixture::STATUS,
    };
    {
        let cached = CachedSource::new(&cache);
        let file = FileCalc::new(&schema, &cached, today);
        // The second record starts a new Status group, rather than
        // inheriting the first record's running total.
        let running = file
            .field_value(OCCURRENCE, 2, fixture::RUNNING, 1)
            .unwrap();
        assert_eq!(running.to_text(&file.cx()).into_string(), "5");
        let share = file.field_value(OCCURRENCE, 2, fixture::SHARE, 1).unwrap();
        assert_eq!(share.to_text(&file.cx()).into_string(), "1");
        let total = file
            .summary(&Context::record(OCCURRENCE, 2), summary, group)
            .unwrap();
        assert_eq!(total.to_text(&file.cx()).into_string(), "5");
        assert!(!cached.has_missing());
    }
    provider
        .change_found_set(tasks, FoundSetChange::Unsort)
        .unwrap();
    provider
        .records_with_calculations(tasks, 0, 1, &[])
        .unwrap();
    let unsorted = provider
        .calc_data(&[CalcNeed::FoundSet {
            occurrence: OCCURRENCE,
            table: TABLE,
        }])
        .unwrap();
    assert!(unsorted.sorts[&OCCURRENCE].is_empty());
    cache.borrow_mut().merge(unsorted);
    let cached = CachedSource::new(&cache);
    assert!(cached.found_sort(OCCURRENCE).unwrap().is_empty());
    let file = FileCalc::new(&schema, &cached, today);
    let running = file
        .field_value(OCCURRENCE, 2, fixture::RUNNING, 1)
        .unwrap();
    assert_eq!(running.to_text(&file.cx()).into_string(), "8");
    assert!(file
        .summary(&Context::record(OCCURRENCE, 2), summary, group)
        .unwrap()
        .is_empty());
    provider.close();
}

#[test]
fn hosted_metadata_retains_file_options_from_inline_values() {
    let file = file_with("metadata-parent", &[], |root| {
        let mut options = 0x403u32.to_be_bytes().to_vec();
        options.extend([0, 0]); // field order and direction
        options.extend([2, 1, 7]); // first-window script
        options.extend([0, 0]); // no last-window script or default layout
        root.put(&[&[2], &[0x18]], OwnedNode::leaf(Vec::new(), options));
    });
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema.file_options.as_ref().unwrap().first_window_open,
        Some(7)
    );
    provider.close();
}

/// `Get ( HostApplicationVersion )` is the version string the host gave
/// in its Connect reply (README F14: `Server 26.0.2` from FileMaker
/// Server), until the file closes.
#[test]
fn the_host_names_its_application_version() {
    let file = file("host-version", &[]);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    assert_eq!(
        provider.host_application_version().as_deref(),
        Some("OpenMaker Server test")
    );
    provider.close();
    assert_eq!(provider.host_application_version(), None);
}

/// Criteria on unstored calculations are computed on each candidate with
/// the find operators; criteria on summary and global fields are not
/// criteria (FileMaker Server's engine, script-runtime.md 13.19).
#[test]
fn finds_compute_unstored_calculations_and_skip_summaries_and_globals() {
    // Records 3-7 after the fixture's 1 "Write tests" (3) and 2 "Ship" (5);
    // record 6 has only a Due date, which Label does not read.
    let rows: Vec<(String, String, String)> = [
        ("Alpha", "1", ""),
        ("Beta", "2", ""),
        ("Alpha", "", ""),
        ("", "", "1/1/2026"),
        ("Beta", "2", ""),
    ]
    .iter()
    .map(|(n, c, d)| (n.to_string(), c.to_string(), d.to_string()))
    .collect();
    let file = file_with("unstored", &rows, fixture::add_calculated);
    // Include missing-index fallback when a stored criterion narrows the
    // candidates before calculation evaluation. Later finds use warm indexes.
    let (port, ca_pem) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    // Label = Name & " (" & Count & ")" is empty on record 6, where Name
    // and Count are ("do not evaluate if all referenced fields are empty").
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, 2, "Label"), Some("Ship (5)"));
    assert_eq!(text(&page, 5, "Label"), Some("Alpha ()"));
    assert_eq!(text(&page, 6, "Label"), None);
    let mut find = |criteria: &[(&str, &str)]| -> Result<Vec<i64>, Option<i32>> {
        let mut request = FindRequest::default();
        for (field, text) in criteria {
            request.set(field, *text);
        }
        provider
            .change_found_set(
                tasks,
                FoundSetChange::Find {
                    requests: vec![request],
                    kind: FindKind::Replace,
                },
            )
            .map_err(|e| e.code)?;
        Ok(ids_of(&all_records(&mut provider, tasks)))
    };
    assert_eq!(find(&[("Label", "ship")]), Ok(vec![2]));
    assert_eq!(find(&[("Label", "alpha")]), Ok(vec![3, 5]));
    assert_eq!(find(&[("Label", "==Alpha (1)")]), Ok(vec![3]));
    assert_eq!(find(&[("Label", "\"(2)\"")]), Ok(vec![4, 7]));
    assert_eq!(find(&[("Label", "=")]), Ok(vec![6]));
    assert_eq!(find(&[("Label", "!")]), Ok(vec![4, 7]));
    assert_eq!(find(&[("Label", "B*")]), Ok(vec![4, 7]));
    assert_eq!(find(&[("Label", "zeta")]), Err(Some(401)));
    // ANDed with a criterion the host's index answers.
    assert_eq!(
        find(&[("Name", "alpha"), ("Key", "==Alpha")]),
        Ok(vec![3, 5])
    );
    assert_eq!(find(&[("Count", "1"), ("Label", "alpha")]), Ok(vec![3]));
    // Summary and global fields are not criteria.
    assert_eq!(find(&[("Total", "15")]), Err(Some(400)));
    assert_eq!(find(&[("Setting", "x")]), Err(Some(400)));
    assert_eq!(find(&[("Name", "ship"), ("Total", "999")]), Ok(vec![2]));
    // Constrain within the found set.
    find(&[("Name", "alpha")]).unwrap();
    let mut request = FindRequest::default();
    request.set("Label", "(1)");
    provider
        .change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Constrain,
            },
        )
        .unwrap();
    assert_eq!(ids_of(&all_records(&mut provider, tasks)), vec![3]);
}

#[test]
fn a_transaction_commits_both_tables_together_or_nothing() {
    let file = file("transaction", &[]);
    let (port, ca_pem) = host(&file.0);
    let mut a = open(port, &ca_pem);
    let mut b = open(port, &ca_pem);
    let (tasks, notes) = (layout(&mut a, "Tasks"), layout(&mut a, "Notes"));
    let before = (all_records(&mut b, tasks), all_records(&mut b, notes));
    all_records(&mut a, tasks);

    // Commit: a change in Tasks, a new record in each table.
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 1, values(&[("Name", "Renamed")]))
        .unwrap();
    let task = a.create_record(tasks, values(&[("Name", "New")])).unwrap();
    let note = a.create_record(notes, values(&[("Body", "held")])).unwrap();
    a.update_record(tasks, task.id, values(&[("Count", "4")]))
        .unwrap();
    // The window sees the transaction's records; the host has none yet. A
    // new one shows what New Record entered, and its stored calculation
    // follows a later change.
    let seen = all_records(&mut a, tasks);
    assert_eq!(text(&seen, 1, "Name"), Some("Renamed"));
    assert_eq!(text(&seen, task.id, "Name"), Some("New"));
    assert_eq!(text(&seen, task.id, "Creator"), Some(ACCOUNT));
    assert_eq!(text(&seen, task.id, "Doubled"), Some("8"));
    assert_eq!(
        (all_records(&mut b, tasks), all_records(&mut b, notes)),
        before
    );
    // The changed record is locked by the transaction.
    let locked = b
        .update_record(tasks, 1, values(&[("Name", "Other")]))
        .unwrap_err();
    assert_eq!(locked.code, Some(301), "{locked}");
    a.commit_transaction(false).unwrap();
    let after = all_records(&mut b, tasks);
    assert_eq!(text(&after, 1, "Name"), Some("Renamed"));
    // Auto-enter ran at the commit.
    assert_eq!(text(&after, task.id, "Creator"), Some(ACCOUNT));
    assert_eq!(
        text(&all_records(&mut b, notes), note.id, "Body"),
        Some("held")
    );
    // The locks are released.
    b.update_record(tasks, 1, values(&[("Name", "Write tests")]))
        .unwrap();

    // A note that fails its validation (always) reverts the change to
    // Tasks too, and releases its lock.
    let before = (all_records(&mut b, tasks), all_records(&mut b, notes));
    all_records(&mut a, tasks);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 2, values(&[("Name", "Never")]))
        .unwrap();
    a.create_record(notes, values(&[("Body", "far too long for ten")]))
        .unwrap();
    let refused = a.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(507), "{refused}");
    assert_eq!(
        (all_records(&mut b, tasks), all_records(&mut b, notes)),
        before
    );
    b.update_record(tasks, 2, values(&[("Name", "Ship it")]))
        .unwrap();
    a.close();
    b.close();
}

/// A window's record session (#892): New Record holds the record under the
/// window's own transaction, as FileMaker Pro holds it under op71 until the
/// record is committed (write-path.md 1.1, 1.2). Another window's writes
/// are not held in it, a refused commit keeps it held, and Revert or the
/// window's close leaves nothing on the host, so another client's data is
/// never touched.
#[test]
fn a_windows_record_session_holds_its_new_record_apart_until_commit_or_revert() {
    let file = file("record-session", &[]);
    let (port, ca_pem) = host(&file.0);
    let mut a = open(port, &ca_pem);
    let mut b = open(port, &ca_pem);
    let (tasks, notes) = (layout(&mut a, "Tasks"), layout(&mut a, "Notes"));
    // The window's writes, as its New Record and commit make them: Body
    // typed by the user, so validated as data entry.
    let write = |record: Option<i64>, body: &str| om_data::ScriptWrite {
        layout: notes,
        table: None,
        record,
        cells: [(fixture::BODY, Some(body.to_owned()))].into(),
        fields: values(&[("Body", body)]),
        typed: [fixture::BODY].into(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };
    let before = ids_of(&all_records(&mut b, notes));
    all_records(&mut a, notes);

    // Window 1 holds a new note; window 2's edit is written at once.
    a.use_window(1);
    a.begin_record_commit().unwrap();
    let note = a.write_record(&write(None, "held")).unwrap();
    assert!(a.record_session_open());
    // Beside it, the window's direct saves are refused, not held.
    let refused = a
        .update_record(tasks, 1, values(&[("Name", "Held too")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused}");
    a.use_window(2);
    assert!(!a.record_session_open());
    a.update_record(tasks, 1, values(&[("Name", "Window two")]))
        .unwrap();
    assert_eq!(
        text(&all_records(&mut b, tasks), 1, "Name"),
        Some("Window two")
    );
    assert_eq!(ids_of(&all_records(&mut b, notes)), before);

    // Back in window 1: a refused commit keeps the record held.
    a.use_window(1);
    assert!(a.record_session_open());
    a.write_record(&write(Some(note.id), "far too long for ten"))
        .unwrap();
    let refused = a.commit_record_session(false).unwrap_err();
    assert_eq!(refused.code, Some(507), "{refused}");
    assert!(a.record_session_open());
    assert_eq!(ids_of(&all_records(&mut b, notes)), before);
    a.write_record(&write(Some(note.id), "kept")).unwrap();
    a.commit_record_session(false).unwrap();
    assert!(!a.record_session_open());
    assert_eq!(
        text(&all_records(&mut b, notes), note.id, "Body"),
        Some("kept")
    );

    // Revert, and a window closed while it holds one: nothing is written.
    let before = ids_of(&all_records(&mut b, notes));
    a.begin_record_commit().unwrap();
    a.write_record(&write(None, "reverted")).unwrap();
    a.revert_transaction().unwrap();
    a.begin_record_commit().unwrap();
    a.write_record(&write(None, "closed")).unwrap();
    a.use_window(2);
    a.drop_window(1);
    a.use_window(1);
    assert!(!a.record_session_open());
    assert_eq!(ids_of(&all_records(&mut b, notes)), before);

    // A script's transaction opened in window 2 sets window 1's session
    // aside: still held, not open for its writes, open again once the
    // script's ends. Reverted while aside, the script's stays open.
    a.begin_record_commit().unwrap();
    let held = a.write_record(&write(None, "held")).unwrap();
    a.use_window(2);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.use_window(1);
    assert!(!a.record_session_open());
    assert!(a.record_session_held());
    a.commit_transaction(false).unwrap();
    assert!(a.record_session_open());
    a.commit_record_session(false).unwrap();
    assert_eq!(
        text(&all_records(&mut b, notes), held.id, "Body"),
        Some("held")
    );
    let before = ids_of(&all_records(&mut b, notes));
    a.begin_record_commit().unwrap();
    a.write_record(&write(None, "dropped")).unwrap();
    a.use_window(2);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.use_window(1);
    a.revert_record_session().unwrap();
    assert!(!a.record_session_held());
    a.commit_transaction(false).unwrap();
    assert!(!a.record_session_open());
    assert_eq!(ids_of(&all_records(&mut b, notes)), before);
    // Window 1 closed while its session is set aside: that session ends,
    // and the script's transaction stays open and commits its own change.
    a.begin_record_commit().unwrap();
    a.write_record(&write(None, "closed aside")).unwrap();
    a.use_window(2);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 1, values(&[("Name", "In the script's")]))
        .unwrap();
    a.use_window(1);
    a.drop_window(1);
    assert!(!a.record_session_held());
    a.commit_transaction(false).unwrap();
    assert_eq!(ids_of(&all_records(&mut b, notes)), before);
    assert_eq!(
        text(&all_records(&mut b, tasks), 1, "Name"),
        Some("In the script's")
    );
    a.close();
    b.close();
}

#[test]
fn transaction_options_skip_auto_enter_and_data_entry_validation() {
    let file = file("options", &[]);
    let (port, ca_pem) = host(&file.0);
    let mut a = open(port, &ca_pem);
    let tasks = layout(&mut a, "Tasks");
    let code = |page: &RecordPage, id| text(page, id, "Code").map(str::to_owned);

    // Code accepts at most three characters, during data entry only.
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 1, values(&[("Code", "LONGER")]))
        .unwrap();
    assert_eq!(a.commit_transaction(false).unwrap_err().code, Some(507));
    assert_eq!(code(&all_records(&mut a, tasks), 1), None);
    // Skipped by Open Transaction's option, or by Commit Transaction's.
    a.begin_transaction(TransactionOptions {
        skip_validation: true,
        ..TransactionOptions::default()
    })
    .unwrap();
    a.update_record(tasks, 1, values(&[("Code", "LONGER")]))
        .unwrap();
    a.commit_transaction(false).unwrap();
    assert_eq!(
        code(&all_records(&mut a, tasks), 1).as_deref(),
        Some("LONGER")
    );
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 2, values(&[("Code", "LONGEST")]))
        .unwrap();
    a.commit_transaction(true).unwrap();
    assert_eq!(
        code(&all_records(&mut a, tasks), 2).as_deref(),
        Some("LONGEST")
    );

    // Skip auto-enter options: no creator.
    a.begin_transaction(TransactionOptions {
        skip_auto_enter: true,
        ..TransactionOptions::default()
    })
    .unwrap();
    let plain = a
        .create_record(tasks, values(&[("Name", "Plain")]))
        .unwrap();
    a.commit_transaction(false).unwrap();
    assert_eq!(text(&all_records(&mut a, tasks), plain.id, "Creator"), None);

    // Revert: nothing is written, a reserved id stays used, and without a
    // transaction there is nothing to commit or revert (3).
    let before = all_records(&mut a, tasks);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    let ghost = a
        .create_record(tasks, values(&[("Name", "Ghost")]))
        .unwrap();
    a.update_record(tasks, 1, values(&[("Name", "Gone")]))
        .unwrap();
    a.delete_record(tasks, 2).unwrap();
    assert_eq!(
        a.begin_transaction(TransactionOptions::default())
            .unwrap_err()
            .code,
        Some(3)
    );
    a.revert_transaction().unwrap();
    assert_eq!(all_records(&mut a, tasks), before);
    let next = a.create_record(tasks, values(&[("Name", "Next")])).unwrap();
    assert!(next.id > ghost.id, "{} after {}", next.id, ghost.id);
    assert_eq!(a.commit_transaction(false).unwrap_err().code, Some(3));
    assert_eq!(a.revert_transaction().unwrap_err().code, Some(3));
    a.close();
}

/// A related-values list ("Include only related values starting from"):
/// Code must be a Name of the records related to the record through S, a
/// self-join of Tasks on Count, as the write leaves the record. The record
/// is among its own related records with its unsaved values, as a related
/// list FileMaker Pro shows was seen to have it (file-metadata.md 6.4, F91);
/// a new record's own place is unmeasured, so a new record related to
/// itself is refused (3) rather than checked. Num, a number, must be one of
/// S::Count, compared by value; Mark one of S::Shout, an unstored
/// calculation computed on each related record (#1058), the written
/// record's over the values the write gives it; Owner one of S::Who,
/// `Get ( AccountName )`, this session's account. While a transaction holds
/// a record of another table, which such a calculation could read only as
/// stored, Commit Transaction refuses (3).
#[test]
fn a_related_value_list_is_computed_for_the_written_record() {
    const SELF_JOIN: u64 = 0xd0_0009;
    const SHOUT: u64 = 33;
    const MARK: u64 = 34;
    const NUM: u64 = 35;
    const WHO: u64 = 36;
    const OWNER: u64 = 37;
    let mut root = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_field(TABLE, SHOUT, "Shout");
    fixture::add_field(&mut root, TABLE, SHOUT, "Shout", 2, 1, 0, 0x2_0000, 0);
    let shout = "Name & \"!\"";
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), SHOUT, 5, shout);
    fixture::add_field(&mut root, TABLE, MARK, "Mark", 1, 1, 0, 0, 0x100);
    fixture::add_field(&mut root, TABLE, NUM, "Num", 1, 2, 0, 0, 0x100);
    fixture::add_field(&mut root, TABLE, WHO, "Who", 2, 1, 0, 0x2_0000, 0);
    let who = "Get ( AccountName )";
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), WHO, 5, who);
    fixture::add_field(&mut root, TABLE, OWNER, "Owner", 1, 1, 0, 0, 0x100);
    fixture::add_occurrence(&mut root, SELF_JOIN, "S", TABLE);
    fixture::add_relationship(&mut root, 12, (OCCURRENCE, COUNT), 0, (SELF_JOIN, COUNT));
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x100);
    let (t, code) = (uint_segment(TABLE), uint_segment(CODE));
    let reference = vec![2, 1, 6, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff];
    root.put(
        &[&t, &[3], &[5], &code, &[0x07]],
        OwnedNode::leaf(Vec::new(), reference),
    );
    root.put(&[&[0x21], &[5], &[6], &[0x10]], text_node(&[], "Related"));
    // Kind 1, "Include only related values starting from" (0x0001): S::Name,
    // no second field, starting from Tasks.
    let related = |field: u8| {
        let mut list = vec![1, 0, 1, 4, 3, 0xd0, 0, 9, 2, 1, field, 0, 1, 0, 0, 0, 0];
        list.extend([4, 3, 0xd0, 0, 1, 0, 0]);
        list.extend(om_scsu::encode_counted(" "));
        OwnedNode::leaf(Vec::new(), list)
    };
    root.put(&[&[0x21], &[5], &[6], &[2]], related(NAME as u8));
    for (list, name, listed, validated) in [
        (7u8, "Shouts", SHOUT, MARK),
        (8, "Counts", COUNT, NUM),
        (9, "Accounts", WHO, OWNER),
    ] {
        root.put(&[&[0x21], &[5], &[list], &[0x10]], text_node(&[], name));
        root.put(&[&[0x21], &[5], &[list], &[2]], related(listed as u8));
        root.put(
            &[&t, &[3], &[5], &uint_segment(validated), &[0x07]],
            OwnedNode::leaf(
                Vec::new(),
                vec![2, 1, list, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
            ),
        );
    }
    let file = TempFile::from_tree("related-list", &root);
    let (port, ca_pem) = host(&file.0);
    let mut a = open(port, &ca_pem);
    let tasks = layout(&mut a, "Tasks");
    let code = |result: Result<Record, DataError>| result.map(|_| ()).map_err(|e| e.code);
    // Record 2 ("Ship", Count 5) relates to itself only.
    assert_eq!(
        code(a.update_record(tasks, 2, values(&[("Code", "ship")]))),
        Ok(())
    );
    assert_eq!(
        code(a.update_record(tasks, 2, values(&[("Code", "Write tests")]))),
        Err(Some(506))
    );
    // With Count 3 as written, record 1 ("Write tests", Count 3) is related.
    assert_eq!(
        code(a.update_record(tasks, 2, values(&[("Count", "3"), ("Code", "Write tests")]))),
        Ok(())
    );
    // Record 1's Count is 3: "03" is one of S::Count by value.
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Num", "03")]))),
        Ok(())
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Num", "4")]))),
        Err(Some(506))
    );
    // Records 1 and 2 (Count 3 since) relate to record 1: Shout is computed
    // on each, record 1's from the name the write gives it.
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Mark", "Ship!")]))),
        Ok(())
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Mark", "Nope!")]))),
        Err(Some(506))
    );
    assert_eq!(
        code(a.update_record(
            tasks,
            1,
            values(&[("Name", "Renamed"), ("Mark", "Renamed!")])
        )),
        Ok(())
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Mark", "Write tests!")]))),
        Err(Some(506))
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Owner", ACCOUNT)]))),
        Ok(())
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Owner", "Nobody")]))),
        Err(Some(506))
    );
    // In a transaction (#1063): a held record of a table the list does not
    // read (Notes) leaves it checked; one it reads that the commit computes
    // after record 1 (record 2, related by Count) refuses it.
    a.begin_transaction(TransactionOptions::default()).unwrap();
    let notes = layout(&mut a, "Notes");
    a.create_record(notes, values(&[("Body", "Held")])).unwrap();
    a.update_record(tasks, 1, values(&[("Mark", "Ship!")]))
        .unwrap();
    a.commit_transaction(false).unwrap();
    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(tasks, 1, values(&[("Mark", "Renamed!")]))
        .unwrap();
    a.update_record(tasks, 2, values(&[("Name", "Later")]))
        .unwrap();
    let refused = a.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("transaction holds"), "{refused:?}");
    // A row imported in the transaction, then typed into (#1074): its list
    // is not computed over what the import left, so the commit refuses (3)
    // rather than take the typed value unchecked.
    a.begin_transaction(TransactionOptions::default()).unwrap();
    let cells = [(NAME, Some("Imported".to_owned()))].into();
    let row = a.import_record(tasks, None, &cells, true).unwrap();
    a.write_record(&om_data::ScriptWrite {
        layout: tasks,
        record: Some(row),
        cells: [(MARK, Some("Nope!".to_owned()))].into(),
        typed: [MARK].into(),
        ..Default::default()
    })
    .unwrap();
    let refused = a.commit_transaction(false).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("not named"), "{refused:?}");
    // A new record of Count 3 relates to itself: not checked, refused.
    let refused = a
        .create_record(tasks, values(&[("Count", "3"), ("Code", "Write tests")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("new record"), "{refused:?}");
    let page = all_records(&mut a, tasks);
    assert_eq!(text(&page, 2, "Code"), Some("Write tests"));
    a.close();
}

#[test]
fn existing_values_and_value_lists_of_a_field_are_checked_on_the_host() {
    // Name: an existing value, validated Always, value-indexed; Code: a
    // member of value list 5, the values of Name (script-runtime.md
    // 13.21).
    let mut root = fixture::tree(&[], true);
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    fixture::set_field_validation(&mut root, TABLE, NAME, 0x420);
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x500);
    let (t, code) = (uint_segment(TABLE), uint_segment(CODE));
    let reference = vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff];
    root.put(
        &[&t, &[3], &[5], &code, &[0x07]],
        OwnedNode::leaf(Vec::new(), reference),
    );
    root.put(&[&[0x21], &[5], &[5], &[0x10]], text_node(&[], "Names"));
    // Kind 1: Tasks::Name (occurrence d0 00 01, field 1), no second field.
    let mut list = vec![
        1, 0, 0, 4, 3, 0xd0, 0, 1, 2, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ];
    list.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
    let file = TempFile::from_tree("lookups", &root);
    let (port, ca_pem) = host(&file.0);
    let mut a = open(port, &ca_pem);
    let tasks = layout(&mut a, "Tasks");
    let code = |result: Result<Record, DataError>| result.map(|_| ()).map_err(|e| e.code);
    // Another record holds "Ship"; nobody holds "Zed".
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Name", "Zed")]))),
        Err(Some(505))
    );
    assert_eq!(
        code(a.update_record(tasks, 1, values(&[("Name", "ship")]))),
        Ok(())
    );
    // Code must be a Name: "Ship" is, "Zed" is not.
    assert_eq!(
        code(a.update_record(tasks, 2, values(&[("Code", "Zed")]))),
        Err(Some(506))
    );
    assert_eq!(
        code(a.update_record(tasks, 2, values(&[("Code", "SHIP")]))),
        Ok(())
    );
    let page = all_records(&mut a, tasks);
    assert_eq!(text(&page, 1, "Name"), Some("ship"));
    assert_eq!(text(&page, 2, "Code"), Some("SHIP"));
    a.close();
}

/// A field-backed list on Code, with no related-value filter or second
/// display field. This exercises ExistsInIndex, not CreateValueListEx.
fn validation_list(root: &mut OwnedNode, occurrence: u64, field: u64) {
    fixture::set_field_validation(root, TABLE, CODE, 0x500);
    root.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(CODE), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    root.put(&[&[0x21], &[5], &[5], &[0x10]], text_node(&[], "Allowed"));
    let key = |id| {
        let bytes = uint_segment(id);
        [vec![bytes.len() as u8 + 1, bytes.len() as u8], bytes].concat()
    };
    let mut definition = vec![1, 0, 0];
    definition.extend(key(occurrence));
    definition.extend(key(field));
    definition.extend([0, 1, 0, 0, 0, 0, 0, 0, 0]);
    definition.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
}

#[test]
fn missing_index_unique_cold_and_warm_preserve_duplicates_exclusion_and_new_values() {
    for warm in [false, true] {
        for (candidate, duplicate) in [("SHIP", true), ("WRITE TESTS", false), ("New", false)] {
            let mut root = fixture::tree(&[], true);
            fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
            fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
            root.put(
                &[&uint_segment(TABLE), &[5], &[2], &[1]],
                text_node(&[], "Ship\rShip"),
            );
            let file = TempFile::from_tree("missing-unique", &root);
            let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
            let mut provider = open(port, &ca);
            let tasks = layout(&mut provider, "Tasks");
            if warm {
                build_indexes(&mut provider, &[(TABLE, NAME)]);
            }
            let before = all_records(&mut provider, tasks);
            let result = provider.update_record(tasks, 1, values(&[("Name", candidate)]));
            assert_eq!(
                result.as_ref().err().and_then(|e| e.code),
                duplicate.then_some(504),
                "warm={warm}, candidate={candidate:?}: {result:?}"
            );
            let after = all_records(&mut provider, tasks);
            if duplicate {
                assert_eq!(after, before, "refusal must not change stored values");
            } else {
                result.unwrap();
                assert_eq!(text(&after, 1, "Name"), Some(candidate));
            }
            // A refusal releases the record and index locks.
            let mut other = open(port, &ca);
            other
                .update_record(tasks, 1, values(&[("Name", "Released")]))
                .unwrap();
            other.close();
            provider.close();
        }
    }
}

#[test]
fn missing_index_existing_cold_and_warm_require_another_record() {
    for warm in [false, true] {
        for (candidate, accepted) in [
            ("SHIP", true),
            ("Write tests", false),
            ("New", false),
            ("", true),
        ] {
            let mut root = fixture::tree(&[], true);
            fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
            fixture::set_field_validation(&mut root, TABLE, NAME, 0x420);
            let file = TempFile::from_tree("missing-existing", &root);
            let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
            let mut provider = open(port, &ca);
            let tasks = layout(&mut provider, "Tasks");
            if warm {
                build_indexes(&mut provider, &[(TABLE, NAME)]);
            }
            let before = all_records(&mut provider, tasks);
            // Change capitalization to ensure the excluded-record case
            // runs a write rather than the unchanged-values shortcut.
            let candidate = if candidate == "Write tests" {
                "WRITE TESTS"
            } else {
                candidate
            };
            let result = provider.update_record(tasks, 1, values(&[("Name", candidate)]));
            assert_eq!(
                result.as_ref().err().and_then(|e| e.code),
                (!accepted).then_some(505),
                "warm={warm}, candidate={candidate:?}: {result:?}"
            );
            if accepted {
                result.unwrap();
            } else {
                assert_eq!(all_records(&mut provider, tasks), before);
            }
            provider.close();
        }
    }
}

#[test]
fn missing_index_field_list_cold_and_warm_read_lines_and_all_repetitions() {
    for warm in [false, true] {
        for (candidate, accepted) in [
            ("CAFE", true),
            ("second", true),
            (" padded ", true),
            ("padded", false),
            ("missing", false),
            ("", true),
        ] {
            let mut root = fixture::tree(&[], true);
            validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
            fixture::set_field_options(&mut root, fixture::NOTES, fixture::BODY, 0x8000_0000);
            fixture::set_field_repetitions(&mut root, fixture::NOTES, fixture::BODY, 2);
            for (cell, value) in [
                (fixture::BODY, "Café\r padded "),
                (fixture::BODY * 256 + 2, "second\nsecond"),
            ] {
                root.put(
                    &[
                        &uint_segment(fixture::NOTES),
                        &[5],
                        &[1],
                        &uint_segment(cell),
                    ],
                    text_node(&[], value),
                );
            }
            let file = TempFile::from_tree("missing-list", &root);
            let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
            let mut provider = open(port, &ca);
            let tasks = layout(&mut provider, "Tasks");
            if warm {
                build_indexes(&mut provider, &[(fixture::NOTES, fixture::BODY)]);
            }
            let before = all_records(&mut provider, tasks);
            let result = provider.update_record(tasks, 1, values(&[("Code", candidate)]));
            assert_eq!(
                result.as_ref().err().and_then(|e| e.code),
                (!accepted).then_some(506),
                "warm={warm}, candidate={candidate:?}: {result:?}"
            );
            if accepted {
                result.unwrap();
            } else {
                assert_eq!(all_records(&mut provider, tasks), before);
            }
            provider.close();
        }
    }
}

#[test]
fn missing_index_transaction_checks_host_and_pending_duplicates_before_any_upload() {
    for host_duplicate in [true, false] {
        let mut root = fixture::tree(&[], true);
        fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
        fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
        let file = TempFile::from_tree("missing-transaction", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let before = all_records(&mut provider, tasks);
        provider
            .begin_transaction(TransactionOptions::default())
            .unwrap();
        let candidate = if host_duplicate { "Ship" } else { "New" };
        if !host_duplicate {
            provider
                .create_record(tasks, values(&[("Name", candidate)]))
                .unwrap();
        }
        provider
            .create_record(tasks, values(&[("Name", candidate)]))
            .unwrap();
        let refused = provider.commit_transaction(false).unwrap_err();
        assert_eq!(refused.code, Some(504), "{refused}");
        assert_eq!(all_records(&mut provider, tasks), before);
        let mut other = open(port, &ca);
        other
            .update_record(tasks, 1, values(&[("Name", "Unlocked")]))
            .unwrap();
        other.close();
        provider.close();
    }
}

/// The warm control is a literal native index, not one generated by the
/// same stored-key helper as the fallback (query-program.md 7.2).
#[test]
fn missing_index_validation_agrees_with_literal_native_keys() {
    for indexed in [false, true] {
        for (candidate, accepted) in [
            ("a", true),
            ("Á", true),
            ("b", true),
            (" a ", false),
            ("\u{301}", false),
        ] {
            let mut root = fixture::tree(&[], true);
            validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
            fixture::set_field_options(&mut root, fixture::NOTES, fixture::BODY, 0x8000_0000);
            fixture::set_field_repetitions(&mut root, fixture::NOTES, fixture::BODY, 2);
            let table = uint_segment(fixture::NOTES);
            root.put(&[&table, &[5], &[1], &[1]], text_node(&[], "A"));
            root.put(
                &[&table, &[5], &[1], &uint_segment(258)],
                text_node(&[], "B"),
            );
            if indexed {
                for key in [vec![1, 0x12, 0x0f], vec![1, 0x12, 0x25]] {
                    root.put(
                        &[&table, &[1], &[1], &[0], &key],
                        RidList::from_iter([1]).to_node(Vec::new()),
                    );
                }
                root.put(
                    &[&table, &[1], &[1], &[0x10]],
                    OwnedNode::leaf(
                        Vec::new(),
                        [1u64.to_be_bytes(), 2u64.to_be_bytes()].concat(),
                    ),
                );
            }
            let file = TempFile::from_tree("missing-literal", &root);
            let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
            let mut provider = open(port, &ca);
            let tasks = layout(&mut provider, "Tasks");
            let result = provider.update_record(tasks, 1, values(&[("Code", candidate)]));
            assert_eq!(
                result.is_ok(),
                accepted,
                "indexed={indexed}, {candidate:?}: {result:?}"
            );
            if !accepted {
                assert_eq!(result.unwrap_err().code, Some(506));
            }
            provider.close();
        }
    }
}

#[test]
fn missing_index_never_indexed_text_is_scanned_but_unknown_formats_are_explicit() {
    // What "a" and then "A" get against a source holding "A": accepted
    // (None) or refused with a code.
    const ACCEPTED: [Option<i32>; 2] = [None, None];
    const UNSUPPORTED: [Option<i32>; 2] = [Some(3), Some(3)];
    for (data_type, language, options, expected) in [
        (1, 21, 0, ACCEPTED),
        (1, 77, 0, ACCEPTED),
        // Unicode raw keys keep case (query-program.md 7.2).
        (1, 2, 0, [Some(506), None]),
        (1, 22, 0, UNSUPPORTED),
        (1, 0, 0, UNSUPPORTED),
        (2, 21, 0, UNSUPPORTED),
        (3, 21, 0, UNSUPPORTED),
        (4, 21, 0, UNSUPPORTED),
        (5, 21, 0, UNSUPPORTED),
        (6, 21, 0, UNSUPPORTED),
        (1, 21, 0x10000, UNSUPPORTED), // Globals are not record index values.
    ] {
        let mut root = fixture::tree(&[], true);
        validation_list(&mut root, fixture::NOTES_OCCURRENCE, 20);
        fixture::add_field(
            &mut root,
            fixture::NOTES,
            20,
            "Source",
            1,
            data_type,
            0,
            options | 0x4000_0000, // Prohibit automatic index creation.
            0,
        );
        fixture::set_field_language(&mut root, fixture::NOTES, 20, language);
        root.put(
            &[&uint_segment(fixture::NOTES), &[5], &[1], &[20]],
            text_node(&[], "A"),
        );
        let file = TempFile::from_tree("missing-formats", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let before = all_records(&mut provider, tasks);
        // A never-indexed text field continues to use the scan on each
        // attempt; unsupported keys are not reported as NOT_IN_LIST.
        for (candidate, expected) in ["a", "A"].into_iter().zip(expected) {
            let result = provider.update_record(tasks, 1, values(&[("Code", candidate)]));
            let Some(code) = expected else {
                result.unwrap();
                continue;
            };
            let error = result.unwrap_err();
            assert_eq!(
                error.code,
                Some(code),
                "type={data_type}, language={language}, {candidate:?}: {error}"
            );
            if code == 3 {
                assert!(
                    error.to_string().contains("without the host's value index"),
                    "{error}"
                );
            }
            assert_eq!(all_records(&mut provider, tasks), before);
        }
        provider.close();
    }
}

#[test]
fn missing_index_new_unique_record_is_not_committed_on_refusal() {
    let mut root = fixture::tree(&[], true);
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
    let file = TempFile::from_tree("missing-new", &root);
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let before = all_records(&mut provider, tasks);
    let refused = provider
        .create_record(tasks, values(&[("Name", "SHIP")]))
        .unwrap_err();
    assert!(refused.to_string().contains("unique value"), "{refused}");
    assert_eq!(all_records(&mut provider, tasks), before);
    let new = provider
        .create_record(tasks, values(&[("Name", "New")]))
        .unwrap();
    let after = all_records(&mut provider, tasks);
    assert_eq!(after.records.len(), before.records.len() + 1);
    assert_eq!(text(&after, new.id, "Name"), Some("New"));
    provider.close();
}

#[test]
fn missing_index_empty_stored_values_match_the_literal_empty_text_key() {
    for indexed in [false, true] {
        let mut root = fixture::tree(&[], true);
        validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
        let table = uint_segment(fixture::NOTES);
        root.remove(&[&table, &[5], &[1], &[1]]);
        if indexed {
            root.put(
                &[&table, &[1], &[1], &[0], &[1]],
                RidList::from_iter([1]).to_node(Vec::new()),
            );
            root.put(
                &[&table, &[1], &[1], &[0x10]],
                OwnedNode::leaf(
                    Vec::new(),
                    [1u64.to_be_bytes(), 1u64.to_be_bytes()].concat(),
                ),
            );
        }
        let file = TempFile::from_tree("missing-empty-key", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        // Nonempty text with no primary weights asks op18 about the
        // native empty-text key 01, without the empty-input skip path.
        provider
            .update_record(tasks, 1, values(&[("Code", "\u{301}")]))
            .unwrap();
        provider.close();
    }
}

#[test]
fn missing_index_validation_preserves_script_import_and_skip_options() {
    use om_data::ScriptWrite;
    for path in ["script", "import", "transaction"] {
        for always in [false, true] {
            for skip in [false, true] {
                let mut root = fixture::tree(&[], true);
                fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
                fixture::set_field_validation(
                    &mut root,
                    TABLE,
                    NAME,
                    0x10 | if always { 0x400 } else { 0 },
                );
                let file = TempFile::from_tree("missing-entry", &root);
                let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
                let mut provider = open(port, &ca);
                let tasks = layout(&mut provider, "Tasks");
                let before = all_records(&mut provider, tasks);
                let cells = BTreeMap::from([(NAME, Some("SHIP".to_owned()))]);
                let result = match path {
                    "script" => provider
                        .write_record(&ScriptWrite {
                            layout: tasks,
                            table: None,
                            record: Some(1),
                            cells,
                            fields: FieldChanges::new(),
                            typed: Default::default(),
                            skip_validation: skip,
                            privilege_set: None,
                            record_number: None,
                            prepared: None,
                        })
                        .map(|_| ()),
                    "import" => provider
                        .import_record(tasks, Some(1), &cells, false)
                        .map(|_| ()),
                    _ => {
                        provider
                            .begin_transaction(TransactionOptions {
                                skip_validation: skip,
                                ..TransactionOptions::default()
                            })
                            .unwrap();
                        provider
                            .update_record(tasks, 1, values(&[("Name", "SHIP")]))
                            .unwrap();
                        provider.commit_transaction(false)
                    }
                };
                let accepted = !always && (skip || path == "import");
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "path={path}, always={always}, skip={skip}: {result:?}"
                );
                if !accepted {
                    assert!(result.unwrap_err().to_string().contains("unique value"));
                    assert_eq!(all_records(&mut provider, tasks), before);
                }
                provider.close();
            }
        }
    }
}

#[test]
fn missing_index_scan_spans_batches_then_uses_the_warm_index_without_scanning() {
    let mut root = fixture::tree(&[], true);
    validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
    fixture::set_field_options(&mut root, fixture::NOTES, fixture::BODY, 0x8000_0000);
    let table = uint_segment(fixture::NOTES);
    for id in 1..=260 {
        root.put(
            &[&table, &[5], &serial::encode(id), &[1]],
            text_node(&[], if id == 260 { "Café" } else { "Other" }),
        );
    }
    root.put(
        &[&table, &[0x0d]],
        RidList::from_iter(1..=260).to_node(Vec::new()),
    );
    let file = TempFile::from_tree("missing-batches", &root);
    let recordings = record_dir("missing-batches");
    let (port, ca) = host_with(
        &file.0,
        Some(recordings.clone()),
        Server::with_late_index_builds,
    );
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    provider
        .update_record(tasks, 1, values(&[("Code", "CAFE")]))
        .unwrap();
    let cold = row_downloads(&recordings);
    assert_eq!(cold[&fixture::NOTES], 260);
    provider
        .update_record(tasks, 1, values(&[("Code", "cafe")]))
        .unwrap();
    assert_eq!(
        row_downloads(&recordings).get(&fixture::NOTES),
        cold.get(&fixture::NOTES)
    );
    provider.close();
    std::fs::remove_dir_all(recordings).unwrap();
}

#[test]
fn missing_index_scan_limits_and_unreadable_cells_refuse_without_changes() {
    for over_limit in [true, false] {
        let mut root = fixture::tree(&[], true);
        validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
        let table = uint_segment(fixture::NOTES);
        if over_limit {
            root.put(
                &[&table, &[0x0d]],
                RidList::from_iter(1..=100_001).to_node(Vec::new()),
            );
        } else {
            root.put(
                &[&table, &[5], &[1], &[1]],
                OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![99], vec![1])]),
            );
        }
        let file = TempFile::from_tree("missing-refusal", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let before = all_records(&mut provider, tasks);
        let error = provider
            .update_record(tasks, 1, values(&[("Code", "Missing")]))
            .unwrap_err();
        assert_eq!(error.code, Some(3), "{error}");
        assert!(
            error.to_string().contains(if over_limit {
                "scan limit"
            } else {
                "readable text"
            }),
            "{error}"
        );
        assert_eq!(all_records(&mut provider, tasks), before);
        provider.close();
    }
}

/// Count auto-enters a serial number, by 5: each new record takes the
/// host's next value (op72, write-path.md 3.2), whichever client creates
/// it.
#[test]
fn index_values_and_next_serial_values_reach_the_host() {
    let file = file_with("index", &[], |root| {
        fixture::set_field_options(root, TABLE, COUNT, 0x2);
        fixture::set_field_serial_increment(root, TABLE, COUNT, 5);
    });
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = layout(&mut provider, "Tasks");
    let first = provider
        .create_record(tasks, values(&[("Name", "Äpfel")]))
        .unwrap();
    let second = provider
        .create_record(tasks, values(&[("Name", "Ship")]))
        .unwrap();
    // A field without a next value starts at 1.
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, first.id, "Count"), Some("1"));
    assert_eq!(text(&page, second.id, "Count"), Some("6"));
    // Insert from Index: the distinct values in index order.
    assert_eq!(
        provider.index_values(tasks, "Name").unwrap(),
        ["Äpfel", "Ship", "Write tests"]
    );
    assert_eq!(
        provider.index_values(tasks, "Nowhere").unwrap_err().code,
        Some(102)
    );
    // A value list's field values: the host's whole list from one
    // CreateValueListEx (8.7); a field it does not know gives none.
    assert_eq!(
        provider.host_field_values(TABLE, NAME).unwrap(),
        Some(["Äpfel", "Ship", "Write tests"].map(String::from).into())
    );
    assert_eq!(provider.host_field_values(TABLE, 0x7fff).unwrap(), None);
    // Set Next Serial Value: op72 replaces the field's next value.
    provider
        .set_next_serial_value(OCCURRENCE, COUNT, "INV0100")
        .unwrap();
    let mut session = raw_session(port);
    let table = uint_segment(TABLE);
    let field = uint_segment(COUNT);
    let read = session.get_serial(&table, &field, "", 0, 0).unwrap();
    assert_eq!((read.status, read.value.as_str()), (0, "INV0100"));
    // Another client's new record continues the sequence.
    let mut other = open(port, &ca_pem);
    let other_tasks = layout(&mut other, "Tasks");
    let third = other
        .create_record(other_tasks, values(&[("Name", "Third")]))
        .unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, third.id, "Count"), Some("INV0100"));
    // Inside a transaction the new record takes its serial at New Record,
    // as FileMaker Pro's op72 follows op71: shown before the commit, and
    // stored by it.
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    let held = provider
        .create_record(tasks, values(&[("Name", "Held")]))
        .unwrap();
    let shown = all_records(&mut provider, tasks);
    assert_eq!(text(&shown, held.id, "Count"), Some("INV0105"));
    provider.commit_transaction(false).unwrap();
    let page = all_records(&mut other, other_tasks);
    assert_eq!(text(&page, held.id, "Count"), Some("INV0105"));
    let read = session.get_serial(&table, &field, "", 0, 0).unwrap();
    assert_eq!((read.status, read.value.as_str()), (0, "INV0110"));
    other.close();
    session.disconnect().unwrap();
    provider.close();
}

/// Import Records' writes (`DataProvider::import_record`, the import
/// review's C6): any normal field of any type, one whose auto-entered value
/// prohibits modification too; "only during data entry" validation not
/// checked; auto-enter only when asked; inside a transaction held with
/// those rules and written so by Commit Transaction. A user's typing still
/// writes only the fields it may edit.
#[test]
fn imports_write_every_field_type_with_the_imports_rules() {
    use fixture::CREATOR;
    use std::collections::BTreeMap;
    let file = file("provider-imports", &[]);
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let cells = |pairs: &[(u64, &str)]| -> BTreeMap<u64, Option<String>> {
        pairs
            .iter()
            .map(|(cell, text)| (*cell, (!text.is_empty()).then(|| (*text).to_owned())))
            .collect()
    };
    let row = cells(&[
        (NAME, "Imported"),
        (COUNT, "7"),
        (DUE, "1/2/2026"),
        (CREATOR, "Alice"),
        (CODE, "too long"),
    ]);
    let id = provider.import_record(tasks, None, &row, true).unwrap();
    let page = all_records(&mut provider, tasks);
    for (field, want) in [
        ("Count", "7"),
        ("Due", "1/2/2026"),
        ("Creator", "Alice"),
        ("Code", "too long"),
    ] {
        assert_eq!(text(&page, id, field), Some(want), "{field}");
    }
    // The stored calculation follows the imported number.
    assert_eq!(text(&page, id, "Doubled"), Some("14"));
    // Auto-enter off: no creator; on, for a row without one: the account.
    let plain = provider
        .import_record(tasks, None, &cells(&[(NAME, "Plain")]), false)
        .unwrap();
    let entered = provider
        .import_record(tasks, None, &cells(&[(NAME, "Entered")]), true)
        .unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, plain, "Creator"), None);
    assert_eq!(text(&page, entered, "Creator"), Some(ACCOUNT));
    // An update writes a number field.
    provider
        .import_record(tasks, Some(id), &cells(&[(COUNT, "8")]), true)
        .unwrap();
    assert_eq!(
        text(&all_records(&mut provider, tasks), id, "Count"),
        Some("8")
    );
    // A user types into a number field (#185): the count is stored as
    // typed and the stored calculation follows it. A date is stored as
    // data entry reads it, its two-digit year completed; text that is not
    // a date is refused (500) and nothing is written.
    let typed = provider
        .create_record(tasks, values(&[("Name", "Typed"), ("Count", "3")]))
        .unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, typed.id, "Count"), Some("3"));
    assert_eq!(text(&page, typed.id, "Doubled"), Some("6"));
    let dated = provider
        .create_record(tasks, values(&[("Name", "Dated"), ("Due", "1/2/26")]))
        .unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, dated.id, "Due"), Some("1/2/2026"));
    let total = page.total;
    let refused = provider
        .create_record(tasks, values(&[("Name", "Undated"), ("Due", "abc")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(500));
    assert_eq!(all_records(&mut provider, tasks).total, total);
    // The same typing in a record a script writes (`ScriptWrite::typed`: a
    // portal row's, or a record a script commits) is stored and refused
    // the same way, not as a script's text.
    let script_typed = |text: &str| om_data::ScriptWrite {
        layout: tasks,
        table: None,
        record: Some(dated.id),
        cells: [(DUE, Some(text.to_owned()))].into(),
        fields: FieldChanges::new(),
        typed: [DUE].into(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };
    provider.write_record(&script_typed("3/4/26")).unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, dated.id, "Due"), Some("3/4/2026"));
    let refused = provider.write_record(&script_typed("abc")).unwrap_err();
    assert_eq!(refused.code, Some(500));
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, dated.id, "Due"), Some("3/4/2026"));

    // Inside a transaction: held with the import's rules (shown before
    // the commit), written so by Commit Transaction.
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    let held = provider
        .import_record(
            tasks,
            None,
            &cells(&[(NAME, "Held"), (CODE, "too long")]),
            false,
        )
        .unwrap();
    assert_eq!(
        text(&all_records(&mut provider, tasks), held, "Name"),
        Some("Held")
    );
    provider.commit_transaction(false).unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, held, "Code"), Some("too long"));
    assert_eq!(text(&page, held, "Creator"), None);
}

/// A script's writes (`DataProvider::write_record`, script-runtime.md
/// 13.20, 13.21): any normal field, a number field and one whose
/// auto-entered value prohibits modification included, with Set Field's
/// own entry checks (`check_entry`: maximum characters 511 for a field
/// validated Always); inside a transaction held and planned with a
/// script's rules at Commit Transaction. A user's typing of the same
/// values is refused (201).
#[test]
fn script_writes_take_a_scripts_rules_in_and_out_of_transactions() {
    use fixture::CREATOR;
    use om_data::ScriptWrite;
    let file = file_with("provider-script-writes", &[], |root| {
        // Doubled is a stored calculation; Due, at most 4 characters,
        // validated Always; Name at most 6, only during data entry.
        fixture::set_field_validation(root, TABLE, DUE, 0x600);
        fixture::set_field_byte_limit(root, TABLE, DUE, 4);
        fixture::set_field_validation(root, TABLE, NAME, 0x200);
        fixture::set_field_byte_limit(root, TABLE, NAME, 6);
    });
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let write = |record: Option<i64>, pairs: &[(u64, &str)]| ScriptWrite {
        layout: tasks,
        table: None,
        record,
        cells: pairs
            .iter()
            .map(|(cell, text)| (*cell, (!text.is_empty()).then(|| (*text).to_owned())))
            .collect(),
        fields: FieldChanges::new(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    };
    // A user may not type into Count or Creator.
    let typed = provider
        .update_record(tasks, 1, values(&[("Count", "4"), ("Creator", "Ann")]))
        .unwrap_err();
    assert_eq!(typed.code, Some(201));
    provider
        .write_record(&write(Some(1), &[(COUNT, "4"), (CREATOR, "Script")]))
        .unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, 1, "Count"), Some("4"));
    assert_eq!(text(&page, 1, "Creator"), Some("Script"));
    assert_eq!(text(&page, 1, "Doubled"), Some("8"));
    // Set Field's own check: Due takes at most 4 characters.
    let long = provider.check_entry(TABLE, DUE, "10/1/2026").unwrap_err();
    assert_eq!(long.code, Some(511));
    provider.check_entry(TABLE, DUE, "1/1").unwrap();
    // The user's typing among a script's cells keeps a user's rules
    // (`ScriptWrite::typed`): Name takes a script's longer value, not the
    // user's.
    let mut typed = write(Some(1), &[(NAME, "Typed too long"), (COUNT, "5")]);
    typed.typed.insert(NAME);
    assert_eq!(provider.write_record(&typed).unwrap_err().code, Some(511));
    assert_eq!(
        text(&all_records(&mut provider, tasks), 1, "Count"),
        Some("4")
    );
    provider
        .write_record(&write(Some(1), &[(NAME, "Scripted too long")]))
        .unwrap();

    // Inside a transaction: held (the page shows it), then written.
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    let held = provider
        .write_record(&write(
            None,
            &[(NAME, "Held"), (COUNT, "6"), (CREATOR, "Script")],
        ))
        .unwrap();
    assert_eq!(
        text(&all_records(&mut provider, tasks), held.id, "Name"),
        Some("Held")
    );
    provider
        .write_record(&write(Some(2), &[(COUNT, "9")]))
        .unwrap();
    provider.commit_transaction(false).unwrap();
    let page = all_records(&mut provider, tasks);
    assert_eq!(text(&page, held.id, "Count"), Some("6"));
    assert_eq!(text(&page, held.id, "Creator"), Some("Script"));
    assert_eq!(text(&page, held.id, "Doubled"), Some("12"));
    assert_eq!(text(&page, 2, "Count"), Some("9"));
    provider.close();
}

/// Typing into a global field on a layout sets this session's value, as
/// Set Field does (Claris "Global fields"): the record's other changes
/// are written, the global's value is not sent to the host.
#[test]
fn typing_into_a_global_field_sets_the_sessions_value() {
    use fixture::SETTING;
    use om_data::host::CalcNeed;
    for (data_type, first, second) in [(1, "typed", "again"), (3, "01/02/2003", "02/03/2004")] {
        let file = file_with(&format!("provider-typed-global-{data_type}"), &[], |root| {
            fixture::add_field(
                root, TABLE, SETTING, "Setting", 1, data_type, 0, 0x1_0000, 0,
            );
        });
        let (port, ca) = host(&file.0);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let capabilities = provider.record_capabilities(tasks).unwrap();
        assert!(capabilities.editable_fields.unwrap().contains("Setting"));
        let global = |provider: &mut NativeProvider| {
            let data = provider
                .calc_data(&[CalcNeed::Globals { table: TABLE }])
                .unwrap();
            let locale = om_data::calc::Locale::en_us();
            let cx = om_data::calc::Ctx::new(
                &locale,
                om_data::calc::Date::from_ymd(2026, 10, 29).unwrap(),
            );
            data.globals[&TABLE]
                .get(SETTING, 1)
                .map(|v| v.to_text(&cx).into_string())
                .unwrap_or_default()
        };
        provider
            .update_record(tasks, 1, values(&[("Setting", first), ("Name", "Renamed")]))
            .unwrap();
        assert_eq!(global(&mut provider), first);
        assert_eq!(
            text(&all_records(&mut provider, tasks), 1, "Name"),
            Some("Renamed")
        );
        // Only a global typed: nothing is written, the record stays.
        provider
            .update_record(tasks, 2, values(&[("Setting", second)]))
            .unwrap();
        assert_eq!(global(&mut provider), second);
        if data_type == 3 {
            let before = text(&all_records(&mut provider, tasks), 2, "Name").map(str::to_owned);
            let error = provider
                .update_record(
                    tasks,
                    2,
                    values(&[("Setting", "not a date"), ("Name", "must not save")]),
                )
                .unwrap_err();
            assert_eq!(error.code, Some(500));
            assert_eq!(global(&mut provider), second);
            assert_eq!(
                text(&all_records(&mut provider, tasks), 2, "Name"),
                before.as_deref()
            );
            assert_eq!(
                provider
                    .set_typed_global(TABLE, SETTING, 2, first)
                    .unwrap_err()
                    .code,
                Some(3)
            );
            assert_eq!(global(&mut provider), second);
            // Scripts retain the separate, permissive assignment operation.
            provider
                .set_global(TABLE, SETTING, 1, "script text")
                .unwrap();
            assert_eq!(global(&mut provider), "script text");
        }
        // Another client starts from the file's stored value.
        let mut other = open(port, &ca);
        assert_eq!(global(&mut other), "");
        other.close();
        provider.close();
    }
}

#[test]
fn repetition_256_matches_literal_native_index() {
    let mut answers = Vec::new();
    for native_index in [false, true] {
        let file = file_with(
            if native_index {
                "independent-rep256-native"
            } else {
                "independent-rep256-cold"
            },
            &[],
            |root| {
                const FIELD: u64 = 250;
                fixture::add_field(root, TABLE, FIELD, "Repetition 256", 1, 1, 0, 0, 0);
                fixture::set_field_repetitions(root, TABLE, FIELD, 256);
                root.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(1),
                        &uint_segment(FIELD * 256 + 256),
                    ],
                    text_node(&[], "Ship"),
                );
                if native_index {
                    // Literal English key from query-program.md 7.2, not server key construction.
                    let key = vec![1, 0x14, 0x10, 0x12, 0xd3, 0x12, 0xec, 0x13, 0xb3];
                    root.put(
                        &[&uint_segment(TABLE), &[1], &uint_segment(FIELD), &[1], &key],
                        [1].into_iter().collect::<RidList>().to_node(Vec::new()),
                    );
                    let counts = [1u64.to_be_bytes(), 1u64.to_be_bytes()].concat();
                    root.put(
                        &[&uint_segment(TABLE), &[1], &uint_segment(FIELD), &[0x11]],
                        OwnedNode::leaf(Vec::new(), counts),
                    );
                }
            },
        );
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        answers.push(find_ids(
            &mut provider,
            tasks,
            vec![request("Repetition 256", "ship")],
            FindKind::Replace,
        ));
        provider.close();
    }
    assert_eq!(answers, [Ok(vec![1]), Ok(vec![1])]);
}

#[test]
fn join_repetition_256_matches_literal_native_index() {
    let mut answers = Vec::new();
    for native_index in [false, true] {
        let file = file_with(
            if native_index {
                "independent-join256-native"
            } else {
                "independent-join256-cold"
            },
            &[],
            |root| {
                const FIELD: u64 = 250;
                fixture::add_field(root, TABLE, FIELD, "Join repetition 256", 1, 1, 0, 0, 0);
                fixture::set_field_repetitions(root, TABLE, FIELD, 256);
                root.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(1),
                        &uint_segment(FIELD * 256 + 256),
                    ],
                    text_node(&[], "Hi"),
                );
                fixture::add_relationship(
                    root,
                    40,
                    (OCCURRENCE, FIELD),
                    0,
                    (fixture::NOTES_OCCURRENCE, fixture::BODY),
                );
                if native_index {
                    let key = vec![1, 0x12, 0xd3, 0x12, 0xec];
                    root.put(
                        &[&uint_segment(TABLE), &[1], &uint_segment(FIELD), &[0], &key],
                        [1].into_iter().collect::<RidList>().to_node(Vec::new()),
                    );
                    root.put(
                        &[&uint_segment(TABLE), &[1], &uint_segment(FIELD), &[0x10]],
                        OwnedNode::leaf(
                            Vec::new(),
                            [1u64.to_be_bytes(), 1u64.to_be_bytes()].concat(),
                        ),
                    );
                }
            },
        );
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        answers.push(find_ids(
            &mut provider,
            tasks,
            vec![request("Notes::Body", "==Hi")],
            FindKind::Replace,
        ));
        provider.close();
    }
    assert_eq!(answers, [Ok(vec![1]), Ok(vec![1])]);
}

#[path = "validation_scan/mod.rs"]
mod validation_scan;

#[path = "values/mod.rs"]
mod value_lists;
