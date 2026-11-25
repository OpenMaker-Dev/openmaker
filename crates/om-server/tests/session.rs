//! End to end: OpenMaker's own client (`om-session`) against a running
//! `om-server` over real TLS on loopback.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::path::PathBuf;

use om_protocol::{LoginEnvironment, MachineInfo, Opcode};
use om_server::default_identity::{self, SystemCertificate};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};
use om_session::Session;

/// A local copy of the lab's `test project.fmp12`, git-ignored on the analysis
/// host. Its account "Admin" is record 2 of the security catalog.
const TEST_PROJECT: &str = "../../re/work/login/testproject.fmp12";

/// A copy of test project taken with FileMaker Server's backup command, with
/// Contacts records 1 "Test Record", 2 "Hello World", 3 "Ima Computer".
const TEST_PROJECT_3: &str = "../../re/work/omserver/testproject-live-1234.fmp12";

/// The account of those copies, and its password from the environment:
/// the server checks it against the one the file stores.
const ACCOUNT: &str = "Admin";
const PASSWORD_VARIABLE: &str = "OM_TEST_PROJECT_PASSWORD";

fn password() -> String {
    std::env::var(PASSWORD_VARIABLE)
        .unwrap_or_else(|_| panic!("{PASSWORD_VARIABLE} holds the test project's password"))
}

/// Start a server on an ephemeral loopback port, hosting `files`.
fn start(files: Vec<HostedFile>) -> u16 {
    start_with(
        files,
        Identity::self_signed(vec!["localhost".into()]).unwrap(),
    )
}

/// [`start`] with the TLS certificate `identity`.
fn start_with(files: Vec<HostedFile>, identity: Identity) -> u16 {
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files,
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    port
}

/// Connect with a throwaway client certificate, as FileMaker clients may.
fn connect(port: u16) -> Session {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    connect_with(
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
    )
}

