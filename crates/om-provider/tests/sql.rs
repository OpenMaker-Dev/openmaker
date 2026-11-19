//! ExecuteSQL on the desktop client against an `om-server` on loopback, over
//! verified TLS: the provider's calculation schema and data
//! (`calc_schema`, `calc_data`) under `om_data::host::sql`, as the
//! desktop client's script host uses them, and an unstored calculation
//! that queries in a record page. The file is om-server's test fixture
//! (`tests/fixtures/tasks.rs`) with a value index on Name and a calculation
//! "Peers" that calls ExecuteSQL.
//!
//! The ignored test runs the FileMaker Server sample's rows of
//! `docs/re/catalogs/execute-sql-vectors.tsv` through a hosted copy and
//! compares each with FileMaker Server's answer and with om-sql's local
//! `.fmp12` source:
//!
//! ```sh
//! OM_SQL_RE=/path/to/openmaker/re cargo test -p om-provider --test sql -- --ignored --nocapture
//! ```

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "../../om-sql/examples/sql_vectors.rs"]
mod sql_vectors;

use std::cell::RefCell;
use std::path::Path;

use fixture::{TempFile, ACCOUNT, FILE, LAYOUT, NAME, OCCURRENCE, PASSWORD, TABLE};
use om_data::calc::eval::{display, evaluate_text, Context, Variables};
use om_data::calc::{Ctx, Date, Locale};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, FileCalc, FileHost, SessionInfo,
};
use om_data::{Connection, DataProvider};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

/// The fixture's unstored calculation that queries its own table.
const PEERS: u64 = 8;
const PEERS_FORMULA: &str =
    "ExecuteSQL ( \"SELECT COUNT(*) FROM Tasks WHERE \\\"Count\\\" > ?\" ; \"\" ; \"\" ; Count )";

/// A server hosting `path` as `name` on an ephemeral port, and the CA to
/// trust.
fn host(path: &Path, name: &str) -> (u16, Vec<u8>) {
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
            name: name.into(),
            path: path.to_path_buf(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    (port, ca_pem)
}

fn open(port: u16, ca_pem: Vec<u8>, file: &str, account: &str) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem,
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: file.into(),
            account: account.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    provider
}

/// The fixture with a value index on Name and the calculation Peers.
fn file() -> TempFile {
    let mut root = fixture::tree(&[], true);
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    let (t, f) = (uint_segment(TABLE), uint_segment(PEERS));
    let at = |rest: &[&[u8]]| -> Vec<Vec<u8>> {
        [&t[..], &[3], &[5], &f]
            .iter()
            .chain(rest)
            .map(|s| s.to_vec())
            .collect()
    };
    let put = |root: &mut OwnedNode, path: Vec<Vec<u8>>, node: OwnedNode| {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        root.put(&path, node);
    };
    put(&mut root, at(&[&[16]]), om_scripts::text_node(&[], "Peers"));
    // A number calculation, unstored (`0x2_0000`), evaluated always
    // (`0x20`), English index language, one repetition.
    let mut definition = vec![0u8; 26];
    definition[0] = 2;
    definition[1] = 2;
    definition[4..8].copy_from_slice(&21u32.to_be_bytes());
    definition[8..12].copy_from_slice(&0x2_0020u32.to_be_bytes());
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    put(
        &mut root,
        at(&[&[2]]),
        OwnedNode::leaf(Vec::new(), definition),
    );
    let names = fixture::names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(PEERS_FORMULA, &ctx).unwrap();
    for (child, bytes) in [
        (4u8, om_formula::Key::from_id(OCCURRENCE).0),
        (5, om_formula::formatted::encode(&compiled.tokens)),
        (6, om_formula::polish::encode(&compiled.expr)),
    ] {
        put(
            &mut root,
            at(&[&[5], &[child]]),
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
    TempFile::from_tree("sql", &root)
}

fn today() -> Date {
    Date::from_ymd(2026, 10, 29).unwrap()
}

/// Evaluate `text` as the desktop client's script host does: the provider's
/// schema, data fetched through `calc_data` as calculations need it into
/// `cache`. Every fetch's needs are logged.
fn evaluate(
    provider: &RefCell<&mut NativeProvider>,
    schema: &CalcSchema,
    session: &SessionInfo,
    cache: &RefCell<CalcData>,
    log: &RefCell<Vec<Vec<CalcNeed>>>,
    context: &Context,
    text: &str,
) -> (String, i32) {
    let fetch = |needs: &[CalcNeed]| {
        log.borrow_mut().push(needs.to_vec());
        provider.borrow_mut().calc_data(needs).ok()
    };
    let source = CachedSource::new(cache).with_fetch(&fetch);
    let today = session.now.map_or_else(today, |t| t.date());
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, &source, today),
        session,
        &mut locals,
        &mut globals,
    );
    let result = evaluate_text(text, &mut host, context);
    (display(&result.value, &host), result.code())
}

