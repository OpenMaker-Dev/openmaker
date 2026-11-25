//! OpenMaker Server hosting files from the store engine (`--engine store`,
//! docs/server-storage.md 5, 11): each file imported into the store folder
//! at startup and served read-only from its tree. Every read of the other
//! test servers is also answered through a store copy and compared
//! (`engine::copies`); these tests check what the store engine alone does:
//! a client's writes refused as on a file the server hosts read-only, what
//! it cannot do yet answering "unavailable" (error 3), a file the import
//! refuses left out, and a file the administrator opens again imported
//! again once it changed.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::time::{Duration, Instant};

use fixture::{TempFile, FILE, NAME, TABLE};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_protocol::{authoring, container, schema, write, Resume};
use om_scripts::{Script, ScriptStep};
use om_server::{FileStatus, Notice};
use om_session::Session;
use server::{records_of, start_hosting, start_on_store};

/// A store folder for one test, removed when dropped.
struct StoreDir(std::path::PathBuf);

impl StoreDir {
    fn new(tag: &str) -> StoreDir {
        let path = std::env::temp_dir().join(format!(
            "om-store-engine-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        StoreDir(path)
    }
}

impl Drop for StoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_file_hosted_from_the_store_reads_as_from_the_file_engine() {
    let file = TempFile::new("store-engine-reads", &[], true);
    let before = std::fs::read(&file.0).unwrap();
    let store = StoreDir::new("reads");
    let (port, _) = start_on_store(&[(FILE, &file)], &store.0, |s| s);
    let (mut client, _) = server::logged_in(port, "store");
    let stored = records_of(&mut client, TABLE);
    let (port, handle) = start_hosting(&[(FILE, &file)], |s| s);
    let (mut client, _) = server::logged_in(port, "file");
    let loaded = records_of(&mut client, TABLE);
    assert!(!stored.is_empty());
    assert_eq!(stored, loaded);
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
    // The file engine's server also answered each read through a store
    // copy (`engine::copies`): this test claims that comparison for every
    // read it counts (each call through the test switch), so it fails when
    // nothing was compared, the copy could not be made, or a read was of
    // any other kind (a later version, told to fail, run once).
    #[cfg(debug_assertions)]
    {
        let compared = handle.store_comparisons().expect("a server checking reads");
        assert!(compared.compared > 0, "{compared:?}");
        assert_eq!(
            (
                compared.no_copy,
                compared.changed,
                compared.failing,
                compared.single_run
            ),
            (0, 0, 0, 0),
            "{compared:?}"
        );
    }
}

/// The Tasks file with field work deferred for a field whose definition is
/// gone, which no server can do: the file engine hosts it read-only
/// (`ReadOnlyCause::DeferredWork`), and so the store engine's answers can
/// be compared with a read-only file's. Its field catalog comes back too.
fn read_only_tasks(tag: &str, scripts: &[Script]) -> (TempFile, OwnedNode) {
    let mut root = fixture::tree(scripts, true);
    let t = uint_segment(TABLE);
    let mut catalog = root.at(&[&t, &[3]]).unwrap().clone();
    catalog.remove(&[&[5], &uint_segment(NAME)]);
    catalog.put(
        &[&[1], &[0xfd], &uint_segment(NAME)],
        OwnedNode::leaf(Vec::new(), 0x1e1u32.to_be_bytes().to_vec()),
    );
    root.put(&[&t, &[3]], catalog.clone());
    (TempFile::from_tree(tag, &root), catalog)
}

/// One file hosted by both engines, a session logged in to each: the file
/// engine's first, the store engine's second.
struct Both {
    file: TempFile,
    before: Vec<u8>,
    _store: StoreDir,
    sessions: Vec<(Session, u32)>,
}

impl Both {
    fn host(tag: &str, file: TempFile) -> Both {
        let before = std::fs::read(&file.0).unwrap();
        let store = StoreDir::new(tag);
        let (port, _) = start_hosting(&[(FILE, &file)], |s| s);
        let by_file = server::logged_in(port, "file engine");
        let (port, _) = start_on_store(&[(FILE, &file)], &store.0, |s| s);
        let by_store = server::logged_in(port, "store engine");
        Both {
            file,
            before,
            _store: store,
            sessions: vec![by_file, by_store],
        }
    }

    /// Send what `request` builds for the file's id to both servers and
    /// check that they answer the same, byte for byte (an exception by its
    /// text): the store engine's answer.
    fn same(&mut self, what: &str, request: impl Fn(u32) -> Vec<u8>) -> Result<Vec<u8>, String> {
        let [by_file, by_store] = [0, 1].map(|i| {
            let (session, id) = &mut self.sessions[i];
            session.perform(&request(*id)).map_err(|e| e.to_string())
        });
        assert_eq!(
            by_store, by_file,
            "{what}: the store engine answers otherwise"
        );
        by_store
    }

    /// Both read the same records, and the file on disk is as it was.
    fn unchanged(mut self) {
        let [by_file, by_store] = [0, 1].map(|i| records_of(&mut self.sessions[i].0, TABLE));
        assert!(!by_store.is_empty());
        assert_eq!(by_store, by_file);
        assert_eq!(std::fs::read(&self.file.0).unwrap(), self.before);
    }
}

/// A request to send both engines: what it is, and its bytes for a file id.
type Row<'a> = (&'a str, Box<dyn Fn(u32) -> Vec<u8> + 'a>);

/// docs/server-storage.md 11: each write request a client sends a file the
/// store engine hosts is refused as the file engine refuses it for a file
/// it hosts read-only, byte for byte, before it changes anything: op71 and
/// op72 (status 200); op14 into the temporary area, op70, op13 into the
/// file and op73 (a system exception, #737 and #800); op88 and op87
/// (containers); and op86 programs that change the schema. A lock (op60)
/// is still granted, with the record's modification count read from the
/// store.
#[test]
fn a_clients_writes_are_refused_as_on_a_file_hosted_read_only() {
    let (file, catalog) = read_only_tasks("store-engine-writes", &[]);
    let mut both = Both::host("writes", file);
    let t = uint_segment(TABLE);
    let tables = vec![t.clone()];
    let record = serial::encode(1);
    let rows: [Row<'_>; 9] = [
        (
            "op71 create a record",
            Box::new(|id| {
                write::build_create_record(
                    1001,
                    id,
                    &write::CreateRecord {
                        table: tables.clone(),
                        count: 1,
                        txn: 0x31,
                    },
                )
            }),
        ),
        (
            "op72 advance a serial",
            Box::new(|id| {
                write::build_get_serial(
                    1002,
                    id,
                    &write::GetSerial {
                        table: tables.clone(),
                        field: vec![uint_segment(fixture::CODE)],
                        value: String::new(),
                        increment: 1,
                        flags: write::SERIAL_INCREMENT,
                    },
                )
            }),
        ),
        (
            "op14 stage a record",
            Box::new(|id| {
                let records = OwnedNode::list(
                    Vec::new(),
                    vec![OwnedNode::list(
                        record.clone(),
                        vec![OwnedNode::leaf(uint_segment(NAME), b"Staged".to_vec())],
                    )],
                );
                write::build_upload(
                    1003,
                    id,
                    &write::Upload {
                        payload: records.to_wire().unwrap(),
                        resume: Resume::start(),
                        terminal: Vec::new(),
                        flags: write::UPLOAD_FLAGS,
                        dest: vec![t.clone(), vec![5]],
                    },
                )
            }),
        ),
        (
            "op70 commit",
            Box::new(|id| {
                let list = OwnedNode::list(
                    Vec::new(),
                    vec![OwnedNode::list(
                        t.clone(),
                        vec![OwnedNode::list(
                            record.clone(),
                            vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
                        )],
                    )],
                );
                write::build_commit(
                    1004,
                    id,
                    &write::Commit {
                        list: list.to_wire().unwrap(),
                        resume: Resume::start(),
                        flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
                        txn: 0x31,
                    },
                )
            }),
        ),
        (
            "op13 upload into the file",
            Box::new(|id| {
                write::build_upload_to_host(
                    1005,
                    id,
                    &write::Upload {
                        payload: catalog.to_wire().unwrap(),
                        resume: Resume::start(),
                        terminal: Vec::new(),
                        flags: write::UPLOAD_NODE,
                        dest: vec![t.clone(), vec![3]],
                    },
                )
            }),
        ),
        (
            "op73 update a table",
            Box::new(|id| schema::build_update_table(1006, id, &tables)),
        ),
        (
            "op88 upload container data",
            Box::new(|id| {
                container::build_upload_binary(
                    1007,
                    id,
                    &container::UploadBinary {
                        payload: b"not stored".to_vec(),
                        resume: Resume::start(),
                        table: tables.clone(),
                        field: vec![uint_segment(NAME)],
                        solved_path: String::new(),
                        reserved: [0; 16],
                    },
                )
            }),
        ),
        (
            "op87 adjust a reference count",
            Box::new(|id| {
                container::build_adjust_reference_count(
                    1008,
                    id,
                    &container::AdjustReferenceCount {
                        path: vec![t.clone(), vec![0x1f], vec![5], vec![1]],
                        delta: 1,
                    },
                )
            }),
        ),
        (
            "op60 lock a record, with its modification count",
            Box::new(|id| {
                write::build_lock(
                    1009,
                    id,
                    &write::Lock {
                        waiter: 0,
                        timeout: 0,
                        want_mod_count: true,
                        path: vec![t.clone(), vec![5], record.clone()],
                        argument: id as i32,
                        host_file: id as i32,
                        txn: 0x32,
                        lock_type: write::LOCK_WRITE,
                    },
                )
            }),
        ),
    ];
    for (what, request) in &rows {
        let _ = both.same(what, request);
    }
    // A program that deletes the table (op86), with the key the session's
    // connection gives it.
    use om_query::program::{leaf, opcode};
    for i in 0..2 {
        let (session, id) = &mut both.sessions[i];
        let mut program = OwnedNode::list(Vec::new(), Vec::new());
        for (key, value) in [
            (leaf::OPCODE, vec![opcode::DELETE_TABLE]),
            (leaf::SRC, [vec![t.len() as u8], t.clone()].concat()),
            (leaf::FILE_ID, id.to_be_bytes().to_vec()),
            (
                leaf::ACCESS_KEY,
                om_crypto::query_access_key(1, session.connection_key(), *id).to_vec(),
            ),
        ] {
            program.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
        }
        let reply = session.query(&program.to_wire().unwrap()).unwrap();
        assert!(
            reply.slots.windows(3).any(|s| s == [2, 0, 200]),
            "op86 on engine {i}: {:02x?}",
            reply.slots
        );
    }
    both.unchanged();
}

/// docs/server-storage.md 11: on a file hosted read-only, on either
/// engine alike, a server script opens no record for editing and creates
/// none: Set Field and New Record fail with 3, "Command is unavailable",
/// where the script sees it, and a script that sets a field and ends with
/// the record open reports 3 too, never 0 with its edit dropped at the
/// session's end. Set Next Serial Value fails with 3 before anything is
/// written. A global, the session's own value, is set and read in the
/// next step. Nothing changes.
#[test]
fn server_scripts_that_would_write_fail_alike_on_both_engines() {
    let edit = fixture::script(
        4,
        "Edit",
        vec![
            fixture::on(86, true),
            fixture::go_to_record(1),
            fixture::set_field(fixture::COUNT, "99"),
            fixture::exit_with("Get ( LastError )"),
        ],
    );
    let add = fixture::script(
        5,
        "Add",
        vec![
            fixture::on(86, true),
            ScriptStep::new(7),
            fixture::exit_with("Get ( LastError )"),
        ],
    );
    let left_open = fixture::script(
        6,
        "Left open",
        vec![
            fixture::go_to_record(1),
            fixture::set_field(fixture::COUNT, "99"),
        ],
    );
    let mut serial = fixture::with_calc(116, "40");
    serial.set_field_spec(
        0,
        Some(om_scripts::FieldSpec::new(
            om_scripts::StoredKey::from_id(fixture::OCCURRENCE).unwrap(),
            om_scripts::StoredKey::from_id(fixture::COUNT).unwrap(),
            0,
        )),
    );
    let serial = fixture::script(
        7,
        "Serial",
        vec![
            fixture::on(86, true),
            serial,
            fixture::exit_with("Get ( LastError )"),
        ],
    );
    let global = fixture::script(
        8,
        "Global",
        vec![
            fixture::on(86, true),
            fixture::set_field(fixture::SETTING, "\"kept\""),
            fixture::exit_with("Get ( LastError ) & \"|\" & Tasks::Setting"),
        ],
    );
    let (file, _) = read_only_tasks(
        "store-engine-scripts",
        &[edit, add, left_open, serial, global],
    );
    let mut both = Both::host("scripts", file);
    let [by_file, by_store] = [0, 1].map(|i| {
        let reply = both.sessions[i]
            .0
            .perform_script_on_server(8, "", true)
            .unwrap();
        (reply.error, reply.result)
    });
    assert_eq!(by_store, by_file, "the global");
    assert_eq!(by_store, (0, "0|kept".to_owned()));
    for script in [4, 5, 6, 7] {
        let [by_file, by_store] = [0, 1].map(|i| {
            let reply = both.sessions[i]
                .0
                .perform_script_on_server(script, "", true)
                .unwrap();
            (reply.error, reply.result)
        });
        assert_eq!(by_store, by_file, "script {script}");
        assert_eq!(by_store.0, 3, "script {script}: {by_store:?}");
    }
    both.unchanged();
}

/// docs/server-storage.md 11: an authoring edit is refused with status 3
/// and changes nothing; a Save is answered as on a file hosted read-only,
/// byte for byte.
#[test]
fn authoring_edits_answer_unavailable() {
    let (file, catalog) = read_only_tasks("store-engine-authoring", &[]);
    let mut both = Both::host("authoring", file);
    let save = authoring::Request::Save;
    let saved = both.same("op240 Save", |id| {
        authoring::build_request(9, id, &save).unwrap()
    });
    assert!(
        matches!(
            authoring::parse_reply(&save, &saved.unwrap()).unwrap(),
            authoring::Reply::Refused {
                status: authoring::status::UNSUPPORTED,
                ..
            }
        ),
        "op240 Save"
    );
    let create = authoring::Request::CreateField(authoring::CreateField {
        table: TABLE,
        expected: authoring::catalog_revision(&catalog),
        data_type: 1,
        name: "Priority".into(),
        comment: String::new(),
    });
    let [by_file, by_store] = [0, 1].map(|i| {
        let (session, id) = &mut both.sessions[i];
        let reply = session
            .perform(&authoring::build_request(9, *id, &create).unwrap())
            .unwrap();
        authoring::parse_reply(&create, &reply).unwrap()
    });
    assert!(
        matches!(by_file, authoring::Reply::Refused { .. }),
        "{by_file:?}"
    );
    assert!(
        matches!(
            by_store,
            authoring::Reply::Refused {
                status: authoring::status::UNSUPPORTED,
                ..
            }
        ),
        "{by_store:?}"
    );
    both.unchanged();
}

/// docs/server-storage.md 11: a file whose tree's root no longer opens
/// (here a test's) stays listed and open, and every request on it fails as
/// a read of it does, also those the state derived when it was hosted could
/// answer: a login is answered "unable to open", and a download of a
/// session logged in before fails.
#[cfg(debug_assertions)]
#[test]
fn a_file_whose_root_does_not_open_fails_its_logins_too() {
    use om_protocol::{LoginEnvironment, MachineInfo};
    let file = TempFile::new("store-engine-broken-root", &[], true);
    let store = StoreDir::new("broken-root");
    let (port, handle) = start_on_store(&[(FILE, &file)], &store.0, |s| s);
    let (mut before, _) = server::logged_in(port, "before");
    handle.failing_reads(FILE, &[]);
    let table = uint_segment(TABLE);
    assert!(before.download(&[&table, &[5]]).is_err());
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut session = om_session::Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.open_file("127.0.0.1", FILE).unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    let login = session
        .login(
            fixture::ACCOUNT,
            fixture::PASSWORD,
            fixture::INTERNAL_CREATION,
            &environment,
        )
        .unwrap();
    assert_eq!(login.error_code, om_server::LOGIN_UNREADABLE_FILE);
    assert_eq!(handle.files()[0].status, FileStatus::Open);
}

/// docs/server-storage.md 11: a file the import refuses is not hosted, the
/// others are, and the refused file is left as it was.
#[test]
fn a_file_the_import_refuses_is_not_hosted() {
    let good = TempFile::new("store-engine-good", &[], true);
    let bad = TempFile(good.0.with_extension("bad.fmp12"));
    std::fs::write(&bad.0, b"not a FileMaker file").unwrap();
    let store = StoreDir::new("refused");
    let (port, handle) = start_on_store(&[(FILE, &good), ("Bad.fmp12", &bad)], &store.0, |s| s);
    let files = handle.files();
    assert_eq!(
        files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        [FILE]
    );
    let (mut client, _) = server::logged_in(port, "store");
    assert!(!records_of(&mut client, TABLE).is_empty());
    assert_eq!(std::fs::read(&bad.0).unwrap(), b"not a FileMaker file");
}

/// docs/server-storage.md 11: the Admin Console's close and open work on
/// the store engine. A file opened again unchanged is served from what the
/// store holds; one replaced while it was closed is imported again, and
/// clients read the new file.
#[test]
fn a_file_opened_again_is_imported_again_once_it_changed() {
    let file = TempFile::new("store-engine-reopen", &[], true);
    let store = StoreDir::new("reopen");
    let (port, handle) = start_on_store(&[(FILE, &file)], &store.0, |s| s);
    let read = |name: &str| {
        let (mut client, _) = server::logged_in(port, name);
        let records = records_of(&mut client, TABLE);
        client.disconnect().unwrap();
        records
    };
    let first = read("first");
    let reopen = || {
        let notice = Notice {
            message: String::new(),
            grace: Duration::ZERO,
        };
        handle.close_file(1, &notice).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while handle.files()[0].status != FileStatus::Closed {
            assert!(Instant::now() < deadline, "the file did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    reopen();
    handle.open_file(1).unwrap();
    assert_eq!(read("again"), first);
    reopen();
    let mut root = fixture::tree(&[], true);
    let path = [
        &uint_segment(TABLE)[..],
        &[5],
        &serial::encode(1),
        &uint_segment(NAME),
    ];
    root.put(&path, om_scripts::text_node(&[], "Replaced"));
    let replaced = TempFile::from_tree("store-engine-reopen-new", &root);
    std::fs::rename(&replaced.0, &file.0).unwrap();
    handle.open_file(1).unwrap();
    let after = read("replaced");
    assert_ne!(after, first);
    assert_eq!(server::cell(&after, 1, NAME), Some("Replaced"));
}