fn connect_with(port: u16, cert_pem: &[u8], key_pem: &[u8]) -> Session {
    Session::connect(
        "127.0.0.1",
        port,
        cert_pem,
        key_pem,
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap()
}

/// FileMaker Pro presents an X.509 version 1 certificate, which certificate
/// path libraries refuse; the server must still complete the handshake.
#[test]
fn accepts_a_version_1_client_certificate() {
    let port = start(Vec::new());
    let session = connect_with(
        port,
        include_bytes!("fixtures/v1-client.pem"),
        include_bytes!("fixtures/v1-client.key"),
    );
    assert_eq!(session.identity().remote_name, "openmaker-test");
    session.disconnect().unwrap();
}

/// The certificate the server makes and keeps when given none is one
/// OpenMaker's verifying client accepts once told to trust the kept file
/// (`OPENMAKER_CA_CERT`), by each name it was made for.
#[test]
fn verifying_client_trusts_the_kept_certificate() {
    let state = std::env::temp_dir().join(format!("om-session-kept-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let names = ["localhost".to_string(), "127.0.0.1".to_string()];
    let none: &[SystemCertificate] = &[];
    let (identity, _) = default_identity::choose(none, Some(&state), &names).unwrap();
    let port = start_with(Vec::new(), identity);
    let trusted = std::fs::read(state.join("tls/cert.pem")).unwrap();
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    for host in names {
        let session = Session::connect_verified(
            &host,
            port,
            client.cert.pem().as_bytes(),
            client.signing_key.serialize_pem().as_bytes(),
            "127.0.0.1",
            &MachineInfo::default(),
            &trusted,
        )
        .unwrap_or_else(|e| panic!("{host}: {e}"));
        assert_eq!(session.identity().remote_name, "openmaker-test");
        session.disconnect().unwrap();
    }
    std::fs::remove_dir_all(&state).unwrap();
}

#[test]
fn handshake_open_and_unsupported_ops() {
    let port = start(Vec::new());
    let mut session = connect(port);
    let identity = session.identity().clone();
    assert_eq!(identity.remote_name, "openmaker-test");
    assert_eq!(identity.version_string, "OpenMaker Server test");
    assert_eq!(identity.protocol_version, 0x20);
    assert_ne!(identity.connection_id, 0);

    // A second session gets its own id and key.
    let other = connect(port);
    assert_ne!(other.identity().connection_id, identity.connection_id);
    assert_ne!(other.connection_key(), session.connection_key());
    other.disconnect().unwrap();

    let missing = session.open_file("127.0.0.1", "nothing here.fmp12");
    assert!(
        missing
            .as_ref()
            .is_err_and(|e| e.to_string().contains("0x0004")),
        "{missing:?}"
    );

    // An opcode the server does not implement (GetGuestCount, op84) gets
    // status 3 at the stack tail.
    let txn = om_rctransaction::Builder::new(Opcode::GetGuestCount as u8).finish();
    let result = session.perform(&txn).unwrap();
    assert_eq!(result, [0x00, 0x03]);
    session.disconnect().unwrap();
}

/// A client that has only connected (any certificate passes TLS) cannot make
/// the server hold what it uploads: each upload-style request is refused
/// before any part of it is kept, so no reply hands out a cursor to
/// continue. Before Login the server holds only a key list for a path
/// readable then, and only a small one. Logged in, a connection holds a
/// bounded number of transfers: starting more evicts the least recently
/// used, continuing that one fails instead of reading as the end of its
/// stream, and a cursor continues only the operation that started it. An
/// upload whose copy key a one-byte length cannot frame fails without
/// ending the connection.
#[test]
fn holds_uploads_only_for_requests_that_may_run() {
    use om_protocol::container::{self, UploadBinary};
    use om_protocol::{
        build_download_list, build_query, parse_download_list_reply, parse_query_reply, write,
        DownloadListReply, DownloadListRequest, QueryReply, QueryRequest, Resume, QUERY_MAX_RETURN,
    };
    use om_session::SessionError;

    let file = fixture::TempFile::new("held-uploads", &[], true);
    let port = server::start(&file);
    // A node's leaves, its list never closed: an upload that continues.
    let unfinished = |len: usize| {
        let mut list = vec![0x80];
        while list.len() < len {
            list.extend([0x03, 0x01, 1, 2, 3, 4]);
        }
        list
    };
    let query = |session: &mut Session, file_id, program: &[u8], resume: &Resume| {
        let txn = build_query(&QueryRequest {
            transaction_id: 1,
            file_id,
            program,
            resume: resume.clone(),
        });
        let result = session.perform_with_max(&txn, QUERY_MAX_RETURN)?;
        Ok::<QueryReply, SessionError>(parse_query_reply(&result).unwrap())
    };
    // An op92 whose key list is `part`: `build_download_list` frames only a
    // whole list, so the empty one it builds (`80 40` and its u32 length on
    // a first request) is replaced.
    let key_list =
        |session: &mut Session, file_id, path: &[&[u8]], part: &[u8], resume: &Resume| {
            let txn = build_download_list(&DownloadListRequest {
                transaction_id: 1,
                file_id,
                key_path: path,
                keys: &[],
                flag: 0,
                resume: resume.clone(),
            });
            let framed = if resume.has_more() { 4 } else { 6 };
            let len = u32::try_from(part.len()).unwrap().to_be_bytes();
            let txn = [&txn[..20], part, &len, &txn[20 + framed..]].concat();
            let result = session.perform(&txn)?;
            Ok::<DownloadListReply, SessionError>(parse_download_list_reply(&result).unwrap())
        };
    // The error slot of a refused query, "record access is denied".
    let denied = vec![0x80, 0x06, 0x00, 0x03, 0x02, 0x00, 0xc8, 0x40];
    let program = unfinished(4096);
    // A list that stops inside a push with a 255-byte key, whose copy key
    // (256 bytes) a one-byte length cannot frame.
    let long_key = [&[0x80, 0x38, 0xff][..], &[0x5a; 255]].concat();

    // Connected only, no file open.
    let mut session = connect(port);
    for payload in [&program, &long_key] {
        let reply = query(&mut session, 0x7777, payload, &Resume::start()).unwrap();
        assert_eq!(
            (&reply.slots, &reply.resume),
            (&denied, &Resume::complete())
        );
    }

    // The file open, nobody logged in to it.
    let file_id = session
        .open_file("127.0.0.1", fixture::FILE)
        .unwrap()
        .file_id;
    for payload in [&program, &long_key] {
        let reply = query(&mut session, file_id, payload, &Resume::start()).unwrap();
        assert_eq!(
            (&reply.slots, &reply.resume),
            (&denied, &Resume::complete())
        );
        let records = |flags| write::Upload {
            payload: payload.clone(),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags,
            dest: vec![vec![0x80, 0x01], vec![0x05]],
        };
        let commit = write::Commit {
            list: payload.clone(),
            resume: Resume::start(),
            flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
            txn: 1,
        };
        for txn in [
            write::build_upload(1, file_id, &records(write::UPLOAD_FLAGS)),
            write::build_upload_to_host(1, file_id, &records(write::UPLOAD_NODE)),
            write::build_commit(1, file_id, &commit),
        ] {
            let reply = session.perform(&txn).unwrap();
            assert_eq!(
                write::parse_upload_reply(&reply).unwrap(),
                Resume::no_stream()
            );
        }
        let binary = UploadBinary {
            payload: payload.clone(),
            resume: Resume::start(),
            table: vec![vec![0x80, 0x01]],
            field: vec![vec![0x14]],
            solved_path: String::new(),
            reserved: [0; 16],
        };
        let reply = session
            .perform(&container::build_upload_binary(1, file_id, &binary))
            .unwrap();
        let reply = container::parse_upload_binary_reply(&reply).unwrap();
        assert_eq!((reply.status, reply.resume.has_more()), (200, false));
        let reply = key_list(&mut session, file_id, &[&[3]], payload, &Resume::start()).unwrap();
        assert_eq!((reply.status, reply.resume.has_more()), (200, false));
    }
    // A key list for a path readable before Login whose copy key cannot be
    // framed fails, and the connection goes on.
    let refused = key_list(&mut session, file_id, &[&[2]], &long_key, &Resume::start());
    assert!(refused.is_err());

    // A key list for the status area, readable before Login, is held in
    // parts up to 64 KiB; the part that would pass that fails.
    let part = unfinished(16 << 10);
    let (mut resume, mut held) = (Resume::start(), 0);
    let refused = loop {
        let bytes = if resume.has_more() {
            &part[1..]
        } else {
            &part[..]
        };
        match key_list(&mut session, file_id, &[&[2]], bytes, &resume) {
            Ok(reply) => {
                assert!(reply.resume.has_more(), "an unfinished list continues");
                resume = reply.resume;
                held += bytes.len();
            }
            Err(e) => break e,
        }
    };
    assert!(held <= 64 << 10 && held + part.len() > 64 << 10, "{held}");
    assert!(refused.to_string().contains("SystemException"), "{refused}");

    // Logged in: the copy key that cannot be framed fails a program too.
    let (mut session, file_id) = server::logged_in(port, "held-uploads");
    assert!(query(&mut session, file_id, &long_key, &Resume::start()).is_err());
    // Nine unfinished programs, one more than a connection holds.
    let started: Vec<Resume> = (0..9)
        .map(|_| {
            let reply = query(&mut session, file_id, &program, &Resume::start()).unwrap();
            assert!(reply.resume.has_more());
            reply.resume
        })
        .collect();
    // The least recently used was evicted: continuing it fails, where a
    // cursor never issued gets the empty last page.
    let closing = [0x40];
    assert!(query(&mut session, file_id, &closing, &started[0]).is_err());
    let never = Resume {
        perform: 1,
        cursor: 2,
        ..Resume::start()
    };
    let reply = query(&mut session, file_id, &closing, &never).unwrap();
    assert_eq!((reply.slots.len(), reply.resume), (0, Resume::complete()));
    // Another operation cannot continue a cursor; its own finishes it.
    let reply = key_list(&mut session, file_id, &[&[2]], &[], &started[1]).unwrap();
    assert_eq!((reply.slots.len(), reply.resume), (0, Resume::complete()));
    for resume in [&started[1], &started[8]] {
        let reply = query(&mut session, file_id, &closing, resume).unwrap();
        assert_eq!(reply.resume, Resume::complete());
        assert!(!reply.slots.is_empty());
    }
}

/// A connection's requests hold at most `MAX_LOCKS` locks: past that a new
/// lock and a record creation are refused as a lock conflict with the
/// session itself, which reserves no id, while a lock it holds can be
/// taken again. Another client locks and creates records as before, and
/// the first gets room again when a transaction ends.
#[test]
fn holds_a_bounded_number_of_locks_per_connection() {
    use om_hbam::slot::uint_segment;
    use om_protocol::write::{self, LockReply, LOCK_CONFLICT};

    let file = fixture::TempFile::new("lock-cap", &[], true);
    let port = server::start(&file);
    let (mut greedy, greedy_file) = server::logged_in(port, "greedy");
    let (mut other, other_file) = server::logged_in(port, "other");
    let table = uint_segment(fixture::TABLE);
    let create = |session: &mut Session, file_id, count, txn| {
        let request = write::CreateRecord {
            table: vec![table.clone()],
            count,
            txn,
        };
        let result = session
            .perform(&write::build_create_record(1, file_id, &request))
            .unwrap();
        write::parse_create_record_reply(&result).unwrap()
    };
    let record = |id: u64| [table.clone(), vec![5], om_hbam::serial::encode(id)];
    let lock = |session: &mut Session, path: &[Vec<u8>; 3], txn| {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        session.lock(&path, txn, false).unwrap()
    };

    // The fixture's table has records 1 and 2: the blocks start at 3.
    let block = 1_000;
    for txn in 0..u32::try_from(om_server::MAX_LOCKS / block).unwrap() {
        let reply = create(&mut greedy, greedy_file, block as u32, txn);
        assert_eq!(reply.status, 0, "block {txn}");
    }
    let last = 2 + om_server::MAX_LOCKS as u64;
    let refused = create(&mut greedy, greedy_file, 1, 999);
    assert_eq!(refused.status, u32::from(LOCK_CONFLICT));
    match lock(&mut greedy, &record(last + 1_000), 999) {
        LockReply::Refused { status, conflict } => {
            assert_eq!(status, LOCK_CONFLICT);
            assert!(conflict.self_conflict && conflict.users.contains("Admin"));
        }
        granted => panic!("a lock past the cap: {granted:?}"),
    }
    // Taking a lock it holds again adds none.
    assert!(matches!(
        lock(&mut greedy, &record(3), 0),
        LockReply::Granted { .. }
    ));

    // The other client is unaffected, and its record comes after the ids
    // reserved before: the refused request reserved none.
    let reply = create(&mut other, other_file, 25, 7);
    assert_eq!(reply.status, 0);
    assert_eq!(om_hbam::serial::decode(&reply.record[0]), Some(last + 1));
    assert!(matches!(
        lock(&mut other, &record(1), 8),
        LockReply::Granted { .. }
    ));
    // The greedy client's reserved ids stay its own.
    assert!(matches!(
        lock(&mut other, &record(3), 8),
        LockReply::Refused { .. }
    ));

    // Ending a transaction gives the room back.
    greedy.kill_all_locks(0).unwrap();
    assert_eq!(create(&mut greedy, greedy_file, 1, 999).status, 0);
}

/// Notifications wait for a client that sends no request, but within
/// bounds. A client that keeps asking gets every notification in the order
/// they came, each reply carrying what fits in its `maxreturn`. A bulk
/// change by a script is combined for a client that has not asked: the
/// deleted records' notifications become one for the table's records. A
/// client that has not asked while more distinct notifications came than
/// combining can bound is closed, and the server goes on serving the
/// others. Only a release of a lock the sender held, with a code naming a
/// change, is announced.
#[test]
fn bounds_the_notifications_waiting_for_a_quiet_client() {
    use om_hbam::slot::uint_segment;
    use om_protocol::write::{self, attention, Notification};
    use om_server::{MAX_QUEUED, MAX_QUEUED_BYTES};

    let mut root = fixture::tree(
        &[fixture::script(
            1,
            "Delete all",
            vec![fixture::on(86, true), om_scripts::ScriptStep::new(10)],
        )],
        true,
    );
    // More records than notifications may wait for one connection.
    let records = MAX_QUEUED as u64 + 100;
    fixture::add_tasks(&mut root, 3..=records);
    let file = fixture::TempFile::from_tree("notification-bounds", &root);
    let port = server::start(&file);
    let (mut quiet, _) = server::logged_in(port, "quiet");
    let (mut polling, _) = server::logged_in(port, "polling");
    let (mut sender, sender_file) = server::logged_in(port, "sender");
    let probe = om_rctransaction::Builder::new(Opcode::GetGuestCount as u8).finish();
    let catalog = |i: u32| vec![vec![0x42; 250], i.to_be_bytes()[2..].to_vec()];
    let unlock = |session: &mut Session, path: &Vec<Vec<u8>>, code: u32| {
        let request = write::Unlock {
            message_address: 0,
            attention_mask: code,
            path: path.clone(),
            host_file: sender_file as i32,
            txn: 1,
            lock_type: write::LOCK_WRITE,
        };
        session
            .perform(&write::build_unlock(1, sender_file, &request))
            .unwrap();
    };
    let announce = |session: &mut Session, path: &Vec<Vec<u8>>, code: u32| {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        session.lock(&segments, 1, false).unwrap();
        unlock(session, path, code);
    };
    let seen = |notifications: &[Notification]| -> Vec<(u32, Vec<u8>)> {
        notifications.iter().map(server::notified).collect()
    };
    let key =
        |path: &Vec<Vec<u8>>| server::path_key(&path.iter().map(Vec::as_slice).collect::<Vec<_>>());
    // Everything waiting, in replies of at most `max_return` bytes.
    let collect = |session: &mut Session, max_return: u32| {
        let mut all = Vec::new();
        loop {
            let result = session.perform_with_max(&probe, max_return).unwrap();
            let got = session.take_notifications();
            if got.is_empty() {
                return all;
            }
            let backchannel = 2 + got.iter().map(|n| 6 + n.message.len()).sum::<usize>();
            assert!(result.len() + backchannel <= max_return as usize);
            all.extend(got);
        }
    };

    // Only a change, of a lock the sender held, is announced.
    let held_by_other = catalog(0xffff);
    let segments: Vec<&[u8]> = held_by_other.iter().map(Vec::as_slice).collect();
    polling.lock(&segments, 1, false).unwrap();
    unlock(&mut sender, &held_by_other, 2);
    unlock(&mut sender, &catalog(0xfffe), 2);
    announce(&mut sender, &catalog(0xfffd), 0x71);
    announce(&mut sender, &catalog(0xfffc), 0x802);
    // Announced changes, delivered in small replies.
    let announced: Vec<Vec<Vec<u8>>> = (0..40).map(catalog).collect();
    for path in &announced {
        announce(&mut sender, path, 2);
    }
    let expected: Vec<(u32, Vec<u8>)> = announced.iter().map(|p| (2, key(p))).collect();
    let small = collect(&mut polling, 1_000);
    assert_eq!(seen(&small), expected);
    // What one of them takes in a backchannel.
    let size = 6 + small[0].message.len();

    // A script deletes every record in one commit. Past `MAX_QUEUED`
    // waiting notifications the deleted records' become one for the
    // table's records; those of the records deleted after that follow.
    let reply = sender.perform_script_on_server(1, "", true).unwrap();
    assert_eq!(reply.error, 0, "{reply:?}");
    let code = attention::DELETE | attention::RECORD;
    let table = key(&vec![uint_segment(fixture::TABLE), vec![5]]);
    let combined = |waiting: usize, got: Vec<(u32, Vec<u8>)>| {
        let before = MAX_QUEUED + 1 - waiting;
        assert_eq!(
            got.len(),
            waiting + 1 + (records as usize - before),
            "{got:?}"
        );
        assert_eq!(got[waiting], (code, table.clone()));
        for (other, path) in &got[waiting + 1..] {
            assert!(*other == code && path.starts_with(&table) && path.len() > table.len());
        }
        got[..waiting].to_vec()
    };
    let polled = combined(0, seen(&collect(&mut polling, 0x20_0000)));
    assert!(polled.is_empty());
    let waited = combined(expected.len(), seen(&collect(&mut quiet, 0x20_0000)));
    assert_eq!(waited, expected);

    // Distinct announcements past what combining can bound: the quiet
    // client is closed, the polling one gets every one of them.
    let rounds = MAX_QUEUED_BYTES / size + 1;
    let mut got = Vec::new();
    let mut expected = Vec::new();
    for i in 0..u32::try_from(rounds).unwrap() {
        let path = catalog(i);
        announce(&mut sender, &path, 2);
        expected.push((2, key(&path)));
        if i % 100 == 99 {
            got.extend(collect(&mut polling, 0x20_0000));
        }
    }
    got.extend(collect(&mut polling, 0x20_0000));
    assert_eq!(seen(&got), expected);
    assert!(quiet.perform(&probe).is_err());
    assert!(quiet.connection_lost().is_some());
    // The others go on.
    announce(&mut sender, &catalog(0), 2);
    assert_eq!(
        seen(&collect(&mut polling, 0x20_0000)),
        [(2, key(&catalog(0)))]
    );
}

/// Records of short keys staged with op14 on `session`, 16,000 to an
/// upload, until the server refuses one: how many uploads it took. `next`
/// is the first record id, moved on past each upload sent.
fn stage_until_refused(session: &mut Session, next: &mut u64) -> usize {
    use om_hbam::{serial, OwnedNode};
    let table = om_hbam::slot::uint_segment(fixture::TABLE);
    for staged in 0..100 {
        let records: Vec<OwnedNode> = (*next..*next + 16_000)
            .map(|id| OwnedNode::leaf(serial::encode(id), Vec::new()))
            .collect();
        *next += 16_000;
        if let Err(e) = session.upload_records(&table, records) {
            assert!(e.to_string().contains("SystemException"), "{e}");
            return staged;
        }
    }
    panic!("100 uploads staged without a refusal");
}

/// The downloads connections keep paged share one budget on the server
/// (#145). With a budget smaller than any stream, each paged download
/// started drops the one left unfinished longest, on another client's
/// connection too: continuing that one fails, where the newest reads to
/// its end as the whole stream reads at once.
#[test]
fn paged_downloads_share_the_servers_budget() {
    use om_hbam::OwnedNode;
    use om_protocol::{
        build_download_from_host, parse_download_from_host_reply, DownloadRequest, Resume,
        DOWNLOAD_MAX_RETURN,
    };
    use om_server::config::ConnectionLimits;
    // A subtree of 1.5 MB: two pages.
    let mut root = fixture::tree(&[], true);
    root.set_child(OwnedNode::list(
        [0x7e],
        (0..3000u32)
            .map(|n| OwnedNode::leaf(n.to_be_bytes(), vec![7; 500]))
            .collect(),
    ));
    let file = fixture::TempFile::from_tree("download-memory", &root);
    let port = server::start_configured(&file, |server| {
        server.with_connection_limits(ConnectionLimits {
            download_memory: 1,
            ..ConnectionLimits::DEFAULT
        })
    });
    let page = |session: &mut Session, file_id, resume: Resume| {
        let txn = build_download_from_host(&DownloadRequest {
            transaction_id: 1,
            file_id,
            key_path: &[&[0x7e]],
            resume,
        });
        let result = session.perform_with_max(&txn, DOWNLOAD_MAX_RETURN)?;
        Ok::<_, om_session::SessionError>(parse_download_from_host_reply(&result).unwrap())
    };
    let read_all = |session: &mut Session, file_id| {
        let mut reply = page(session, file_id, Resume::start()).unwrap();
        let mut slots = reply.slots.clone();
        while reply.resume.has_more() {
            reply = page(session, file_id, reply.resume.clone()).unwrap();
            slots.extend(&reply.slots);
        }
        slots
    };
    let (mut first, first_file) = server::logged_in(port, "download-memory-1");
    let (mut second, second_file) = server::logged_in(port, "download-memory-2");
    let whole = read_all(&mut first, first_file);
    assert!(whole.len() > 1_500_000, "{}", whole.len());
    // The stream is held past the budget, alone.
    let started = page(&mut first, first_file, Resume::start()).unwrap();
    assert!(started.resume.has_more());
    // Another client's download drops it.
    assert_eq!(read_all(&mut second, second_file), whole);
    let refused = page(&mut first, first_file, started.resume).unwrap_err();
    assert!(refused.to_string().contains("SystemException"), "{refused}");
    // The connection goes on.
    assert_eq!(read_all(&mut first, first_file), whole);
}

/// A server whose clients' upload trees may take 32 MiB in all, so 8 MiB
/// for one address (#387).
fn start_with_little_upload_memory(file: &fixture::TempFile) -> u16 {
    use om_server::config::ConnectionLimits;
    server::start_configured(file, |server| {
        let server = server.with_connection_limits(ConnectionLimits {
            upload_memory: 32 << 20,
            ..ConnectionLimits::DEFAULT
        });
        // Perform of op250, which no client sends, panics.
        #[cfg(debug_assertions)]
        let server = server.panicking_on(250, false);
        server
    })
}

/// Staging past what one client address may hold of the server's upload
/// memory is refused before any of the upload is staged: committing a
/// record that the refused upload held changes nothing, while the same
/// change staged within the limit commits.
#[test]
fn refuses_staging_past_what_one_address_may_hold() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode};
    let file = fixture::TempFile::new("upload-memory-refused", &[], true);
    let port = start_with_little_upload_memory(&file);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(1)];
    let record = |text: &str| {
        OwnedNode::list(
            serial::encode(1),
            vec![OwnedNode::leaf(name.clone(), om_scsu::encode_scsu(text))],
        )
    };
    let changed = || OwnedNode::list(serial::encode(1), vec![OwnedNode::leaf(name.clone(), [])]);
    let commit = |session: &mut Session, txn: u32| {
        assert!(matches!(
            session.lock(&path, txn, true).unwrap(),
            om_protocol::write::LockReply::Granted { .. }
        ));
        session
            .commit_records(&table, vec![changed()], txn)
            .unwrap();
        session.kill_all_locks(txn).unwrap();
    };

    let (mut full, _) = server::logged_in(port, "full");
    let mut next = 1_000_000;
    assert!(stage_until_refused(&mut full, &mut next) >= 1);
    // An upload holding record 1 changed, past the limit: refused whole.
    let mut records = vec![record("refused")];
    records.extend((next..next + 16_000).map(|id| OwnedNode::leaf(serial::encode(id), [])));
    assert!(full.upload_records(&table, records).is_err());
    commit(&mut full, 0x31);
    let names = server::records_of(&mut full, fixture::TABLE);
    assert_eq!(server::cell(&names, 1, fixture::NAME), Some("Write tests"));

    // Within the limit, the same change stages and commits.
    let (mut within, _) = server::logged_in(port, "within");
    within
        .upload_records(&table, vec![record("staged")])
        .unwrap();
    commit(&mut within, 0x32);
    let names = server::records_of(&mut within, fixture::TABLE);
    assert_eq!(server::cell(&names, 1, fixture::NAME), Some("staged"));
}