#[test]
fn executes_sql_against_the_hosted_file() {
    let file = file();
    let (port, ca_pem) = host(&file.0, FILE);
    let mut provider = open(port, ca_pem, FILE, ACCOUNT);
    // The host builds Name's value index for the first find on it
    // (query-program.md 7.1), which the provider sees missing and does not
    // use; from then on Name has the index an indexed field has.
    let ship = CalcNeed::Lookup {
        table: TABLE,
        field: NAME,
        text: "Ship".into(),
    };
    for answered in [false, true] {
        let data = provider.calc_data(std::slice::from_ref(&ship)).unwrap();
        let lookup = data.lookups.values().next().unwrap();
        assert_eq!(lookup.is_some(), answered, "{lookup:?}");
    }
    let schema = provider
        .calc_schema()
        .unwrap()
        .expect("a calculation schema");
    let provider = RefCell::new(&mut provider);
    let session = SessionInfo {
        account_name: ACCOUNT.into(),
        ..SessionInfo::for_this_machine()
    };
    let log = RefCell::new(Vec::new());
    let cache = RefCell::new(CalcData::default());
    let sql = |cache: &RefCell<CalcData>, query: &str| {
        let text = format!("ExecuteSQL ( {query:?} ; \"|\" ; \"¶\" )");
        let (value, code) = evaluate(
            &provider,
            &schema,
            &session,
            cache,
            &log,
            &Context::new(),
            &text,
        );
        assert_eq!(code, 0, "{query}: {value}");
        value
    };

    // `Name = 'Ship'` is a find on the host's value index: only the record
    // it finds is downloaded.
    assert_eq!(
        sql(&cache, "SELECT ROWID FROM Tasks WHERE Name = 'Ship'"),
        "2"
    );
    let needs = log.take();
    assert!(
        needs.iter().flatten().any(|n| matches!(
            n,
            CalcNeed::Lookup { table: TABLE, field: NAME, text } if &**text == "Ship"
        )),
        "{needs:?}"
    );
    let records: Vec<&CalcNeed> = needs
        .iter()
        .flatten()
        .filter(|n| matches!(n, CalcNeed::Record { .. }))
        .collect();
    assert_eq!(
        records,
        [&CalcNeed::Record {
            table: TABLE,
            record: 2
        }]
    );

    // Stored values by type, a stored calculation, and an unstored one
    // that queries again (each record counts the tasks with a larger Count).
    assert_eq!(
        sql(
            &cache,
            "SELECT Name, \"Count\", Due, Doubled, Peers FROM Tasks ORDER BY ROWID"
        ),
        "Write tests|3|2026-10-01|6|1\rShip|5||10|0"
    );
    assert_eq!(
        sql(
            &cache,
            "SELECT b.Body, COUNT(*) FROM Tasks a, Notes b GROUP BY b.Body"
        ),
        "Hi|2"
    );
    assert_eq!(
        sql(
            &cache,
            "SELECT TableName, TableId, BaseTableName, BaseFileName FROM FileMaker_Tables"
        ),
        "Notes|1065090|Notes|Tasks\rTasks|1065089|Tasks|Tasks"
    );
    assert_eq!(
        sql(
            &cache,
            "SELECT FieldName, FieldType, FieldClass FROM FileMaker_Fields WHERE TableName = 'Tasks'"
        ),
        "Name|varchar|Normal\rCount|decimal|Normal\rDue|date|Normal\rSetting|global varchar|Normal\r\
         Doubled|decimal|Calculated\rCreator|varchar|Normal\rCode|varchar|Normal\rPeers|decimal|Calculated"
    );
    // A calculation's own context reaches the query only as an argument.
    let (value, code) = evaluate(
        &provider,
        &schema,
        &session,
        &cache,
        &log,
        &Context::record(OCCURRENCE, 1),
        "ExecuteSQL ( \"SELECT Name FROM Tasks WHERE \\\"Count\\\" = ?\" ; \"\" ; \"\" ; Count + 2 )",
    );
    assert_eq!((value.as_str(), code), ("Ship", 0));

    // The record page computes Peers on the provider, querying there.
    let page = provider
        .borrow_mut()
        .records(u32::try_from(LAYOUT).unwrap(), 0, 10)
        .unwrap();
    let peers = page.fields.iter().position(|f| f.name == "Peers").unwrap();
    let values: Vec<Option<&str>> = page
        .records
        .iter()
        .map(|r| r.values[peers].as_deref())
        .collect();
    assert_eq!(values, [Some("1"), Some("0")]);
    provider.borrow_mut().close();
}