/// What a connection holds of the server's upload memory comes back when
/// its socket closes in the middle of staging, without `Disconnect`, and
/// when its thread panics: a second connection from the same address can
/// then stage as much as the first did.
#[test]
fn gives_back_the_upload_memory_of_a_dropped_connection() {
    let file = fixture::TempFile::new("upload-memory-dropped", &[], true);
    let port = start_with_little_upload_memory(&file);
    let mut next = 1_000_000;
    let (mut first, _) = server::logged_in(port, "first");
    let fills = stage_until_refused(&mut first, &mut next);
    assert!(fills >= 1);
    drop(first);
    // The server ends the session when it reads the socket's end.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (mut second, _) = server::logged_in(port, "second");
    let mut staged = 0;
    while staged == 0 && std::time::Instant::now() < deadline {
        staged = stage_until_refused(&mut second, &mut next);
        if staged == 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    assert_eq!(staged, fills, "the second connection had the whole share");

    #[cfg(debug_assertions)]
    {
        let txn = om_rctransaction::Builder::new(250).finish();
        assert!(second.perform(&txn).is_err());
        let (mut third, _) = server::logged_in(port, "third");
        let mut staged = 0;
        while staged == 0 && std::time::Instant::now() < deadline {
            staged = stage_until_refused(&mut third, &mut next);
            if staged == 0 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        assert_eq!(
            staged, fills,
            "the panicking connection gave its share back"
        );
    }
}

/// Send one part of about 1 MiB of an op88 upload whose list never closes:
/// the first part (`resume` the start) opens it. Answers the block that
/// continues it.
fn send_part(
    session: &mut Session,
    file_id: u32,
    resume: &om_protocol::Resume,
) -> Result<om_protocol::Resume, om_session::SessionError> {
    use om_protocol::container::{build_upload_binary, parse_upload_binary_reply, UploadBinary};
    // Leaves with 2-byte keys and 1000-byte values, as FileMaker Pro sends
    // a stream's pieces.
    let mut payload = if resume.has_more() {
        vec![]
    } else {
        vec![0x80]
    };
    for key in 0..1043u16 {
        payload.push(0x0f);
        payload.extend(key.to_be_bytes());
        payload.extend(1000u16.to_be_bytes());
        payload.extend([0x5a; 1000]);
    }
    let request = UploadBinary {
        payload,
        resume: resume.clone(),
        table: vec![om_hbam::slot::uint_segment(fixture::TABLE)],
        field: vec![om_hbam::slot::uint_segment(fixture::PHOTO)],
        solved_path: String::new(),
        reserved: [0; 16],
    };
    let reply = session.perform(&build_upload_binary(1, file_id, &request))?;
    let reply = parse_upload_binary_reply(&reply).unwrap();
    assert!(reply.resume.has_more(), "status {:#x}", reply.status);
    Ok(om_protocol::Resume {
        depth: -1,
        ..reply.resume
    })
}

/// Hold up to `parts` parts of an unfinished upload on `session`: how many
/// the server held, and the block of the next part if it refused one.
fn hold_parts(
    session: &mut Session,
    file_id: u32,
    parts: usize,
) -> (usize, Option<om_protocol::Resume>) {
    let mut resume = om_protocol::Resume::start();
    for held in 0..parts {
        match send_part(session, file_id, &resume) {
            Ok(next) => resume = next,
            Err(e) => {
                assert!(e.to_string().contains("SystemException"), "{e}");
                return (held, Some(resume));
            }
        }
    }
    (parts, None)
}

/// An upload held in parts is charged as each part arrives: the part past
/// what one client address may hold is refused, the upload is dropped
/// whole (its next part is refused too), and what it held comes back, so
/// the same connection can hold as much again.
#[test]
fn refuses_held_uploads_past_what_one_address_may_hold() {
    let file = fixture::TempFile::new("held-upload-refused", &[], true);
    let port = start_with_little_upload_memory(&file);
    let (mut session, file_id) = server::logged_in(port, "holder");
    let (held, refused) = hold_parts(&mut session, file_id, 100);
    let refused = refused.expect("a part past the share is refused");
    assert!(held >= 2, "{held}");
    assert!(send_part(&mut session, file_id, &refused).is_err());
    assert_eq!(hold_parts(&mut session, file_id, 100).0, held);
}

/// What an unfinished upload holds comes back when its connection's socket
/// closes in the middle of it, without `Disconnect`, and when its thread
/// panics: a second connection from the same address can then hold as much.
#[test]
fn gives_back_held_uploads_of_a_dropped_connection() {
    let file = fixture::TempFile::new("held-upload-dropped", &[], true);
    let port = start_with_little_upload_memory(&file);
    // How many parts fit one address's share; refusing the next one drops
    // that upload whole.
    let (mut probe, file_id) = server::logged_in(port, "probe");
    let fits = hold_parts(&mut probe, file_id, 100).0;
    assert!(fits >= 2, "{fits}");
    // Holds the whole share without being refused.
    let hold_all = |name: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (mut session, file_id) = server::logged_in(port, name);
        // The server ends a dropped session when it reads the socket's end.
        loop {
            match hold_parts(&mut session, file_id, fits) {
                (held, None) if held == fits => break,
                (0, Some(_)) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the share never came back"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                other => panic!("held {other:?} of {fits} parts"),
            }
        }
        session
    };
    let first = hold_all("first");
    drop(first);
    let second = hold_all("second");

    #[cfg(debug_assertions)]
    {
        let mut second = second;
        let txn = om_rctransaction::Builder::new(250).finish();
        assert!(second.perform(&txn).is_err());
        hold_all("third");
    }
    #[cfg(not(debug_assertions))]
    drop(second);
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn open_download_and_login_against_a_hosted_file() {
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: PathBuf::from(TEST_PROJECT),
    }]);
    let local = om_hbam::HbamFile::open(TEST_PROJECT).unwrap();
    let mut session = connect(port);

    // Names match case-insensitively.
    let open = session
        .open_file("127.0.0.1", "Test Project.FMP12")
        .unwrap();
    assert_eq!(open.server_message, "openmaker-test");

    // Before Login only the status area is readable; it carries the second
    // half of the Login nonce.
    let internal_creation = session.fetch_internal_creation().unwrap();
    assert_eq!(Some(internal_creation), local.internal_creation().unwrap());
    let refused = session.download(&[&[3]]).unwrap();
    assert_eq!((refused.lock_error, refused.slots.len()), (200, 0));

    // Before Login FileMaker Pro also reads the security catalog entry
    // [23, 1, 1], which FileMaker Server serves.
    let security = session.download(&[&[23], &[1], &[1]]).unwrap();
    assert_eq!(security.lock_error, 0);
    assert!(!security.slots.is_empty());

    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "test project.fmp12");
    let wrong = session
        .login(ACCOUNT, "wrong", internal_creation, &environment)
        .unwrap();
    assert_eq!(wrong.error_code, 212);
    let right = session
        .login("admin", &password(), internal_creation, &environment)
        .unwrap();
    // The result key names the account's record (key 2), as FileMaker
    // Server's reply for this file does.
    assert!(right.is_success(), "{right:?}");
    assert_eq!(
        (right.result_key.as_slice(), right.version),
        (&[1, 2][..], Some(9))
    );

    // After Login the schema is readable, and it decodes to the file's own tree.
    let schema = session.download(&[&[3]]).unwrap();
    assert_eq!(schema.lock_error, 0);
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
        &schema.slots,
        vec![om_hbam::KeySegment(&[3])],
    ))
    .unwrap();
    let expected = local.subtree(&[&[3]]).unwrap().unwrap();
    assert_eq!(tree.node(&[&[3]]).unwrap().children, expected.children);

    // A key the file does not have comes back empty.
    let absent = session.download(&[&[0x7f, 0x7f]]).unwrap();
    assert_eq!((absent.lock_error, absent.slots.len()), (0, 0));

    // Records by id, as the official client fetches a found set (op92): the
    // existing ones come back, a missing id is left out.
    let table = local.catalog().unwrap().tables[0].id;
    let records = local.records(table).unwrap();
    assert!(!records.is_empty(), "test project has records");
    let table_key = om_hbam::slot::uint_segment(table);
    let ids: Vec<Vec<u8>> = records
        .iter()
        .map(|r| om_hbam::slot::uint_segment(r.id))
        .chain([vec![0xff, 0xff, 0xff]])
        .collect();
    let keys: Vec<&[u8]> = ids.iter().map(Vec::as_slice).collect();
    let path: [&[u8]; 2] = [&table_key, &[5]];
    let list = session.download_list(&path, &keys, 0).unwrap();
    assert_eq!(list.status, 0);
    let base = path.iter().map(|s| om_hbam::KeySegment(s)).collect();
    let tree =
        om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(&list.slots, base)).unwrap();
    let got: Vec<&[u8]> = tree
        .node(&path)
        .unwrap()
        .children
        .iter()
        .map(|c| c.segment.0)
        .collect();
    assert_eq!(got, &keys[..keys.len() - 1]);
    session.disconnect().unwrap();
}

/// A subtree larger than one page comes back in pages that concatenate to
/// the same stream one reply carries; a key holding a value comes back inline.
#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn pages_large_subtrees_and_inlines_values() {
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: PathBuf::from(TEST_PROJECT),
    }]);
    let mut session = connect(port);
    session
        .open_file("127.0.0.1", "test project.fmp12")
        .unwrap();
    let internal_creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "test project.fmp12");
    let login = session
        .login(ACCOUNT, &password(), internal_creation, &environment)
        .unwrap();
    assert!(login.is_success());

    // The theme subtree [6, 5, 1] is about 141 KB: one reply at the official
    // maxreturn, dozens of pages at 4 KiB.
    let path: [&[u8]; 3] = [&[6], &[5], &[1]];
    let whole = session.download(&path).unwrap();
    assert!(whole.slots.len() > 100_000, "{} bytes", whole.slots.len());
    let paged = session.download_with_max(&path, 4096).unwrap();
    assert_eq!(paged.slots.len(), whole.slots.len());
    assert!(
        paged.slots == whole.slots,
        "pages concatenate to the stream"
    );
    assert_eq!(paged.resume, om_protocol::Resume::complete());

    // [2, 254] holds mInternalCreation as a 4-byte value.
    let value = session.download(&[&[2], &[254]]).unwrap();
    assert_eq!(value.node_state, om_protocol::NodeState::Terminal);
    assert!(value.slots.is_empty());
    assert_eq!(value.value, internal_creation.to_be_bytes());

    // A missing key.
    let missing = session.download(&[&[0x7f], &[1]]).unwrap();
    assert_eq!(missing.node_state, om_protocol::NodeState::Missing);
    session.disconnect().unwrap();
}

/// Finds run on the server: the client's program (with this session's access
/// keys) comes back with the found set, and a wrong key is refused.
#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn runs_find_programs() {
    use om_hbam::{OwnedNode, RidList};
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: PathBuf::from(TEST_PROJECT_3),
    }]);
    let mut session = connect(port);
    let open = session
        .open_file("127.0.0.1", "test project.fmp12")
        .unwrap();
    let internal_creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "test project.fmp12");
    assert!(session
        .login(ACCOUNT, &password(), internal_creation, &environment)
        .unwrap()
        .is_success());
    let key = *session.connection_key();

    // One SimpleFind instruction on Contacts (80 02) with the given finders,
    // writing the found set to [3, 2].
    let program = |finders: Vec<Vec<u8>>, access: [u8; 16]| {
        let leaf = |k: u8, v: &[u8]| OwnedNode::leaf(vec![k], v.to_vec());
        let instruction = OwnedNode::list(
            vec![1],
            vec![
                leaf(0x00, &[10]),
                leaf(0x01, &[1, 2, 1, 1]),
                leaf(0x02, &[1, 3, 1, 2]),
                leaf(0x04, &open.file_id.to_be_bytes()),
                leaf(0x05, &[2, 0x80, 0x02]),
                leaf(0x07, &access),
            ],
        );
        let finders = finders
            .into_iter()
            .enumerate()
            .map(|(i, f)| OwnedNode::leaf(vec![i as u8 + 1], f))
            .collect();
        OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::list(vec![1], vec![instruction]),
                OwnedNode::list(vec![2], vec![OwnedNode::list(vec![1], finders)]),
            ],
        )
        .to_wire()
        .unwrap()
    };
    // Finder: kind, scope, options, table, field, repetition, begin, end.
    let finder = |kind: u8, options: u8, field: u8, begin: &[u8], end: &[u8]| {
        let key = |k: &[u8]| -> Vec<u8> {
            if k.is_empty() {
                vec![0]
            } else {
                [&[k.len() as u8 + 1, k.len() as u8][..], k].concat()
            }
        };
        [
            &[kind, 1, options][..],
            &[3, 2, 0x80, 0x02],
            &[2, 1, field],
            &[0, 1],
            &key(begin),
            &key(end),
        ]
        .concat()
    };
    let found = |slots: &[u8]| {
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(slots)).unwrap();
        RidList::from_node(tree.node(&[&[2]]).expect("result spot [2]"))
            .unwrap()
            .iter()
            .collect::<Vec<_>>()
    };
    let access = om_crypto::query_access_key(1, &key, open.file_id);

    // Every non-empty Last Name (value index range over all text keys).
    let all = session
        .query(&program(
            vec![finder(2, 3, 2, &[0x01], &[0x01, 0xff, 0xff])],
            access,
        ))
        .unwrap();
    assert_eq!(found(&all.slots), [1, 2, 3]);
    // Record ids 2..3, ANDed with Last Name range: records 2 and 3.
    let two = session
        .query(&program(
            vec![
                finder(6, 3, 2, &[2], &[3]),
                finder(2, 3, 2, &[0x01], &[0x01, 0xff, 0xff]),
            ],
            access,
        ))
        .unwrap();
    assert_eq!(found(&two.slots), [2, 3]);
    // A wrong access key is refused with FileMaker's unknown-error slot.
    let mut bad = access;
    bad[3] ^= 0x40;
    let refused = session
        .query(&program(vec![finder(6, 3, 2, &[1], &[3])], bad))
        .unwrap();
    assert_eq!(
        refused.slots,
        [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40]
    );
    session.disconnect().unwrap();
}

/// A session logged in to test project, connecting as machine `name`.
fn logged_in(port: u16, name: &str) -> (Session, u32) {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut session = Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo {
            machine_name: name.into(),
            ..MachineInfo::default()
        },
    )
    .unwrap();
    let open = session
        .open_file("127.0.0.1", "test project.fmp12")
        .unwrap();
    let internal_creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "test project.fmp12");
    assert!(session
        .login(ACCOUNT, &password(), internal_creation, &environment)
        .unwrap()
        .is_success());
    (session, open.file_id)
}