#[test]
#[ignore = "needs the FileMaker Server sample (OM_SQL_RE)"]
fn the_hosted_sample_answers_as_filemaker_server_and_the_local_file() {
    const SAMPLE: &str = "en_FMServer_Sample.fmp12";
    let (path, locale) = sql_vectors::sample(SAMPLE).unwrap();
    assert!(path.exists(), "{} is missing", path.display());
    // Both copies are named `work.fmp12`, as the harness's were
    // (`BaseFileName`).
    let dir = std::env::temp_dir().join(format!("om-provider-sql-{}", std::process::id()));
    for side in ["hosted", "local"] {
        std::fs::create_dir_all(dir.join(side)).unwrap();
        std::fs::copy(&path, dir.join(side).join("work.fmp12")).unwrap();
    }
    // The server checks a login against the file's own accounts; the
    // sample has only Guest, so the hosted copy gets an account "Admin" in
    // [Full Access] with [`PASSWORD`] (security catalog record 2), written
    // as OpenMaker Server writes files.
    let account = "Admin".to_string();
    {
        let original = om_hbam::HbamFile::open(&path).unwrap();
        let mut root = original.key_tree().unwrap().root.to_owned_node();
        root.put(
            &[&[23], &[1], &[5], &[2], &[16]],
            om_scripts::text_node(&[], &account),
        );
        fixture::put_account(&mut root, 2, PASSWORD, None, 1);
        let bytes = om_hbam::writer::rebuild(&root, original.superblock().raw).unwrap();
        std::fs::write(dir.join("hosted/work.fmp12"), bytes).unwrap();
    }
    let (port, ca_pem) = host(&dir.join("hosted/work.fmp12"), "work.fmp12");
    let mut provider = open(port, ca_pem, "work.fmp12", &account);
    let schema = provider
        .calc_schema()
        .unwrap()
        .expect("a calculation schema");
    let provider = RefCell::new(&mut provider);
    let mut local = om_sql::fmp12::FileHost::open(
        dir.join("local/work.fmp12"),
        locale.clone(),
        sql_vectors::now(),
    )
    .unwrap();
    // The harness session's answers (calc-values.md 12.1).
    let version = "FMDeveloperTool 26.0.3.309 (08-26-2026)";
    local.set_get(1, om_calc::Value::text(version));
    local.set_get(12, om_calc::Value::text("English"));
    let session = SessionInfo {
        account_name: account.clone(),
        application_version: version.into(),
        system_language: "English".into(),
        now: Some(sql_vectors::now()),
        utc_offset: 0,
        ..SessionInfo::for_this_machine()
    };
    let cache = RefCell::new(CalcData::default());
    let log = RefCell::new(Vec::new());
    let cx_locale = Locale::en_us();
    let cx = Ctx::new(&cx_locale, sql_vectors::now().date());
    let (mut rows, mut expected, mut same) = (0, 0, 0);
    let mut differences = Vec::new();
    let started = std::time::Instant::now();
    for row in sql_vectors::rows() {
        if row.skip.is_some() || !row.file.starts_with(SAMPLE) {
            continue;
        }
        rows += 1;
        let context = sql_vectors::context(&row.context);
        let local_result = om_calc::eval::evaluate_text(&row.expression, &mut local, &context);
        let local_text = local_result.value.to_text(&cx).into_string();
        let (hosted, code) = evaluate(
            &provider,
            &schema,
            &session,
            &cache,
            &log,
            &context,
            &row.expression,
        );
        let hosted = (hosted, code);
        if hosted == (row.result.clone(), row.error) {
            expected += 1;
        }
        if hosted == (local_text.clone(), local_result.code()) {
            same += 1;
        }
        if hosted != (row.result.clone(), row.error) {
            differences.push(format!(
                "line {}: {}\n  expected {:?} ({})\n  local    {:?} ({})\n  hosted   {:?} ({})",
                row.line,
                row.expression,
                row.result,
                row.error,
                local_text,
                local_result.code(),
                hosted.0,
                hosted.1
            ));
        }
    }
    let fetches = log.borrow().len();
    let needs: usize = log.borrow().iter().map(Vec::len).sum();
    println!(
        "{rows} rows: {expected} as FileMaker Server, {same} as the local file; \
         {fetches} fetches ({needs} needs) in {:.1?}",
        started.elapsed()
    );
    for d in &differences {
        println!("{d}");
    }
    provider.borrow_mut().close();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(rows > 1000, "{rows} rows ran");
    assert!(differences.is_empty(), "{} rows differ", differences.len());
}