/// FileMaker Pro's record write path end to end (write-path.md 1): New
/// Record, commit, edit under a lock while another session is refused and
/// told of each change, then delete. The server applies the changes to the
/// file it holds in memory, keeping the record list and indexes current.
#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn creates_edits_and_deletes_records() {
    use om_hbam::{OwnedNode, RidList};
    use om_protocol::write::{self, LockReply};
    use om_protocol::{build_request_notification, RequestNotification};

    // The server writes changes back to the file, so it hosts a copy.
    let copy =
        std::env::temp_dir().join(format!("om-server-write-test-{}.fmp12", std::process::id()));
    std::fs::copy(TEST_PROJECT_3, &copy).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let (mut a, file) = logged_in(port, "writer-a");
    let (mut b, _) = logged_in(port, "writer-b");
    let contacts = vec![vec![0x80, 0x02]];

    // B asks to be told about changes to the file.
    let registration = RequestNotification {
        attention_mask: u32::MAX,
        message_address: 1,
        path: Vec::new(),
        file_id: file,
        client_file_id: -4,
    };
    let reply = b
        .perform(&build_request_notification(1, &registration))
        .unwrap();
    assert_eq!(reply, [0, 0]);

    // New Record: op71 reserves an id after the table's last.
    let created = write::parse_create_record_reply(
        &a.perform(&write::build_create_record(
            2,
            file,
            &write::CreateRecord {
                table: contacts.clone(),
                count: 1,
                txn: 0x101,
            },
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(created.status, 0);
    let record = created.record[0].clone();
    let id = om_hbam::serial::decode(&record).unwrap();
    assert!(id > 3, "a new id after the existing records: {id}");

    // Commit: op14 uploads the record, op70 commits it, op103, op65.
    let text = |s: &str| om_scsu::encode_scsu(s);
    let upload_and_commit =
        |session: &mut Session, fields: Vec<OwnedNode>, entry: OwnedNode, txn| {
            // FileMaker Pro's form: the record, then the 40 closing [T, 05],
            // without a leading 80.
            let payload =
                OwnedNode::list(Vec::new(), vec![OwnedNode::list(record.clone(), fields)])
                    .to_wire()
                    .unwrap()[1..]
                    .to_vec();
            let reply = session
                .perform(&write::build_upload(
                    3,
                    file,
                    &write::Upload {
                        payload,
                        resume: om_protocol::Resume::start(),
                        terminal: Vec::new(),
                        flags: write::UPLOAD_FLAGS,
                        dest: vec![vec![0x80, 0x02], vec![0x05]],
                    },
                ))
                .unwrap();
            assert_eq!(
                write::parse_upload_reply(&reply).unwrap(),
                om_protocol::Resume::no_stream()
            );
            let list = OwnedNode::list(
                Vec::new(),
                vec![OwnedNode::list(
                    vec![0x80, 0x02],
                    vec![OwnedNode::list(vec![0x05], vec![entry])],
                )],
            )
            .to_wire()
            .unwrap();
            session
                .perform(&write::build_commit(
                    4,
                    file,
                    &write::Commit {
                        list,
                        resume: om_protocol::Resume::start(),
                        flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
                        txn,
                    },
                ))
                .unwrap();
        };
    let fields = |last: &str, mod_count: Option<u8>| {
        let mut fields = vec![
            OwnedNode::leaf(vec![0x01], text("OpenMaker E2E")),
            OwnedNode::leaf(vec![0x02], text(last)),
            OwnedNode::leaf(vec![0x0c], text("10/28/2026 1:02:03 PM")),
        ];
        if let Some(count) = mod_count {
            fields.push(OwnedNode::leaf(vec![0xfc], vec![0x01, count]));
        }
        fields
    };
    upload_and_commit(
        &mut a,
        fields("Writer", None),
        OwnedNode::leaf(record.clone(), vec![write::COMMIT_NEW]),
        0x101,
    );
    let traninfo = a
        .perform(&write::build_transact_info(
            5,
            file,
            &write::TransactInfo {
                files: vec![file as i32],
                txn: 0x101,
            },
        ))
        .unwrap();
    assert_eq!(traninfo, write::TRANSACT_INFO_REPLY.to_be_bytes());
    a.perform(&write::build_kill_all_locks(
        6,
        file,
        &write::KillAllLocks {
            host_file: file as i32,
            txn: 0x101,
            all: false,
        },
    ))
    .unwrap();

    // The record, the record list and the indexes show it.
    let tree = |session: &mut Session, path: &[&[u8]]| {
        let reply = session.download(path).unwrap();
        om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&reply.slots))
            .unwrap()
            .root
            .to_owned_node()
    };
    let records_under = |node: &OwnedNode, key: &[u8]| {
        node.child(key)
            .map(|k| RidList::from_owned(k).unwrap().iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let first_name = om_collate::text_key("OpenMaker E2E");
    let stored = tree(&mut a, &[&[0x80, 0x02], &[0x05], &record]);
    assert_eq!(
        stored.child(&[0x01]).unwrap().value.as_deref(),
        Some(&text("OpenMaker E2E")[..])
    );
    let ids = tree(&mut a, &[&[0x80, 0x02], &[0x0d]]);
    assert!(RidList::from_owned(&ids).unwrap().contains(id));
    let index = tree(&mut a, &[&[0x80, 0x02], &[0x01], &[0x01], &[0x00]]);
    assert_eq!(records_under(&index, &first_name), [id]);
    let created_at =
        om_collate::timestamp_key("10/28/2026 1:02:03 PM", om_collate::DateOrder::MonthDayYear)
            .unwrap();
    let index = tree(&mut a, &[&[0x80, 0x02], &[0x01], &[0x0c], &[0x00]]);
    assert_eq!(records_under(&index, &created_at), [id]);
    // The commit reached the file on disk.
    let saved = om_hbam::HbamFile::open(&copy).unwrap();
    let saved_record = saved
        .subtree(&[&[0x80, 0x02], &[0x05], &record])
        .unwrap()
        .expect("the new record is in the saved file")
        .to_owned_node();
    assert_eq!(saved_record.children, stored.children);

    // B hears of the new record with its next request (write-path.md 6.4).
    let notified = b
        .perform_reply(&write::build_transact_info(
            7,
            file,
            &write::TransactInfo {
                files: vec![],
                txn: 0,
            },
        ))
        .unwrap();
    let notifications = write::parse_notification_backchannel(&notified.backchannel).unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].code, 0x101);
    let mut path = vec![0x80, 0x02];
    path.extend([0x01, 0x05, record.len() as u8]);
    path.extend(&record);
    assert!(notifications[0]
        .message
        .windows(path.len())
        .any(|w| w == path));

    // Edit: A locks the record (mod count 01 00); B is refused with A's name.
    let lock = |session: &mut Session, txn: u32| {
        let request = write::Lock {
            waiter: 1,
            timeout: u32::MAX,
            want_mod_count: true,
            path: vec![vec![0x80, 0x02], vec![0x05], record.clone()],
            argument: file as i32,
            host_file: file as i32,
            txn,
            lock_type: write::LOCK_WRITE,
        };
        write::parse_lock_reply(
            &session
                .perform(&write::build_lock(8, file, &request))
                .unwrap(),
            true,
        )
        .unwrap()
    };
    assert_eq!(
        lock(&mut a, 0x102),
        LockReply::Granted {
            mod_count: Some(vec![0x01, 0x00])
        }
    );
    match lock(&mut b, 0x201) {
        LockReply::Refused { status, conflict } => {
            assert_eq!(status, write::LOCK_CONFLICT);
            assert!(!conflict.self_conflict);
            assert_eq!(conflict.users, "writer-a (Admin)");
        }
        other => panic!("B was not refused: {other:?}"),
    }
    let last_name = |s| om_collate::text_key(s);
    upload_and_commit(
        &mut a,
        fields("Edited", Some(1)),
        OwnedNode::list(
            record.clone(),
            vec![OwnedNode::leaf(vec![0x02], Vec::new())],
        ),
        0x102,
    );
    let index = tree(&mut a, &[&[0x80, 0x02], &[0x01], &[0x02], &[0x00]]);
    assert!(records_under(&index, &last_name("Writer")).is_empty());
    assert_eq!(records_under(&index, &last_name("Edited")), [id]);
    // The commit released A's lock; B now gets it, with the new mod count.
    assert_eq!(
        lock(&mut b, 0x201),
        LockReply::Granted {
            mod_count: Some(vec![0x01, 0x01])
        }
    );
    b.perform(&write::build_kill_all_locks(
        9,
        file,
        &write::KillAllLocks {
            host_file: file as i32,
            txn: 0x201,
            all: false,
        },
    ))
    .unwrap();

    // Delete: lock, commit `03`. The record and its index entries are gone.
    assert!(matches!(lock(&mut a, 0x103), LockReply::Granted { .. }));
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            vec![0x80, 0x02],
            vec![OwnedNode::list(
                vec![0x05],
                vec![OwnedNode::leaf(record.clone(), vec![write::COMMIT_DELETED])],
            )],
        )],
    )
    .to_wire()
    .unwrap();
    a.perform(&write::build_commit(
        10,
        file,
        &write::Commit {
            list,
            resume: om_protocol::Resume::start(),
            flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
            txn: 0x103,
        },
    ))
    .unwrap();
    let gone = a.download(&[&[0x80, 0x02], &[0x05], &record]).unwrap();
    assert_eq!(gone.node_state, om_protocol::NodeState::Missing);
    let ids = tree(&mut a, &[&[0x80, 0x02], &[0x0d]]);
    assert!(!RidList::from_owned(&ids).unwrap().contains(id));
    let index = tree(&mut a, &[&[0x80, 0x02], &[0x01], &[0x01], &[0x00]]);
    assert!(records_under(&index, &first_name).is_empty());
    a.disconnect().unwrap();
    b.disconnect().unwrap();

    // The saved file holds the original records, not the deleted one, and
    // remembers the issued id.
    let saved = om_hbam::HbamFile::open(&copy).unwrap();
    let ids: Vec<u64> = saved
        .records(0x8002)
        .unwrap()
        .iter()
        .map(|r| r.id)
        .collect();
    assert!(!ids.contains(&id), "{ids:?}");
    assert!(ids.starts_with(&[1, 2, 3]), "{ids:?}");
    let last = saved.subtree(&[&[0x80, 0x02], &[0x00]]).unwrap().unwrap();
    assert_eq!(last.value.unwrap()[1..], om_hbam::serial::encode(id));
    std::fs::remove_file(&copy).unwrap();
}

/// A binary-data tree as FileMaker Pro uploads it with op88
/// (containers.md 2): directory, MD5 and the streams, long ones chunked.
fn binary_data(streams: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    use md5::{Digest, Md5};
    use om_hbam::OwnedNode;
    let mut directory = (streams.len() as u16).to_be_bytes().to_vec();
    let mut hash = Md5::new();
    let mut keys = Vec::new();
    for (kind, bytes) in streams {
        directory.extend(kind.as_slice());
        directory.extend((bytes.len() as u32).to_be_bytes());
        hash.update(kind);
        hash.update(bytes);
        keys.push(if bytes.len() <= 1000 {
            OwnedNode::leaf(kind.to_vec(), bytes.to_vec())
        } else {
            let mut pieces = vec![OwnedNode::leaf(vec![0], (bytes.len() as u32).to_be_bytes())];
            pieces.extend(bytes.chunks(1000).enumerate().map(|(i, piece)| {
                OwnedNode::leaf(om_hbam::serial::encode(i as u64 + 1), piece.to_vec())
            }));
            OwnedNode::list(kind.to_vec(), pieces)
        });
    }
    keys.insert(0, OwnedNode::leaf(vec![0x02], directory));
    keys.insert(1, OwnedNode::leaf(vec![0x04], hash.finalize().to_vec()));
    OwnedNode::list(Vec::new(), keys).to_wire().unwrap()
}

/// A `width` x `height` PNG; `noise` makes it incompressible.
fn png(width: u32, height: u32, noise: bool) -> Vec<u8> {
    let mut seed = 0x2545_f491u32;
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        if noise {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let [a, b, c, _] = seed.to_be_bytes();
            image::Rgb([a, b, c])
        } else {
            image::Rgb([(x * 5) as u8, (y * 5) as u8, 128])
        }
    });
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Container fields as FileMaker Pro uses them (containers.md 3-7): op88
/// stores a picture (deduplicated by MD5, multi-part when large), op96
/// scales it, op87 releases it, and the library is saved with the file.
#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn stores_scales_and_releases_container_data() {
    use om_protocol::container::{self, ThumbnailReply, UploadBinary};
    use om_protocol::Resume;

    let copy = std::env::temp_dir().join(format!(
        "om-server-container-test-{}.fmp12",
        std::process::id()
    ));
    std::fs::copy(TEST_PROJECT_3, &copy).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let (mut a, file) = logged_in(port, "container-a");
    let contacts = vec![vec![0x80, 0x02]];
    let photo = vec![vec![0x03]];

    let picture = png(48, 48, false);
    let payload = binary_data(&[
        (b"FNAM", b"\x00\x00\x00\x01\x05?7;=?\x00\x05:+-*4"),
        (b"PNGf", &picture),
        (b"SIZE", &[0, 48, 0, 48]),
    ]);
    let upload = |session: &mut Session, payload: &[u8], resume: Resume| {
        let request = UploadBinary {
            payload: payload.to_vec(),
            resume,
            table: contacts.clone(),
            field: photo.clone(),
            solved_path: String::new(),
            reserved: [0; 16],
        };
        let reply = session
            .perform(&container::build_upload_binary(9, file, &request))
            .unwrap();
        container::parse_upload_binary_reply(&reply).unwrap()
    };

    // Insert a picture: a new member with count 1; the same data again
    // counts the existing member (status 0x1006).
    let stored = upload(&mut a, &payload, Resume::start());
    assert_eq!((stored.status, stored.error.as_str()), (0, ""));
    assert_eq!(stored.member.len(), 1);
    let member = stored.member[0].clone();
    let again = upload(&mut a, &payload, Resume::start());
    assert_eq!(again.status, container::LIBRARY_OBJECT_EXISTS);
    assert_eq!(again.member, stored.member);

    // FileMaker Pro reads it back like any key (6.1).
    let count = a
        .download(&[&[0x80, 0x02], &[0x1f], &[0x05], &member, &[0x12]])
        .unwrap();
    assert_eq!(count.value, [0, 0, 0, 2]);
    let stream = |session: &mut Session, member: &[u8]| {
        let reply = session
            .download(&[&[0x80, 0x02], &[0x1f], &[0x05], member, b"PNGf"])
            .unwrap();
        // Inline up to 1000 bytes, otherwise a chunked node (1.5, 6.1).
        match reply.node_state {
            om_protocol::NodeState::Terminal => reply.value,
            _ => om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&reply.slots))
                .unwrap()
                .root
                .to_owned_node()
                .assembled()
                .unwrap()
                .into_owned(),
        }
    };
    assert_eq!(stream(&mut a, &member), picture);

    // Data whose MD5 does not match is refused.
    // (The last key is SIZE; its value ends just before the closing 40.)
    let mut tampered = payload.clone();
    let end = tampered.len() - 2;
    tampered[end] ^= 1;
    let refused = upload(&mut a, &tampered, Resume::start());
    assert_ne!(refused.status, 0);
    assert!(refused.error.contains("MD5"), "{}", refused.error);
    assert!(refused.member.is_empty());

    // A thumbnail scaled to exactly the box, whole and in chunks (6.3).
    let thumbnail = |session: &mut Session, offset: u64, max_chunk: u32| {
        let request = container::DownloadThumbnail {
            reserved: [0; 4],
            height: 32,
            width: 64,
            member: vec![member.clone()],
            table: contacts.clone(),
            flags: 0,
            offset,
            max_chunk,
        };
        let reply = session
            .perform(&container::build_download_thumbnail(10, file, &request))
            .unwrap();
        container::parse_download_thumbnail_reply(&reply).unwrap()
    };
    let ThumbnailReply::Chunk {
        bytes: whole,
        width,
        height,
        status,
        return_flags,
        ..
    } = thumbnail(&mut a, 0, 0x000f_ffe6)
    else {
        panic!("no thumbnail");
    };
    assert_eq!((width, height, status, return_flags), (64, 32, 0, 0));
    let decoded = image::load_from_memory_with_format(&whole, image::ImageFormat::Png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (64, 32));
    let mut chunked = Vec::new();
    loop {
        let ThumbnailReply::Chunk { bytes, status, .. } =
            thumbnail(&mut a, chunked.len() as u64, 1000)
        else {
            panic!("no chunk");
        };
        assert!(bytes.len() <= 996);
        chunked.extend(bytes);
        if status == 0 {
            break;
        }
        assert_eq!(status, container::THUMBNAIL_MORE);
    }
    assert_eq!(chunked, whole);

    // A large picture arrives in two parts (3.3).
    let large = png(620, 620, true);
    let large_payload = binary_data(&[(b"PNGf", &large), (b"SIZE", &[2, 0x6c, 2, 0x6c])]);
    assert!(large_payload.len() > 0xF_FF00);
    let first = upload(&mut a, &large_payload[..0xF_0000], Resume::start());
    assert_eq!(first.status, container::UPLOAD_CONTINUES);
    assert!(first.member.is_empty() && first.resume.has_more());
    let second = upload(&mut a, &large_payload[0xF_0000..], first.resume);
    assert_eq!(second.status, 0);
    let large_member = second.member[0].clone();
    assert_ne!(large_member, member);
    assert!(stream(&mut a, &large_member) == large);

    // FileMaker Pro's own upload of a photo (capture om:122, a 37,422-byte
    // JPEG) is stored, and its thumbnail is a JPEG.
    let captured = std::fs::read("../../re/work/containers/capn/00122-op088/txn.bin").unwrap();
    let captured = container::parse_upload_binary_request(&captured).unwrap();
    let photo_member = upload(&mut a, &captured.payload, captured.resume).member[0].clone();
    let request = container::DownloadThumbnail {
        reserved: [0; 4],
        height: 120,
        width: 90,
        member: vec![photo_member],
        table: contacts.clone(),
        flags: 0,
        offset: 0,
        max_chunk: 0x000f_ffe6,
    };
    let reply = a
        .perform(&container::build_download_thumbnail(12, file, &request))
        .unwrap();
    let ThumbnailReply::Chunk { bytes, status, .. } =
        container::parse_download_thumbnail_reply(&reply).unwrap()
    else {
        panic!("no thumbnail of the photo");
    };
    assert_eq!(status, 0);
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (90, 120));

    // Release the references (5.3): count down to 0 deletes the member.
    let adjust = |session: &mut Session, path: Vec<Vec<u8>>, delta: i32| {
        let request = container::AdjustReferenceCount { path, delta };
        let reply = session
            .perform(&container::build_adjust_reference_count(11, file, &request))
            .unwrap();
        container::parse_adjust_reference_count_reply(&reply).unwrap()
    };
    let path = |m: &[u8]| vec![vec![0x80, 0x02], vec![0x1f], vec![0x05], m.to_vec()];
    assert_eq!(adjust(&mut a, path(&member), 0), 2);
    assert_eq!(adjust(&mut a, path(&member), -1), 1);
    assert_eq!(adjust(&mut a, path(&member), -1), 0);
    assert_eq!(adjust(&mut a, path(&member), -1), container::COUNT_FAILED);
    assert_eq!(
        thumbnail(&mut a, 0, 0x000f_ffe6),
        ThumbnailReply::Status(container::THUMBNAIL_MISSING)
    );
    // A path that is not a member changes nothing (FileMaker Server wrote
    // to the file's root for this one, containers.md 10).
    assert_eq!(adjust(&mut a, Vec::new(), 1), container::COUNT_FAILED);
    assert_eq!(
        a.download(&[&[0x12]]).unwrap().node_state,
        om_protocol::NodeState::Missing
    );
    a.disconnect().unwrap();

    // The library was written to the file: two members left.
    let saved = om_hbam::HbamFile::open(&copy).unwrap();
    let tree = saved.key_tree().unwrap();
    let header = tree.node(&[&[0x80, 0x02], &[0x1f], &[0x01]]).unwrap();
    assert_eq!(header.child(&[0x04]).unwrap().value.unwrap(), [0, 0, 0, 2]);
    let large_saved = tree
        .node(&[&[0x80, 0x02], &[0x1f], &[0x05], &large_member, b"PNGf"])
        .unwrap();
    assert!(large_saved.assembled().unwrap().as_ref() == large.as_slice());
    assert!(tree
        .node(&[&[0x80, 0x02], &[0x1f], &[0x05], &member])
        .is_none());
    std::fs::remove_file(&copy).unwrap();
}

/// Manage Database end to end: replay FileMaker Server's live schema probes
/// (`re/work/schema/live`, schema-path.md 10) against OpenMaker Server on a
/// copy of test project in the state they started from. s2b creates table
/// `80 07` with two occurrences, a relationship and five fields; s2c adds a
/// record, renames and indexes a field, deletes one, adds one, and asks for
/// the deferred work with op73; s2d deletes the table and restores the
/// catalogs. Every reply must equal FileMaker Server's, the other session
/// must receive the same notices, and the file must hold what FileMaker
/// Server's did.
#[test]
#[ignore = "requires the git-ignored schema probes and test project copy, with its password in OM_TEST_PROJECT_PASSWORD"]
fn replays_manage_database_as_filemaker_server_answered() {
    use std::collections::HashMap;
    const LIVE: &str = "../../re/work/schema/live";
    const PROBE_FILE: [u8; 4] = [0, 0, 0x01, 0x65];

    let copy = std::env::temp_dir().join(format!(
        "om-server-schema-test-{}.fmp12",
        std::process::id()
    ));
    std::fs::copy(TEST_PROJECT_3, &copy).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let mut sessions: HashMap<String, (Session, u32)> = HashMap::new();
    for name in ["A", "B"] {
        sessions.insert(name.into(), logged_in(port, &format!("schema-{name}")));
    }
    let file = sessions["A"].1;
    // The probe's file id 0x165, in the header and in the body, becomes ours.
    let ours = file.to_be_bytes();
    let patch = |bytes: &[u8]| -> Vec<u8> {
        let mut out = bytes.to_vec();
        let mut i = 0;
        while i + 4 <= out.len() {
            if out[i..i + 4] == PROBE_FILE {
                out[i..i + 4].copy_from_slice(&ours);
                i += 4;
            } else {
                i += 1;
            }
        }
        out
    };
    let hex = |b: &[u8]| -> String { b.iter().map(|b| format!("{b:02x}")).collect() };
    let path_of = |text: &str| -> Vec<Vec<u8>> {
        text.split('/')
            .map(|s| {
                (0..s.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                    .collect()
            })
            .collect()
    };
    // The subtree without the index nodes OpenMaker Server builds for
    // fields defined as indexed in a new table, which FileMaker Server does
    // not (schema-path.md 12.1).
    let without_indexes = |slots: &[u8], fields: &[u8]| -> Vec<u8> {
        let mut node = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(slots))
            .unwrap()
            .root
            .to_owned_node();
        for field in fields {
            node.remove(&[&[0x01], &[*field]]);
        }
        if node.child(&[0x01]).is_some_and(|n| n.children.is_empty()) {
            node.remove(&[&[0x01]]);
        }
        node.to_wire().unwrap()
    };

    // DeleteTable carries an access key derived from the session's
    // connection key (query-program.md 2.4): the probe's becomes ours.
    let access_key = |session: &Session, req: Vec<u8>| -> Vec<u8> {
        let mut req = req;
        if let Some(at) = req.windows(3).position(|w| w == [0x06, 0x07, 0x10]) {
            let key = om_crypto::query_access_key(1, session.connection_key(), file);
            req[at + 3..at + 19].copy_from_slice(&key);
        }
        req
    };

    let mut compared = 0;
    for (run, script) in [
        ("s2b", "s2b-create.txt"),
        ("s2c", "s2c-record.txt"),
        ("s2d", "s2d-delete.txt"),
    ] {
        let dir = format!("{LIVE}/{run}");
        let script = std::fs::read_to_string(format!("{LIVE}/{script}")).unwrap();
        let mut current = String::from("A");
        for line in script.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["session" | "use", name] => current = name.to_string(),
                ["rn"] => {
                    let req = std::fs::read(format!("{dir}/{current}-rn.req")).unwrap();
                    let (session, _) = sessions.get_mut(&current).unwrap();
                    let reply = session.perform(&patch(&req)).unwrap();
                    assert_eq!(reply, [0, 0], "{run} {current}-rn");
                }
                [command, tag, ..]
                    if ["tx", "q86", "up13", "upload", "commit", "ts"].contains(command) =>
                {
                    let req = std::fs::read(format!("{dir}/{tag}.req")).unwrap();
                    let rep = std::fs::read(format!("{dir}/{tag}.rep")).unwrap();
                    let (session, _) = sessions.get_mut(&current).unwrap();
                    let req = access_key(session, patch(&req));
                    let reply = session.perform(&req).unwrap();
                    // op82 answers the time, which differs.
                    if *command != "ts" {
                        assert_eq!(hex(&reply), hex(&patch(&rep)), "{run} {tag}");
                    }
                    compared += 1;
                }
                ["dl" | "dl15", tag, path] => {
                    let path = path_of(path);
                    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
                    let (session, _) = sessions.get_mut(&current).unwrap();
                    let request = om_protocol::build_download(&om_protocol::DownloadRequest {
                        transaction_id: 99,
                        file_id: file,
                        key_path: &segments,
                        resume: om_protocol::Resume::start(),
                    });
                    let reply = session.perform_reply(&request).unwrap();
                    // A download by the other session collects its notices,
                    // recorded as `<tag>.bc<request id>`.
                    let notices = std::fs::read_dir(&dir)
                        .unwrap()
                        .map(|e| e.unwrap().path())
                        .find(|p| {
                            let name = p.file_name().unwrap().to_string_lossy().into_owned();
                            name.strip_prefix(&format!("{tag}.bc")).is_some_and(|id| {
                                !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
                            })
                        });
                    if let Some(notices) = notices {
                        let expected = std::fs::read(notices).unwrap();
                        assert_eq!(hex(&reply.backchannel), hex(&expected), "{run} {tag}");
                        compared += 1;
                    }
                    let recorded = format!("{dir}/{tag}.slots");
                    let compare = [
                        "v_3",
                        "v_8007",
                        "c_after_rec",
                        "c_before73",
                        "c_after73",
                        "d_after_deltable",
                    ];
                    if compare.contains(tag) {
                        let reply = om_protocol::parse_download_reply(&reply.result).unwrap();
                        let expected = std::fs::read(&recorded).unwrap();
                        let ours = if *tag == "v_3" || expected.is_empty() {
                            reply.slots
                        } else {
                            without_indexes(&reply.slots, &[0x01, 0x03])
                        };
                        assert!(ours == expected, "{run} {tag}: the file differs");
                        compared += 1;
                    }
                }
                _ => {}
            }
        }
    }
    assert!(compared > 40, "{compared} comparisons");

    for (_, (session, _)) in sessions {
        session.disconnect().unwrap();
    }
    // s2d put the catalogs back as they were: the saved file's schema is the
    // starting one again, and the table is gone.
    let saved = om_hbam::HbamFile::open(&copy).unwrap();
    let tree = saved.key_tree().unwrap();
    let schema = tree.node(&[&[0x03]]).unwrap().to_wire().unwrap();
    assert!(schema == std::fs::read(format!("{LIVE}/s0/b_3.slots")).unwrap());
    assert!(tree.node(&[&[0x80, 0x07]]).is_none());
    std::fs::remove_file(&copy).unwrap();
}

/// GenerateMemberKey (schema-path.md 3.4): FileMaker Server's live answers
/// to FileMaker Pro's request (live:s4, s4b), replayed against OpenMaker
/// Server on a copy of test project with the same catalog indexes.
#[test]
#[ignore = "requires the git-ignored schema probes and test project copy, with its password in OM_TEST_PROJECT_PASSWORD"]
fn generates_member_keys_as_filemaker_server_does() {
    const LIVE: &str = "../../re/work/schema/live/s4";
    // The issued ids are written to the file, so the server hosts a copy.
    let copy = std::env::temp_dir().join(format!(
        "om-server-genkey-test-{}.fmp12",
        std::process::id()
    ));
    std::fs::copy(TEST_PROJECT_3, &copy).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let (mut session, file) = logged_in(port, "genkey");
    for tag in ["g_gen1", "g_gen2", "h_gen16", "h_gen4"] {
        let mut req = std::fs::read(format!("{LIVE}/{tag}.req")).unwrap();
        let rep = std::fs::read(format!("{LIVE}/{tag}.rep")).unwrap();
        // The probe's file id, in the header and the instruction, is ours.
        let probe_file: [u8; 4] = req[12..16].try_into().unwrap();
        for at in 0..req.len() - 3 {
            if req[at..at + 4] == probe_file {
                req[at..at + 4].copy_from_slice(&file.to_be_bytes());
            }
        }
        assert_eq!(session.perform(&req).unwrap(), rep, "{tag}");
    }
    // The catalogs hold what was issued (live:s4/g_41_after, s5/m_41fc),
    // and op60 reports the layout index's new count (s5/m_lock41).
    let value = |session: &mut Session, path: &[&[u8]]| session.download(path).unwrap().value;
    assert_eq!(value(&mut session, &[&[4], &[1], &[0]]), [1, 7]);
    assert_eq!(value(&mut session, &[&[4], &[1], &[0xfc]]), [1, 0x10]);
    assert_eq!(
        value(&mut session, &[&[3], &[0x10], &[1], &[0]]),
        [2, 0x80, 0x06]
    );
    let lock = std::fs::read("../../re/work/schema/live/s5/m_lock41.req").unwrap();
    let mut lock = lock;
    let probe_file: [u8; 4] = lock[12..16].try_into().unwrap();
    for at in 0..lock.len() - 3 {
        if lock[at..at + 4] == probe_file {
            lock[at..at + 4].copy_from_slice(&file.to_be_bytes());
        }
    }
    assert_eq!(
        session.perform(&lock).unwrap(),
        std::fs::read("../../re/work/schema/live/s5/m_lock41.rep").unwrap()
    );
    session.disconnect().unwrap();
    std::fs::remove_file(&copy).unwrap();
}

/// FileMaker Pro's save of a field change that also rewrote a layout
/// (capture s06-omserver-p, schema-path.md 2.3a): its last op13 stores one
/// value with no slots, which OpenMaker Server once answered with a
/// continuation handle, so FileMaker Pro repeated it until force-quit. The
/// recorded requests now get one complete answer each.
#[test]
#[ignore = "requires the git-ignored capture and test project copy, with its password in OM_TEST_PROJECT_PASSWORD"]
fn completes_the_value_upload_that_looped() {
    const CAPTURE: &str = "../../re/work/schema/capp";
    let copy = std::env::temp_dir().join(format!(
        "om-server-value-upload-{}.fmp12",
        std::process::id()
    ));
    std::fs::copy(TEST_PROJECT_3, &copy).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let (mut session, file) = logged_in(port, "value-upload");
    assert_eq!(file, 1, "the capture's file id");
    // op60 [4, 5, 3] and [4, 1], op61, op13 of the layout, op62, op61, then
    // the op13 of the value [4, 1, 00].
    for rid in [68, 70, 72, 74, 76, 78] {
        let dir = std::fs::read_dir(CAPTURE)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{rid:05}-"))
            })
            .unwrap();
        let reply = session
            .perform(&std::fs::read(dir.join("txn.bin")).unwrap())
            .unwrap();
        if rid == 70 {
            // The layout index's modification count, as FileMaker Server
            // gives it (schema-path.md 4.1).
            assert_eq!(reply, [0x01, 0x0d, 0x02, 0x00, 0x00]);
        }
    }
    let value_upload = std::fs::read(format!("{CAPTURE}/00080-op013/txn.bin")).unwrap();
    let reply = session.perform(&value_upload).unwrap();
    assert_eq!(
        reply,
        om_protocol::write::upload_reply(&om_protocol::Resume::no_stream(), usize::MAX),
        "one complete answer, no continuation"
    );
    let stored = session.download(&[&[4], &[1], &[0]]).unwrap();
    assert_eq!(stored.value, [1, 4]);
    session.disconnect().unwrap();
    std::fs::remove_file(&copy).unwrap();
}

/// Indexes are built on demand (query-program.md 7.1). FileMaker Pro showed
/// a portal of Test Table rows through a field with "create indexes as
/// needed" and no index yet (capture s06-omserver-q rid 836, JoinProjection
/// on `contactForiegnKey`); OpenMaker Server answered empty and the rows
/// vanished. Replayed on a snapshot of that file without the index, the
/// lookup finds the rows and the index is stored.
#[test]
#[ignore = "requires the git-ignored capture and file snapshot, with its password in OM_TEST_PROJECT_PASSWORD"]
fn builds_the_index_a_portal_reads() {
    const SNAPSHOT: &str = "../../re/work/omserver/testproject-testtable.fmp12";
    const JOIN: &str = "../../re/work/schema/capq/00836-op086/txn.bin";
    let test_table: &[u8] = &[0x80, 0x06];
    // The snapshot without contactForiegnKey's index, as before the maintainer
    // indexed it.
    let file = om_hbam::HbamFile::open(SNAPSHOT).unwrap();
    let mut root = file.key_tree().unwrap().root.to_owned_node();
    root.remove(&[test_table, &[0x01], &[0x02]]).unwrap();
    let bytes = om_hbam::writer::rebuild(&root, file.superblock().raw).unwrap();
    let copy = std::env::temp_dir().join(format!(
        "om-server-portal-index-{}.fmp12",
        std::process::id()
    ));
    std::fs::write(&copy, bytes).unwrap();
    let port = start(vec![HostedFile {
        name: "test project.fmp12".into(),
        path: copy.clone(),
    }]);
    let (mut session, file_id) = logged_in(port, "portal-index");
    assert_eq!(file_id, 1, "the capture's file id");
    let mut join = std::fs::read(JOIN).unwrap();
    let at = join
        .windows(3)
        .position(|w| w == [0x06, 0x07, 0x10])
        .unwrap();
    let key = om_crypto::query_access_key(1, session.connection_key(), file_id);
    join[at + 3..at + 19].copy_from_slice(&key);

    let reply = om_protocol::parse_query_reply(&session.perform(&join).unwrap()).unwrap();
    let result = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&reply.slots))
        .unwrap()
        .root
        .to_owned_node();
    let found = om_hbam::RidList::from_owned(result.at(&[&[0x02]]).unwrap()).unwrap();
    assert_eq!(
        found.iter().collect::<Vec<_>>(),
        [2, 3],
        "the portal's rows"
    );
    let index = session
        .download(&[test_table, &[0x01], &[0x02], &[0x00]])
        .unwrap();
    assert_eq!(
        index.node_state,
        om_protocol::NodeState::NonTerminal,
        "stored"
    );
    session.disconnect().unwrap();
    std::fs::remove_file(&copy).unwrap();
}

/// A new connection with a throwaway client certificate and no file open.
fn connected(port: u16) -> Session {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap()
}

/// Open `file` on `session`, log in as Admin and register for its change
/// notifications under the handle's client file id; the file's id.
fn open_and_log_in(session: &mut Session, file: &str) -> u32 {
    let open = session.open_file("127.0.0.1", file).unwrap();
    assert_eq!(open.status, 0, "{open:?}");
    let creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", file);
    let login = session
        .login(fixture::ACCOUNT, fixture::PASSWORD, creation, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    session.request_notifications().unwrap();
    open.file_id
}

/// A request that reads nothing of a file (GetGuestCount, op84, which the
/// server does not implement): its reply carries what the server has
/// waiting for the connection.
fn probe(session: &mut Session) {
    let probe = om_rctransaction::Builder::new(Opcode::GetGuestCount as u8).finish();
    session.perform(&probe).unwrap();
}

/// A change of `path` in file `file`, announced by `sender` to the other
/// clients registered for the file's notifications (attention code 2);
/// the key form a notification names the path by.
fn announce(sender: &mut Session, file: u32, last: u8) -> Vec<u8> {
    use om_protocol::write;
    let path = vec![vec![0x42; 250], vec![0, last]];
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    sender.lock(&segments, 1, false).unwrap();
    let unlock = write::Unlock {
        message_address: 0,
        attention_mask: 2,
        path: path.clone(),
        host_file: file as i32,
        txn: 1,
        lock_type: write::LOCK_WRITE,
    };
    sender
        .perform(&write::build_unlock(1, file, &unlock))
        .unwrap();
    server::path_key(&segments)
}

/// The Perform Script on Server results `session` gets within 10 s, at
/// least one.
fn results(session: &mut Session) -> Vec<(u32, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut got = Vec::new();
    while got.is_empty() {
        assert!(std::time::Instant::now() < deadline, "no callback result");
        session.poll(std::time::Duration::from_millis(100)).unwrap();
        got.extend(
            session
                .take_psos_results()
                .into_iter()
                .map(|r| (r.callback, r.result)),
        );
    }
    got
}

/// Two files on one connection, as FileMaker Pro opens a file's related
/// files in the session it has (network-protocol.md 5.5): each handle
/// reads its own file and gets the change notifications that name its
/// client file id and the results of its own callbacks, the same ids
/// from both included; the connection's transaction ids are one
/// sequence; the server sees one client; closing one file keeps the
/// other and drops what was coming for the closed one.
#[test]
fn two_files_share_one_connection() {
    let tasks = fixture::TempFile::new("shared-tasks", &[fixture::slow(0.0)], true);
    let mut slower = fixture::slow(0.5);
    slower.id = 4;
    slower.name = "Slower".into();
    let mut more = fixture::tree(
        &[
            fixture::script(3, "Quick", vec![fixture::exit_with("\"other done\"")]),
            slower,
        ],
        true,
    );
    fixture::add_tasks(&mut more, 3..=5);
    let other = fixture::TempFile::from_tree("shared-other", &more);
    let (port, handle) = server::start_hosting(
        &[(fixture::FILE, &tasks), ("Other.fmp12", &other)],
        |server| server,
    );
    let mut first = connected(port);
    open_and_log_in(&mut first, fixture::FILE);
    let mut second = first.share();
    open_and_log_in(&mut second, "Other.fmp12");
    // Each op63 names its own client file id: -(its transaction id + 2).
    let (a, b) = (
        first.client_file_id().unwrap(),
        second.client_file_id().unwrap(),
    );
    assert!(a < -2 && b < -2 && a != b, "{a} {b}");
    assert_eq!(first.files_open(), 2);
    let clients = handle.clients();
    assert_eq!(clients.len(), 1, "one connection");
    assert_eq!(clients[0].files.len(), 2, "{:?}", clients[0].files);

    // Each handle reads its own file: Other has three more records.
    let own = server::records_of(&mut first, fixture::TABLE).len();
    assert_eq!(
        server::records_of(&mut second, fixture::TABLE).len(),
        own + 3
    );
    assert_ne!(first.take_transaction(), second.take_transaction());

    // Another client's change to Other is announced to the second handle
    // alone, whichever handle's request brings it.
    let (mut sender, sender_file) = server::logged_in_to(port, "sender", "Other.fmp12");
    let changed = announce(&mut sender, sender_file, 1);
    probe(&mut first);
    assert!(first.take_notifications().is_empty());
    let notified: Vec<_> = second
        .take_notifications()
        .iter()
        .map(server::notified)
        .collect();
    assert_eq!(notified, [(2, changed)]);

    // Both handles' callback 1: each result comes back to its own handle,
    // under that id.
    for session in [&mut first, &mut second] {
        let started = session
            .perform_script_on_server_with_callback(3, "", 1)
            .unwrap();
        assert_eq!(started.error, 0, "{started:?}");
    }
    assert_eq!(results(&mut first), [(1, "slow done".to_string())]);
    assert_eq!(results(&mut second), [(1, "other done".to_string())]);

    // Closing the second file (its handle dropped) keeps the first, which
    // gets nothing of what was still coming for the second.
    let started = second
        .perform_script_on_server_with_callback(4, "", 2)
        .unwrap();
    assert_eq!(started.error, 0, "{started:?}");
    drop(second);
    assert_eq!(first.files_open(), 1);
    assert_eq!(handle.clients()[0].files.len(), 1);
    assert_eq!(server::records_of(&mut first, fixture::TABLE).len(), own);
    first.poll(std::time::Duration::from_secs(2)).unwrap();
    assert!(first.take_psos_results().is_empty());
    first.tickle().unwrap();
    first.disconnect().unwrap();

    // One handle registered by hand (client file id -4, as the fixture
    // does): a notification that came for its file is dropped when the
    // file closes, not handed to the next file it opens.
    let (mut solo, _) = server::logged_in_to(port, "solo", fixture::FILE);
    let (mut tasks_sender, tasks_file) = server::logged_in_to(port, "tasks-sender", fixture::FILE);
    announce(&mut tasks_sender, tasks_file, 2);
    probe(&mut solo);
    solo.close_file().unwrap();
    open_and_log_in(&mut solo, "Other.fmp12");
    probe(&mut solo);
    assert!(solo.take_notifications().is_empty());
    solo.disconnect().unwrap();
    tasks_sender.disconnect().unwrap();
    sender.disconnect().unwrap();
}

/// A file refused on a shared connection fails alone: a wrong password
/// for it leaves the connection and the other file open. Losing the
/// connection loses every file on it.
#[test]
fn a_refused_file_on_a_shared_connection_fails_alone() {
    let tasks = fixture::TempFile::new("refused-tasks", &[], true);
    let other = fixture::TempFile::new("refused-other", &[], true);
    let (port, handle) = server::start_hosting(
        &[(fixture::FILE, &tasks), ("Other.fmp12", &other)],
        |server| server,
    );
    let mut first = connected(port);
    open_and_log_in(&mut first, fixture::FILE);
    let mut refused = first.share();
    refused.open_file("127.0.0.1", "Other.fmp12").unwrap();
    let creation = refused.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "Other.fmp12");
    let login = refused
        .login(fixture::ACCOUNT, "not the password", creation, &environment)
        .unwrap();
    assert!(!login.is_success(), "{login:?}");
    refused.disconnect().unwrap();
    assert_eq!(first.files_open(), 1);
    assert_eq!(handle.clients()[0].files.len(), 1);
    assert!(!server::records_of(&mut first, fixture::TABLE).is_empty());

    let mut second = first.share();
    open_and_log_in(&mut second, "Other.fmp12");
    assert!(handle.disconnect_client(handle.clients()[0].id));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while first.connection_lost().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the connection stayed"
        );
        let _ = first.tickle();
    }
    assert!(second.connection_lost().is_some());
    assert!(second
        .download(&[&om_hbam::slot::uint_segment(fixture::TABLE), &[5]])
        .is_err());
}

/// op88 stores container data only where the account's set lets it write
/// the records that would hold it (#227): Temp's custom set creates and
/// edits Tasks records but views Notes only. An upload to Tasks is stored;
/// one to Notes, or to the file's own library (any table's records may
/// hold it), is refused with 200, also when the data is already there,
/// which would raise the member's count as op87 would not let it. Mixed's
/// set creates Tasks records and edits Notes records, each by the table's
/// own entry, while its default views only: each table allows one of the
/// two, so the file's library takes its upload.
#[test]
fn container_uploads_need_the_tables_create_or_edit_privilege() {
    use om_hbam::slot::uint_segment;
    use om_protocol::container::{self, UploadBinary};
    use om_protocol::Resume;

    let mut root = fixture::tree(&[], true);
    fixture::add_custom_clerk(&mut root);
    // Set 5 as `add_custom_clerk`'s set 4, but records viewed only by
    // default (`10 03`), Tasks' viewed and created (`50 03`), Notes' viewed
    // and edited (`14 03`); account record 8.
    let set: &[u8] = &[5];
    for (path, value) in [
        (vec![vec![2u8]], vec![0x10u8, 0x03]),
        (
            vec![vec![5], uint_segment(fixture::TABLE), vec![2]],
            vec![0x50, 0x03],
        ),
        (vec![vec![0x0a]], vec![0x9f, 0xff]),
        (vec![vec![0x0c]], vec![0, 0]),
        (
            vec![vec![5], uint_segment(fixture::NOTES), vec![2]],
            vec![0x14, 0x03],
        ),
        (vec![vec![9], vec![0x04]], vec![0xdf]),
        (vec![vec![9], vec![0x11]], vec![0xd7]),
        (vec![vec![9], vec![0x21]], vec![0xd7]),
    ] {
        let mut full: Vec<&[u8]> = vec![&[23], &[2], &[5], set];
        full.extend(path.iter().map(Vec::as_slice));
        root.put(&full, om_hbam::OwnedNode::leaf(Vec::new(), value));
    }
    root.put(
        &[&[23], &[2], &[5], set, &[16]],
        om_scripts::text_node(&[], "Mixed"),
    );
    root.put(
        &[&[23], &[1], &[5], &[8], &[16]],
        om_scripts::text_node(&[], "Mixed"),
    );
    fixture::put_account(&mut root, 8, "mixed secret", None, 5);
    let file = fixture::TempFile::from_tree("upload-privileges", &root);
    let port = server::start_configured(&file, |server| server.admitting_custom_sets());
    let (mut admin, admin_file) = server::logged_in(port, "admin");
    let (mut temp, temp_file) = server::logged_in_as(
        port,
        "temp",
        fixture::FILE,
        fixture::TEMP,
        fixture::TEMP_PASSWORD,
    );
    // Streams in key order.
    let payload = binary_data(&[
        (b"FILE", b"hello"),
        (b"FNAM", &om_data::containers::fnam("file", "a.txt")),
    ]);
    let upload = |session: &mut Session, file: u32, table: Vec<Vec<u8>>| {
        let request = UploadBinary {
            payload: payload.clone(),
            resume: Resume::start(),
            table,
            field: vec![vec![1]],
            solved_path: String::new(),
            reserved: [0; 16],
        };
        let reply = session
            .perform(&container::build_upload_binary(9, file, &request))
            .unwrap();
        container::parse_upload_binary_reply(&reply).unwrap()
    };
    let tasks = vec![uint_segment(fixture::TABLE)];
    let notes = vec![uint_segment(fixture::NOTES)];

    let stored = upload(&mut temp, temp_file, tasks);
    assert_eq!((stored.status, stored.member.len()), (0, 1), "{stored:?}");
    for table in [notes.clone(), Vec::new()] {
        let refused = upload(&mut temp, temp_file, table.clone());
        assert_eq!(refused.status, 200, "{table:02x?}");
        assert!(refused.member.is_empty());
    }
    // Admin stores the data in Notes; Temp's copy of it is refused and the
    // member keeps the one reference Admin's upload counted.
    let first = upload(&mut admin, admin_file, notes.clone());
    assert_eq!(first.status, 0, "{first:?}");
    let again = upload(&mut temp, temp_file, notes.clone());
    assert_eq!(again.status, 200);
    let count = [
        notes[0].as_slice(),
        &[0x1f],
        &[5],
        &first.member[0],
        &[0x12],
    ];
    assert_eq!(
        admin.download(&count).unwrap().value,
        1u32.to_be_bytes().to_vec()
    );
    temp.disconnect().unwrap();
    let (mut mixed, mixed_file) =
        server::logged_in_as(port, "mixed", fixture::FILE, "Mixed", "mixed secret");
    let stored = upload(&mut mixed, mixed_file, Vec::new());
    assert_eq!((stored.status, stored.member.len()), (0, 1), "{stored:?}");
    mixed.disconnect().unwrap();
    admin.disconnect().unwrap();
}
