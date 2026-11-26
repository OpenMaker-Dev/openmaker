//! End to end: administering a running server (`docs/admin-console.md`,
//! "Administration in om-server"). The overview, file and client
//! snapshots of `ServerHandle` follow `om-session` clients through Connect,
//! Open, Login, Logout and Disconnect, and `disconnect_client` ends a
//! client's session and closes its connection, however the client goes on
//! sending. A connection whose request handler panics ends the same way.
//! `send_message` reaches the client in a `Notify` call, and Block New
//! Users refuses new sessions' Opens. `disconnect_client_with_notice` asks
//! a client to close its files and ends its session quietly once it has,
//! or forces it out when its grace time ends. `close_file` asks a file's
//! clients to close it, `open_file` loads it again, and `pause_file` holds
//! its requests until `resume_file` while everything else is answered.
//! The event log records the server, its files and its clients' sessions
//! in FileMaker Server's format, and a file that cannot be opened, or that
//! anyone could open with full access, raises an alert.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use fixture::{TempFile, ACCOUNT, FILE, INTERNAL_CREATION, NAME, PASSWORD, TABLE};
use om_giop::ReadError;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_protocol::notify::AdminMessage;
use om_protocol::{write, LoginEnvironment, LoginRequest, MachineInfo, Opcode};
use om_server::{
    ClientFile, ConnectionLimits, DisconnectError, FileError, FileInfo, FileStatus, HostedFile,
    Identity, Level, LogReadError, MessageError, Notice, ScriptLimits, Server, ServerConfig,
    ServerHandle, ServerIdentity,
};
use om_session::{Session, SessionError, Transport, TransportError};

/// Connect as FileMaker Pro 26.0.3 on `machine`.
fn connect(port: u16, machine: &str) -> Session {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo {
            app_type: 0x01,
            machine_name: machine.into(),
            version_string: "Pro 26.0.3".into(),
            ..MachineInfo::default()
        },
    )
    .unwrap()
}

/// Wait up to five seconds for `done`.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `error` says the server closed the connection, rather than that
/// it was slow to answer.
fn closed(error: &SessionError) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    match error {
        SessionError::ConnectionLost(_) => true,
        SessionError::Transport(TransportError::Read(ReadError::Closed)) => true,
        SessionError::Transport(TransportError::Io(e) | TransportError::Read(ReadError::Io(e))) => {
            matches!(
                e.kind(),
                BrokenPipe | ConnectionAborted | ConnectionReset | UnexpectedEof
            )
        }
        _ => false,
    }
}

/// The client's next request goes unanswered: its connection is closed.
fn assert_disconnected(session: &mut Session) {
    match session.download(&[&[2]]) {
        Err(e) => assert!(closed(&e), "{e:?}"),
        Ok(_) => panic!("a disconnected client's request was answered"),
    }
}

/// Whether `session` can lock record 1 of Tasks now (the lock is released
/// again).
fn record_1_free(session: &mut Session) -> bool {
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    let granted = matches!(
        session.lock(&path, 0x41, false).unwrap(),
        write::LockReply::Granted { .. }
    );
    session.kill_all_locks(0x41).unwrap();
    granted
}

/// The lines of the log file `name`, oldest first, as
/// `<level> <number> <message>`.
fn log_lines(handle: &ServerHandle, name: &str) -> Vec<String> {
    let page = handle.read_log(name, None, 100).unwrap();
    assert_eq!(page.next, None);
    page.entries
        .iter()
        .rev()
        .map(|entry| format!("{} {} {}", entry.level, entry.number, entry.message))
        .collect()
}

#[cfg(all(unix, feature = "admin-console"))]
#[test]
fn the_databases_filter_uses_the_resolved_folder_of_a_file_opened_through_a_link() {
    use om_admin::Backend;
    use om_server::console::ServerBackend;

    let fixture = TempFile::new("admin-folder-alias", &[], true);
    let folder = fixture.0.with_extension("databases");
    let alias = fixture.0.with_extension("alias");
    std::fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&folder, &alias).unwrap();
    let file = TempFile(alias.join(FILE));
    std::fs::copy(&fixture.0, &file.0).unwrap();
    let root = folder.canonicalize().unwrap();
    let (_, handle) = server::start_configured_with_handle(&file, |server| {
        server.with_database_folders(vec![root.clone()])
    });

    let backend = ServerBackend::new(handle, Vec::new());
    let folders = backend.folders().unwrap();
    let [database] = backend.databases().try_into().unwrap();
    assert_eq!(folders.databases.as_slice(), std::slice::from_ref(&root));
    assert_eq!(database.folder, root);

    drop(file);
    std::fs::remove_file(alias).unwrap();
    std::fs::remove_dir(folder).unwrap();
}

/// Also checks that `Access.log` records each session's login, refused or
/// accepted, the files it logs in to and out of, and its end, in FileMaker
/// Server's form (`docs/re/server-logs.md` 3).
#[test]
fn snapshots_follow_clients_through_their_sessions() {
    let started = SystemTime::now();
    let file = TempFile::new("admin", &[], true);
    let folder = file.0.with_extension("logs");
    // A database folder the server looked in that holds no hosted file.
    let databases = file.0.with_extension("databases");
    std::fs::create_dir_all(&databases).unwrap();
    let (port, handle) = server::start_configured_with_handle(&file, |server| {
        server
            .with_event_log(&folder)
            .unwrap()
            .with_database_folders(vec![databases.clone()])
    });

    let overview = handle.overview();
    assert_eq!(
        (
            overview.name.as_str(),
            overview.version.as_str(),
            overview.server_id.as_str(),
            overview.address,
            overview.hosted_files,
        ),
        (
            "openmaker-test",
            "OpenMaker Server test",
            "0123456789abcdef0123456789abcdef",
            SocketAddr::from(([127, 0, 0, 1], port)),
            1,
        )
    );
    assert!(started <= overview.started_at && overview.started_at <= SystemTime::now());
    let hosted = |clients| FileInfo {
        id: 1,
        name: FILE.into(),
        path: file.0.clone(),
        folder: std::env::temp_dir(),
        size: Some(std::fs::metadata(&file.0).unwrap().len()),
        status: FileStatus::Open,
        clients,
        error: None,
        read_only: None,
    };
    assert_eq!(handle.files(), [hosted(0)]);
    assert!(handle.clients().is_empty());

    // Front Desk connects and opens the file: listed, not logged in yet.
    let before = SystemTime::now();
    let mut front = connect(port, "Front Desk");
    let connected = SystemTime::now();
    let open = front.open_file("127.0.0.1", FILE).unwrap();
    let [client] = handle.clients().try_into().unwrap();
    assert_eq!(client.id, front.identity().connection_id);
    assert_eq!(client.address, front.local_addr().unwrap());
    assert_eq!(
        (
            client.machine_name.as_str(),
            client.app_type,
            client.version.as_str(),
            client.session_name,
            client.disconnecting,
        ),
        ("Front Desk", 0x01, "Pro 26.0.3", None, false)
    );
    assert!(before <= client.connected_at && client.connected_at <= connected);
    let file_open = |account: Option<&str>| ClientFile {
        id: open.file_id,
        name: FILE.into(),
        account: account.map(Into::into),
    };
    assert_eq!(client.files, [file_open(None)]);

    // Login names the account and the session; a refused one changes
    // nothing.
    assert_eq!(front.fetch_internal_creation().unwrap(), INTERNAL_CREATION);
    let environment = LoginEnvironment {
        session_name: "Front Desk session".into(),
        ..LoginEnvironment::for_remote_file("127.0.0.1", FILE)
    };
    let refused = front
        .login(ACCOUNT, "s3cret guess", INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(!refused.is_success(), "{refused:?}");
    assert_eq!(handle.clients()[0].files, [file_open(None)]);
    let login = front
        .login(ACCOUNT, PASSWORD, INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    let [client] = handle.clients().try_into().unwrap();
    assert_eq!(client.session_name.as_deref(), Some("Front Desk session"));
    assert_eq!(client.files, [file_open(Some(ACCOUNT))]);

    // The Admin Console reads the same through its backend.
    #[cfg(feature = "admin-console")]
    {
        use om_admin::{Backend, BackendError, DatabaseStatus};
        let console = om_server::console::ServerBackend::new(handle.clone(), Vec::new());
        let [seen] = console.clients().try_into().unwrap();
        assert_eq!(
            (
                seen.id,
                seen.session_name.as_deref(),
                seen.files[0].account.as_deref()
            ),
            (client.id, Some("Front Desk session"), Some(ACCOUNT))
        );
        let [database] = console.databases().try_into().unwrap();
        assert_eq!(
            (database.id, database.status, database.clients),
            (1, DatabaseStatus::Normal, 1)
        );
        assert_eq!(
            console.disconnect_client(0xdead, None),
            Err(BackendError::NotFound)
        );
        assert_eq!(console.run_schedule(2), Err(BackendError::NotFound));
        // Database Folders lists the empty database folder, and the one
        // of the file named on the command line (#430).
        let folders = console.folders().unwrap();
        assert!(folders.databases.contains(&databases), "{folders:?}");
        assert_eq!(folders.databases.len(), 2, "{folders:?}");
    }

    // A second client: both listed, oldest first, and both have the file
    // open.
    let (mut back, _) = server::logged_in(port, "Back Office");
    let machines: Vec<String> = handle
        .clients()
        .into_iter()
        .map(|client| client.machine_name)
        .collect();
    assert_eq!(machines, ["Front Desk", "Back Office"]);
    assert_eq!(handle.files(), [hosted(2)]);

    // Logout leaves the file open without an account.
    let mut header = om_rctransaction::Header::new(Opcode::Logout as u8);
    header.file_id = open.file_id;
    let logout = om_rctransaction::Builder::with_header(header).finish();
    assert_eq!(back.perform(&logout).unwrap(), [0, 0]);
    let back_id = back.identity().connection_id;
    let clients = handle.clients();
    assert_eq!(clients[1].id, back_id);
    assert_eq!(clients[1].files, [file_open(None)]);
    assert_eq!(handle.files(), [hosted(2)]);

    // Clients that leave are no longer listed.
    front.disconnect().unwrap();
    wait_until("Front Desk left", || handle.clients().len() == 1);
    assert_eq!(handle.clients()[0].id, back_id);
    assert_eq!(handle.files(), [hosted(1)]);
    back.disconnect().unwrap();
    wait_until("Back Office left", || handle.clients().is_empty());
    assert_eq!(handle.files(), [hosted(0)]);

    // Access.log, written as it happened; never with a password.
    let front = "Front Desk session (Front Desk) [127.0.0.1]";
    let back = "openmaker (Back Office) [127.0.0.1]";
    let expected = [
        format!(
            "Warning 661 SECURITY: Client \"{front}\" authentication failed on database \
             \"Tasks\" using \"Admin\"."
        ),
        "Information 638 Client \"Front Desk session\" opening a connection from \
         \"Front Desk (127.0.0.1)\" using \"Pro 26.0.3 [fmapp]\"."
            .to_owned(),
        format!("Information 94 Client \"{front}\" opening database \"Tasks\" as \"Admin\"."),
        "Information 638 Client \"openmaker\" opening a connection from \
         \"Back Office (127.0.0.1)\" using \"OpenMaker 0.0 [fmapp]\"."
            .to_owned(),
        format!("Information 94 Client \"{back}\" opening database \"Tasks\" as \"Admin\"."),
        format!("Information 98 Client \"{back}\" closing database \"Tasks\" as \"Admin\"."),
        format!("Information 98 Client \"{front}\" closing database \"Tasks\" as \"Admin\"."),
        format!("Information 22 Client \"{front}\" closing a connection."),
        format!("Information 22 Client \"{back}\" closing a connection."),
    ];
    wait_until("Access.log is written", || {
        log_lines(&handle, "Access.log").len() == expected.len()
    });
    assert_eq!(log_lines(&handle, "Access.log"), expected);
    let written = std::fs::read_to_string(folder.join("Access.log")).unwrap();
    assert!(!written.contains("s3cret") && !written.contains(PASSWORD));
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::remove_dir_all(&databases).unwrap();
}

/// The event log records the disconnect and the session's end, naming
/// the client as its first accepted Login did, however it logged in since.
#[test]
fn disconnecting_a_client_ends_its_session_and_releases_its_locks() {
    let file = TempFile::new("admin-disconnect", &[], true);
    let folder = file.0.with_extension("logs");
    let (port, handle) = server::start_configured_with_handle(&file, |server| {
        server.with_event_log(&folder).unwrap()
    });
    let (mut front, _) = server::logged_in(port, "Front Desk");
    let renamed = LoginEnvironment {
        session_name: "Renamed".into(),
        ..LoginEnvironment::for_remote_file("127.0.0.1", FILE)
    };
    let login = front
        .login(ACCOUNT, PASSWORD, INTERNAL_CREATION, &renamed)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    assert_eq!(handle.clients()[0].session_name.as_deref(), Some("Renamed"));
    let (mut back, _) = server::logged_in(port, "Back Office");

    // Front Desk edits record 1, so Back Office cannot.
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        front.lock(&path, 0x31, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    assert!(!record_1_free(&mut back));

    // The administrator disconnects Front Desk (connection ids are never 0).
    let id = front.identity().connection_id;
    assert!(!handle.disconnect_client(0));
    assert!(handle.disconnect_client(id));
    if let Some(client) = handle.clients().iter().find(|client| client.id == id) {
        assert!(client.disconnecting);
    }
    assert_disconnected(&mut front);
    wait_until("Front Desk's session ended", || {
        handle.clients().iter().all(|client| client.id != id)
    });
    assert!(!handle.disconnect_client(id));

    // Back Office stays, and Front Desk's lock went with its session.
    let [client] = handle.clients().try_into().unwrap();
    assert_eq!(
        (client.machine_name.as_str(), client.disconnecting),
        ("Back Office", false)
    );
    assert_eq!(handle.files()[0].clients, 1);
    assert!(record_1_free(&mut back));
    back.disconnect().unwrap();

    let front = "openmaker (Front Desk) [127.0.0.1]";
    let opened =
        format!("Information 94 Client \"{front}\" opening database \"Tasks\" as \"Admin\".");
    let closed =
        format!("Information 98 Client \"{front}\" closing database \"Tasks\" as \"Admin\".");
    let ended = [
        format!("Warning 0 Client \"{front}\" disconnected by the administrator."),
        closed.clone(),
        format!("Information 22 Client \"{front}\" closing a connection."),
    ];
    wait_until("Access.log is written", || {
        log_lines(&handle, "Access.log").len() == 11
    });
    let lines = log_lines(&handle, "Access.log");
    assert_eq!(lines[1..4], [opened.clone(), closed, opened]);
    assert_eq!(lines[6..9], ended);
    std::fs::remove_dir_all(&folder).unwrap();
}

/// A request the client is waiting on when the administrator disconnects it
/// is answered first; the next one is not.
#[test]
fn a_disconnect_lets_the_request_in_progress_finish() {
    let file = TempFile::new("admin-busy", &[fixture::slow(0.5)], true);
    let (port, handle) = server::start_with_handle(&file, ScriptLimits::DEFAULT);
    let (mut client, _) = server::logged_in(port, "Busy");
    let id = client.identity().connection_id;

    let administrator = std::thread::spawn({
        let handle = handle.clone();
        move || {
            wait_until("the script runs", || !handle.scripts().is_empty());
            assert!(handle.disconnect_client(id));
        }
    });
    let reply = client.perform_script_on_server(3, "", true).unwrap();
    administrator.join().unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "slow done"));

    assert_disconnected(&mut client);
    wait_until("the session ended", || handle.clients().is_empty());
}

/// The administrator's messages `session` receives within five seconds,
/// once at least one has come.
fn admin_messages(session: &mut Session) -> Vec<AdminMessage> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.poll(Duration::from_millis(50)).unwrap();
        let messages = session.take_admin_messages();
        if !messages.is_empty() || Instant::now() >= deadline {
            return messages;
        }
    }
}

/// Send Message reaches a client in a `Notify` call on its own connection
/// (network-protocol.md 6.5), and the client goes on working.
#[test]
fn a_message_reaches_the_client() {
    let file = TempFile::new("admin-message", &[], true);
    let (port, handle) = server::start_with_handle(&file, ScriptLimits::DEFAULT);
    let (mut front, file_id) = server::logged_in(port, "Front Desk");
    let id = front.identity().connection_id;

    // The Admin API's rules for the text, and a client to send it to with
    // a file open.
    let idle = connect(port, "No File");
    for (client, text, error) in [
        (0, "Hello", MessageError::UnknownClient),
        (id, " \t\n", MessageError::EmptyText),
        (id, &"é".repeat(201), MessageError::TextTooLong),
        (
            idle.identity().connection_id,
            "Hello",
            MessageError::NoOpenFile,
        ),
    ] {
        assert_eq!(handle.send_message(client, text), Err(error));
    }
    idle.disconnect().unwrap();

    let message = |text: &str| AdminMessage {
        code: 0x0810,
        file: file_id as i32,
        text: text.into(),
        server_name: "openmaker-test".into(),
        countdown: 0,
    };
    let text = "The server restarts at five. 🕔";
    handle.send_message(id, text).unwrap();
    assert_eq!(admin_messages(&mut front), [message(text)]);
    // A later one in the same session, at the 200-character limit.
    let long = "é".repeat(200);
    handle.send_message(id, &long).unwrap();
    assert_eq!(admin_messages(&mut front), [message(&long)]);

    // It is not a change to the file, and the client goes on.
    assert!(front.take_notifications().is_empty());
    assert!(front.download(&[&[2]]).is_ok());
    front.disconnect().unwrap();
}

/// Block New Users: a new session connects but cannot open a file (error
/// 1638, network-protocol.md 6.5), while a client already connected goes
/// on working; turned off, the file opens.
#[test]
fn block_new_users_refuses_new_sessions_opens() {
    let file = TempFile::new("admin-block", &[], true);
    let (port, handle) = server::start_with_handle(&file, ScriptLimits::DEFAULT);
    let (mut front, _) = server::logged_in(port, "Front Desk");

    assert!(!handle.block_new_users());
    handle.set_block_new_users(true);
    assert!(handle.block_new_users());
    let mut late = connect(port, "Late");
    match late.open_file("127.0.0.1", FILE) {
        Err(SessionError::Protocol(error)) => {
            assert!(error.ends_with("server error code 0x0666"), "{error}")
        }
        other => panic!("expected error 1638, got {other:?}"),
    }
    // GetFileList (op77) is refused the same way (network-protocol.md 5.3).
    assert_eq!(late.get_file_list().unwrap().error, 1638);
    let late_id = late.identity().connection_id;
    let listed = handle.clients();
    let late_entry = listed.iter().find(|client| client.id == late_id).unwrap();
    assert!(late_entry.files.is_empty());
    assert_eq!(handle.files()[0].clients, 1);

    // The client already connected is not affected.
    assert_eq!(
        commit_record_1(&mut front, "While blocked").as_deref(),
        Some("While blocked")
    );

    handle.set_block_new_users(false);
    assert_eq!(late.open_file("127.0.0.1", FILE).unwrap().status, 0);
    assert_eq!(handle.files()[0].clients, 2);
    late.disconnect().unwrap();
    front.disconnect().unwrap();
}

/// GetFileList (op77), as FileMaker Pro's Hosts window asks it, anonymously
/// and before any Open: the published files, each entry keyed `01`, `02`,
/// ... with its name `10` and the id `01` that Open then returns for it
/// (network-protocol.md 5.3). An unpublished file is not listed.
#[test]
fn get_file_list_lists_published_files_with_their_open_ids() {
    let file = TempFile::new("admin-op77", &[], true);
    let port = server::start_configured(&file, |s| s.listing_all_files());
    let mut session = connect(port, "Hosts window");
    let reply = session.get_file_list().unwrap();
    assert_eq!((reply.error, reply.count), (0, 1));
    assert!(!reply.host_name.is_empty());
    let tree =
        om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(&reply.slots, Vec::new()))
            .unwrap();
    let entry = tree.at(&[1]).expect("entry 01");
    let name = entry
        .at(&[0x10])
        .and_then(om_hbam::Node::text)
        .unwrap()
        .unwrap();
    assert_eq!(name, FILE);
    let id = entry.at(&[1]).and_then(|node| node.value).unwrap();

    // A small maxreturn pages the list: each page fits it and carries the
    // list's count, and the pages make up the same list. The reply's own
    // fields take 45 bytes here and the name's leaf 14, so 59 cuts the
    // 26-byte list into three pages.
    const SMALL: u32 = 59;
    let mut request = om_protocol::FileListRequest::anonymous();
    for _ in 0..2 {
        let transaction = om_protocol::build_file_list(1, &request);
        let result = session.perform_with_max(&transaction, SMALL).unwrap();
        assert!(result.len() <= SMALL as usize, "{} bytes", result.len());
        let page = om_protocol::parse_file_list_reply(&result).unwrap();
        assert!(page.resume.has_more());
        assert_eq!((page.error, page.count), (0, 1));
        request.resume = page.resume;
    }
    assert_eq!(session.get_file_list_with_max(SMALL).unwrap(), reply);
    // A maxreturn too small for the reply's own fields and one whole chunk
    // of the list is refused, not answered with more than it allows.
    assert!(session.get_file_list_with_max(SMALL - 1).is_err());
    // An op77 without its parameters is malformed: a system exception, as
    // for any malformed transaction, and the connection goes on.
    let bare = om_rctransaction::Builder::new(Opcode::GetFileList as u8).finish();
    let refused = session.perform(&bare).unwrap_err().to_string();
    assert!(refused.contains("SystemException"), "{refused}");

    let opened = session.open_file("127.0.0.1", FILE).unwrap();
    assert_eq!(id, opened.file_id.to_be_bytes());
    session.disconnect().unwrap();

    let unpublished = server::start(&TempFile::new("admin-op77-none", &[], true));
    let mut session = connect(unpublished, "Hosts window");
    let reply = session.get_file_list().unwrap();
    assert_eq!((reply.error, reply.count), (0, 0));
    session.disconnect().unwrap();
}

/// The administrator's messages `session` receives until one has `code`,
/// within five seconds.
fn admin_messages_until(session: &mut Session, code: u32) -> Vec<AdminMessage> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    while !received.iter().any(|m: &AdminMessage| m.code == code) {
        assert!(
            Instant::now() < deadline,
            "no message {code:#x}: {received:?}"
        );
        session.poll(Duration::from_millis(50)).unwrap();
        received.extend(session.take_admin_messages());
    }
    received
}

/// A server whose clients disconnected with notice have 300 ms to log out
/// once their grace time ends, and a handle.
fn start_with_short_logout_wait(file: &TempFile) -> (u16, om_server::ServerHandle) {
    server::start_configured_with_handle(file, |server| {
        server.with_connection_limits(ConnectionLimits {
            logout_wait: Duration::from_millis(300),
            ..ConnectionLimits::DEFAULT
        })
    })
}

/// The request to close every file, or the forced notice (`code`), with
/// `text`, naming file `file`.
fn farewell_message(code: u32, file: u32, text: &str) -> AdminMessage {
    AdminMessage {
        code,
        file: file as i32,
        text: text.into(),
        server_name: "openmaker-test".into(),
        countdown: if code == 0x0850 { 30 } else { 0 },
    }
}

/// A client asked to disconnect closes its file, as FileMaker Pro does when
/// its countdown ends: what it held goes at once, it is no longer listed,
/// and when its grace time and time to leave are over the server ends its
/// session with a `Disconnect` call (network-protocol.md 6.5).
#[test]
fn a_client_that_closes_its_files_when_asked_leaves_quietly() {
    let file = TempFile::new("admin-farewell", &[], true);
    let (port, handle) = start_with_short_logout_wait(&file);
    let (mut front, file_id) = server::logged_in(port, "Front Desk");
    let (mut back, _) = server::logged_in(port, "Back Office");
    let id = front.identity().connection_id;
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        front.lock(&path, 0x31, false).unwrap(),
        write::LockReply::Granted { .. }
    ));

    // The Admin API's rules for the notice.
    let notice = |message: &str, grace: u64| Notice {
        message: message.into(),
        grace: Duration::from_secs(grace),
    };
    for (client, notice, error) in [
        (0, notice("", 90), DisconnectError::UnknownClient),
        (
            id,
            notice(&"é".repeat(201), 90),
            DisconnectError::TextTooLong,
        ),
        (id, notice("", 3601), DisconnectError::GraceTooLong),
    ] {
        assert_eq!(
            handle.disconnect_client_with_notice(client, &notice),
            Err(error)
        );
    }
    assert_eq!(Notice::default(), notice("", 90));

    // A grace time the client closes its file well within, however
    // loaded the machine.
    let text = "Closing for maintenance";
    let notice = Notice {
        message: text.into(),
        grace: Duration::from_secs(3),
    };
    handle.disconnect_client_with_notice(id, &notice).unwrap();
    assert_eq!(
        handle.disconnect_client_with_notice(id, &notice),
        Err(DisconnectError::Disconnecting)
    );
    let listed = |handle: &om_server::ServerHandle| {
        handle.clients().into_iter().find(|client| client.id == id)
    };
    assert!(listed(&handle).unwrap().disconnecting);
    assert_eq!(
        admin_messages_until(&mut front, 0x0850),
        [farewell_message(0x0850, file_id, text)]
    );

    // It closes its file: its lock goes, and it is no longer listed.
    front.close_file().unwrap();
    assert!(record_1_free(&mut back));
    assert!(listed(&handle).is_none());
    assert_eq!(handle.files()[0].clients, 1);

    // The server ends its session once its grace time and time to leave
    // are over.
    let deadline = Instant::now() + Duration::from_secs(5);
    let ended = loop {
        match front.poll(Duration::from_millis(50)) {
            Err(e) => break e,
            Ok(_) => assert!(Instant::now() < deadline, "the session did not end"),
        }
    };
    assert!(
        matches!(&ended, SessionError::ConnectionLost(reason) if reason == "the host ended the session"),
        "{ended:?}"
    );
    // The client sees its session end before the server has dropped its
    // entry (`end_session` removes it last; a client that closed its
    // files is no longer listed by `clients()` meanwhile), so wait for
    // the entry to go (#555). Until then asking again is refused, with no
    // effect, as a disconnect under way.
    wait_until("the session ended on the server", || {
        match handle.disconnect_client_with_notice(id, &notice) {
            Err(DisconnectError::UnknownClient) => true,
            Err(DisconnectError::Disconnecting) => false,
            other => panic!("a disconnect of the ending session: {other:?}"),
        }
    });
    assert!(!handle.disconnect_client(id));
    assert!(handle.clients().iter().all(|client| client.id != id));
    back.disconnect().unwrap();
}

/// A client that keeps its file open past its grace time is sent the
/// forced notice, and disconnected when it does not leave.
#[test]
fn a_client_that_keeps_its_files_open_is_forced_out() {
    let file = TempFile::new("admin-forced", &[], true);
    let (port, handle) = start_with_short_logout_wait(&file);
    let (mut front, file_id) = server::logged_in(port, "Front Desk");
    let (mut back, _) = server::logged_in(port, "Back Office");
    let id = front.identity().connection_id;
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        front.lock(&path, 0x31, false).unwrap(),
        write::LockReply::Granted { .. }
    ));

    let text = "Restarting now";
    let notice = Notice {
        message: text.into(),
        grace: Duration::from_millis(500),
    };
    handle.disconnect_client_with_notice(id, &notice).unwrap();
    // It stays listed, being disconnected, while it keeps its file.
    assert!(handle
        .clients()
        .iter()
        .any(|client| client.id == id && client.disconnecting));
    assert_eq!(
        admin_messages_until(&mut front, 0x0870),
        [
            farewell_message(0x0850, file_id, text),
            farewell_message(0x0870, file_id, text)
        ]
    );

    // It neither logs out nor disconnects: its connection is closed and its
    // lock released.
    wait_until("Front Desk's session ended", || {
        handle.clients().iter().all(|client| client.id != id)
    });
    assert_disconnected(&mut front);
    assert!(record_1_free(&mut back));
    back.disconnect().unwrap();
}

/// A client disconnected with notice that sends without pause, here
/// answers to calls, is still forced out when its time is up: the
/// administrator's deadlines advance with every message it sends.
#[test]
fn a_client_answering_without_pause_is_still_forced_out() {
    let file = TempFile::new("admin-forced-busy", &[], true);
    let (port, handle) = start_with_short_logout_wait(&file);
    let (mut back, _) = server::logged_in(port, "Back Office");
    let (mut front, id, _) = raw_client_locking_record_1(port);
    assert!(!record_1_free(&mut back));

    let notice = Notice {
        message: String::new(),
        grace: Duration::from_millis(200),
    };
    handle.disconnect_client_with_notice(id, &notice).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let sender = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let mut request_id = 1u32;
            while !stop.load(Ordering::Acquire) {
                let reply =
                    om_giop::build_reply(request_id, om_giop::ReplyStatus::NoException, &[], &[]);
                if front.send(&reply).is_err() {
                    break;
                }
                request_id = request_id.wrapping_add(2);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    wait_until(
        "Front Desk's session ended while it went on answering",
        || handle.clients().iter().all(|client| client.id != id),
    );
    assert!(record_1_free(&mut back));
    stop.store(true, Ordering::Release);
    sender.join().unwrap();
    back.disconnect().unwrap();
}

/// Closing one file keeps what the session holds for its others: an upload
/// in parts to one file, interrupted by closing another file on the same
/// connection, still completes and its record is stored.
#[test]
fn closing_one_file_keeps_another_files_upload() {
    let tasks = TempFile::new("admin-close-one", &[], true);
    let other = TempFile::new("admin-close-one-other", &[], true);
    let (port, _handle) =
        server::start_hosting(&[(FILE, &tasks), ("Other.fmp12", &other)], |server| server);
    // One session with both files open; it names Other, opened last.
    let (mut session, tasks_id) = server::logged_in(port, "Front Desk");
    let other_id = session
        .open_file("127.0.0.1", "Other.fmp12")
        .unwrap()
        .file_id;
    assert_eq!(
        session.fetch_internal_creation().unwrap(),
        INTERNAL_CREATION
    );
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "Other.fmp12");
    let login = session
        .login(ACCOUNT, PASSWORD, INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");

    // Record 1 of Other, uploaded in two parts.
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        session.lock(&path, 0x31, true).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let stored = OwnedNode::list(
        record.clone(),
        vec![
            OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu("Kept")),
            OwnedNode::leaf(vec![0xfc], vec![0x01, 0x01]),
        ],
    );
    let payload = write::upload_payload(vec![stored]).unwrap();
    let (first, second) = payload.split_at(payload.len() / 2);
    let part = |transaction, payload: &[u8], resume| {
        let upload = write::Upload {
            payload: payload.to_vec(),
            resume,
            terminal: Vec::new(),
            flags: write::UPLOAD_FLAGS,
            dest: vec![table.clone(), vec![5]],
        };
        write::build_upload(transaction, other_id, &upload)
    };
    let reply = session
        .perform(&part(200, first, om_protocol::Resume::start()))
        .unwrap();
    let resume = write::parse_upload_reply(&reply).unwrap();
    assert!(resume.has_more(), "the first part is held");

    // Tasks is closed in between.
    let close = write::build_unlock(201, tasks_id, &write::Unlock::close_file(tasks_id as i32));
    session.perform(&close).unwrap();

    let reply = session.perform(&part(202, second, resume)).unwrap();
    assert!(!write::parse_upload_reply(&reply).unwrap().has_more());
    let changed = OwnedNode::list(
        record,
        vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
    );
    session.commit_records(&table, vec![changed], 0x31).unwrap();
    session.kill_all_locks(0x31).unwrap();
    let records = server::records_of(&mut session, TABLE);
    assert_eq!(server::cell(&records, 1, NAME), Some("Kept"));
    session.disconnect().unwrap();
}

/// Closing a file asks each of its clients to close it, by the id the
/// client registered it with, again halfway through the grace time, and
/// forces out one that keeps it; the file is then closed, and Open refuses
/// it. Opening it loads it from disk again: a file that fails to load
/// stays closed with the reason (network-protocol.md 6.5).
#[test]
fn closing_a_file_asks_its_clients_and_opening_it_loads_it_again() {
    let file = TempFile::new("admin-close", &[], true);
    let (port, handle) = start_with_short_logout_wait(&file);
    let (mut front, file_id) = server::logged_in(port, "Front Desk");
    let (mut back, _) = server::logged_in(port, "Back Office");
    let status = |handle: &om_server::ServerHandle| handle.files()[0].status;

    let notice = |message: &str, grace: Duration| Notice {
        message: message.into(),
        grace,
    };
    for (id, notice, error) in [
        (99, Notice::default(), FileError::UnknownFile),
        (
            file_id,
            notice(&"é".repeat(201), Duration::from_secs(90)),
            FileError::TextTooLong,
        ),
        (
            file_id,
            notice("", Duration::from_secs(3601)),
            FileError::GraceTooLong,
        ),
    ] {
        assert_eq!(handle.close_file(id, &notice), Err(error));
    }
    assert_eq!(
        handle.open_file(file_id),
        Err(FileError::NotAllowed(FileStatus::Open))
    );

    // Schema changes move the store's version past what the file opened
    // again starts at, and a download reads the new catalog: the test view
    // (`Server::checking_reads`) must take the file opened again for a new
    // load, not for a later version of this one.
    for key in 1..=3u8 {
        let node = OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![key], vec![key])]);
        front.upload_to_host(&[&[0x40]], &node).unwrap();
    }
    front.download(&[&[2]]).unwrap();

    // A grace time Back Office closes the file well within, however loaded
    // the machine.
    let text = "Closing Tasks";
    handle
        .close_file(file_id, &notice(text, Duration::from_secs(2)))
        .unwrap();
    assert_eq!(status(&handle), FileStatus::Closing);
    assert_eq!(
        handle.close_file(file_id, &Notice::default()),
        Err(FileError::NotAllowed(FileStatus::Closing))
    );
    // No one may open it now.
    let mut late = connect(port, "Late");
    match late.open_file("127.0.0.1", FILE) {
        Err(SessionError::Protocol(error)) => {
            assert!(error.ends_with("server error code 0x0004"), "{error}")
        }
        other => panic!("expected error 4, got {other:?}"),
    }

    // Each client is asked to close it by the id it registered (-4).
    let ask = AdminMessage {
        code: 0x0040,
        file: -4,
        text: text.into(),
        server_name: "openmaker-test".into(),
        countdown: 30,
    };
    assert_eq!(admin_messages_until(&mut back, 0x0040)[0], ask);
    back.close_file().unwrap();
    assert_eq!(handle.files()[0].clients, 1);

    // Front Desk keeps it: asked again halfway, then forced out.
    let forced = AdminMessage {
        code: 0x0870,
        file: file_id as i32,
        countdown: 0,
        ..ask.clone()
    };
    assert_eq!(
        admin_messages_until(&mut front, 0x0870),
        [ask.clone(), ask, forced]
    );
    wait_until("the file closed", || status(&handle) == FileStatus::Closed);
    assert_disconnected(&mut front);
    assert_eq!(handle.files()[0].clients, 0);
    assert_eq!(handle.overview().open_files, 0);
    assert_eq!(
        handle.pause_file(file_id),
        Err(FileError::NotAllowed(FileStatus::Closed))
    );
    // Closed, an Open of it is answered as one of a name the server does
    // not host, as FileMaker Server answers it (spec 5.5).
    let mut after = connect(port, "After");
    match after.open_file("127.0.0.1", FILE) {
        Err(SessionError::Protocol(error)) => {
            assert!(error.ends_with("server error code 0x4fb5"), "{error}")
        }
        other => panic!("expected error 0x4fb5, got {other:?}"),
    }

    // A file that fails to load stays closed, with the reason.
    let good = std::fs::read(&file.0).unwrap();
    std::fs::write(&file.0, b"not a FileMaker file").unwrap();
    let Err(FileError::Load(reason)) = handle.open_file(file_id) else {
        panic!("a broken file opened");
    };
    let info = &handle.files()[0];
    assert_eq!(
        (info.status, info.error.as_ref()),
        (FileStatus::Closed, Some(&reason))
    );

    // Put back, it opens from disk and clients use it again.
    std::fs::write(&file.0, &good).unwrap();
    handle.open_file(file_id).unwrap();
    let info = &handle.files()[0];
    assert_eq!((info.status, info.error.as_ref()), (FileStatus::Open, None));
    assert_eq!(late.open_file("127.0.0.1", FILE).unwrap().status, 0);
    late.download(&[&[2]]).unwrap();
    late.disconnect().unwrap();
    back.disconnect().unwrap();
}

/// A server script still running on a file keeps it closing after its last
/// client has closed it: the file is closed only once the script has
/// ended, so its store can never save after the file is opened again, and
/// opening it meanwhile is refused.
#[test]
fn a_running_script_keeps_a_file_closing_until_it_ends() {
    let file = TempFile::new("admin-close-script", &[fixture::slow(1.0)], true);
    let (port, handle) = start_with_short_logout_wait(&file);
    let (mut client, file_id) = server::logged_in(port, "Scripter");
    let status = |handle: &om_server::ServerHandle| handle.files()[0].status;

    // A script that does not wait, then the client closes the file.
    let started = client.perform_script_on_server(3, "", false).unwrap();
    assert_eq!(started.error, 0);
    wait_until("the script runs", || !handle.scripts().is_empty());
    client.close_file().unwrap();
    let notice = Notice {
        message: String::new(),
        grace: Duration::from_secs(30),
    };
    handle.close_file(file_id, &notice).unwrap();

    // No client has it open, but the script holds its store.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(handle.files()[0].clients, 0);
    assert_eq!(status(&handle), FileStatus::Closing);
    assert_eq!(
        handle.open_file(file_id),
        Err(FileError::NotAllowed(FileStatus::Closing))
    );
    wait_until("the script ended and the file closed", || {
        status(&handle) == FileStatus::Closed
    });
    assert!(handle.scripts().is_empty());
    handle.open_file(file_id).unwrap();
    assert_eq!(status(&handle), FileStatus::Open);
    client.disconnect().unwrap();
}

/// A server script admitted to its file just before a close, which starts
/// only after the close first looked for the file's scripts, is still
/// aborted, so the file closes long before the script would have ended.
#[cfg(debug_assertions)]
#[test]
fn a_script_starting_as_a_close_begins_is_aborted_too() {
    let file = TempFile::new("admin-close-race", &[fixture::slow(30.0)], true);
    let mut gate = None;
    let (port, handle) = server::start_configured_with_handle(&file, |server| {
        let (server, held) = server
            .with_connection_limits(ConnectionLimits {
                logout_wait: Duration::from_millis(300),
                ..ConnectionLimits::DEFAULT
            })
            .holding_scripts();
        gate = Some(held);
        server
    });
    let gate = gate.unwrap();
    let (mut client, file_id) = server::logged_in(port, "Scripter");
    let scripter = std::thread::spawn(move || {
        let started = client.perform_script_on_server(3, "", false);
        (client, started)
    });

    // Admitted while the file is open, its script not started: the close,
    // its grace time 0, looks for the file's scripts and finds none.
    assert!(gate.wait_admitted(Duration::from_secs(5)));
    let notice = Notice {
        message: String::new(),
        grace: Duration::ZERO,
    };
    handle.close_file(file_id, &notice).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    gate.open();
    let (client, started) = scripter.join().unwrap();
    assert_eq!(started.unwrap().error, 0);

    // The 30-second script is aborted all the same, and the client forced
    // out: the file closes.
    wait_until("the file closed", || {
        handle.files()[0].status == FileStatus::Closed
    });
    assert!(handle.scripts().is_empty());
    drop(client);
}

/// Pausing a file holds every request on it, unanswered, while the
/// connection answers its Tickle and other files' clients go on; Resume
/// answers what waited. A held request goes with its client when it leaves
/// (network-protocol.md 6.1, 6.5).
#[test]
fn pausing_a_file_holds_its_requests_until_resumed() {
    let tasks = TempFile::new("admin-pause", &[], true);
    let other = TempFile::new("admin-pause-other", &[], true);
    let (port, handle) =
        server::start_hosting(&[(FILE, &tasks), ("Other.fmp12", &other)], |server| server);
    let (mut front, id, file_id) = raw_client_locking_record_1(port);
    let (mut elsewhere, _) = server::logged_in_to(port, "Elsewhere", "Other.fmp12");
    let status = |handle: &om_server::ServerHandle| {
        handle
            .files()
            .iter()
            .map(|file| file.status)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        handle.resume_file(file_id),
        Err(FileError::NotAllowed(FileStatus::Open))
    );
    handle.pause_file(file_id).unwrap();
    assert_eq!(status(&handle), [FileStatus::Paused, FileStatus::Open]);
    assert_eq!(
        handle.pause_file(file_id),
        Err(FileError::NotAllowed(FileStatus::Paused))
    );
    assert_eq!(
        handle.close_file(file_id, &Notice::default()),
        Err(FileError::NotAllowed(FileStatus::Paused))
    );

    // A request on the paused file waits; the Tickle sent after it is
    // answered at once, and another file's client goes on.
    let download = |request_id| {
        let txn = om_protocol::build_download(&om_protocol::DownloadRequest {
            transaction_id: 3,
            file_id,
            key_path: &[&[2]],
            resume: om_protocol::Resume::start(),
        });
        om_protocol::build_perform(request_id, id, b"", &txn)
    };
    let reply_id = |message: &[u8]| om_giop::Reply::parse(message).unwrap().request_id;
    front.send(&download(20)).unwrap();
    front
        .send(&om_protocol::build_tickle(22, id, b"", &[]))
        .unwrap();
    front
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(reply_id(&front.recv_logical().unwrap()), 22);
    assert!(elsewhere.download(&[&[2]]).is_ok());
    front
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    assert!(front.recv_logical().unwrap_err().is_timeout());

    // Resumed, it is answered.
    handle.resume_file(file_id).unwrap();
    front
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let answered = front.recv_logical().unwrap();
    assert_eq!(reply_id(&answered), 20);
    assert!(om_protocol::parse_perform_reply(&answered).is_ok());

    // A client that leaves while a request of its waits takes it along.
    handle.pause_file(file_id).unwrap();
    front.send(&download(24)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    drop(front);
    wait_until("Front Desk's session ended", || {
        handle.clients().iter().all(|client| client.id != id)
    });
    handle.resume_file(file_id).unwrap();
    assert_eq!(status(&handle), [FileStatus::Open, FileStatus::Open]);
    assert!(elsewhere.download(&[&[2]]).is_ok());
    elsewhere.disconnect().unwrap();
}

/// Send `message` on `transport` and read the logical reply.
fn exchange(transport: &mut Transport, message: &[u8]) -> Vec<u8> {
    transport.send(message).unwrap();
    transport.recv_logical().unwrap()
}

/// Perform transaction `txn` on connection `connection`; its result.
fn perform(transport: &mut Transport, request_id: u32, connection: u32, txn: &[u8]) -> Vec<u8> {
    let reply = exchange(
        transport,
        &om_protocol::build_perform(request_id, connection, b"", txn),
    );
    om_protocol::parse_perform_reply(&reply).unwrap().result
}

/// A GIOP 1.2 message of type `msg_type` with "fragment follows" set whose
/// body is only request id 20: a Request that starts a fragmented message,
/// or a Fragment that continues it with no data (network-protocol.md 2).
fn fragment_follows(msg_type: u8) -> Vec<u8> {
    let mut message = b"GIOP".to_vec();
    message.extend_from_slice(&[1, 2, 0x03, msg_type]);
    message.extend_from_slice(&4u32.to_le_bytes());
    message.extend_from_slice(&20u32.to_le_bytes());
    message
}

/// A client that speaks the protocol message by message, so that it can
/// send what a Session never would, logged in and holding a write lock on
/// record 1: its transport, connection id and file id.
fn raw_client_locking_record_1(port: u16) -> (Transport, u32, u32) {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut front = Transport::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
    )
    .unwrap();
    let connect =
        om_protocol::build_connect(4, "127.0.0.1", 5003, &MachineInfo::default()).unwrap();
    let identity = om_protocol::parse_connect_reply(&exchange(&mut front, &connect)).unwrap();
    let id = identity.connection_id;
    let opened = exchange(
        &mut front,
        &om_protocol::build_open(6, id, "127.0.0.1", FILE),
    );
    let open =
        om_protocol::parse_open_reply(&om_protocol::parse_perform_reply(&opened).unwrap().result)
            .unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    let login = om_protocol::build_login(&LoginRequest {
        transaction_id: 1,
        file_id: open.file_id,
        open_nonce: open.file_nonce,
        internal_creation: INTERNAL_CREATION,
        connection_key: identity.connection_key,
        account: ACCOUNT,
        password: PASSWORD,
        environment: &environment,
    });
    let login = perform(&mut front, 8, id, &login);
    assert!(om_protocol::parse_login_reply(&login).unwrap().is_success());
    let lock = write::Lock {
        waiter: 0,
        timeout: 0,
        want_mod_count: false,
        path: vec![uint_segment(TABLE), vec![5], serial::encode(1)],
        argument: 0,
        host_file: i32::try_from(open.file_id).unwrap(),
        txn: 0x31,
        lock_type: write::LOCK_WRITE,
    };
    let locked = perform(
        &mut front,
        10,
        id,
        &write::build_lock(2, open.file_id, &lock),
    );
    assert!(matches!(
        write::parse_lock_reply(&locked, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    (front, id, open.file_id)
}

/// A client cannot put off its disconnect by going on sending: one that
/// keeps a fragmented request open with empty fragments is disconnected,
/// and its lock released, while it still sends.
#[test]
fn a_disconnect_stops_a_client_that_keeps_sending() {
    let file = TempFile::new("admin-sending", &[], true);
    let (port, handle) = server::start_with_handle(&file, ScriptLimits::DEFAULT);
    let (mut back, _) = server::logged_in(port, "Back Office");
    let (mut front, id, _) = raw_client_locking_record_1(port);
    assert!(!record_1_free(&mut back));

    // It starts a fragmented request and never finishes it: an empty
    // fragment follows every millisecond until it is told to stop.
    front.send(&fragment_follows(0)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let sender = std::thread::spawn({
        let stop = stop.clone();
        move || {
            let empty = fragment_follows(7);
            while !stop.load(Ordering::Acquire) && front.send(&empty).is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    std::thread::sleep(Duration::from_millis(50));

    assert!(handle.disconnect_client(id));
    wait_until(
        "Front Desk's session ended while it went on sending",
        || handle.clients().iter().all(|client| client.id != id),
    );
    assert!(record_1_free(&mut back));
    stop.store(true, Ordering::Release);
    sender.join().unwrap();
    back.disconnect().unwrap();
}

/// The opcode whose Perform panics on the servers of the panic tests: one
/// no client sends.
#[cfg(debug_assertions)]
const PANICKING: u8 = 250;

/// How many descriptors this process holds of the server's side of
/// connections to `port` (found by their local port in /proc/net/tcp;
/// each test's server has a port of its own).
#[cfg(target_os = "linux")]
fn server_descriptors(port: u16) -> usize {
    let tcp = std::fs::read_to_string("/proc/net/tcp").unwrap();
    let inodes: std::collections::HashSet<&str> = tcp
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let local = u16::from_str_radix(fields.get(1)?.rsplit(':').next()?, 16).ok()?;
            // State 0A is LISTEN.
            (local == port && *fields.get(3)? != "0A").then_some(*fields.get(9)?)
        })
        .collect();
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter(|link| {
            link.to_str()
                .and_then(|link| link.strip_prefix("socket:[")?.strip_suffix(']'))
                .is_some_and(|inode| inodes.contains(inode))
        })
        .count()
}

/// Lock record 1 of Tasks on `session`, commit `name` into it and let it
/// go; its name as the session then reads it.
fn commit_record_1(session: &mut Session, name: &str) -> Option<String> {
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        session.lock(&path, 0x31, true).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let stored = OwnedNode::list(
        record.clone(),
        vec![
            OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu(name)),
            OwnedNode::leaf(vec![0xfc], vec![0x01, 0x01]),
        ],
    );
    let changed = OwnedNode::list(
        record,
        vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
    );
    session.upload_records(&table, vec![stored]).unwrap();
    session.commit_records(&table, vec![changed], 0x31).unwrap();
    session.kill_all_locks(0x31).unwrap();
    let records = server::records_of(session, TABLE);
    server::cell(&records, 1, NAME).map(str::to_owned)
}

/// A request handler that panics costs its connection and nothing else
/// (#281): the session's lock and its entry in the client list go before
/// the client sees its connection end, and the server keeps none of the
/// connection's descriptors.
#[cfg(debug_assertions)]
#[test]
fn a_panicking_request_ends_its_session_and_connection() {
    let file = TempFile::new("admin-panic", &[], true);
    let (port, handle) =
        server::start_configured_with_handle(&file, |server| server.panicking_on(PANICKING, false));
    let (mut back, _) = server::logged_in(port, "Back Office");
    #[cfg(target_os = "linux")]
    let descriptors = server_descriptors(port);
    let (mut front, id, file_id) = raw_client_locking_record_1(port);
    assert!(!record_1_free(&mut back));
    #[cfg(target_os = "linux")]
    assert!(server_descriptors(port) > descriptors);

    // Front Desk's next request panics on the server: it gets no reply,
    // and its stream ends.
    let mut header = om_rctransaction::Header::new(PANICKING);
    header.file_id = file_id;
    let txn = om_rctransaction::Builder::with_header(header).finish();
    front
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    front
        .send(&om_protocol::build_perform(12, id, b"", &txn))
        .unwrap();
    match front.recv_logical() {
        Err(TransportError::Read(ReadError::Closed)) => {}
        Err(TransportError::Read(ReadError::Io(e)) | TransportError::Io(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        other => panic!("expected the end of the stream, got {other:?}"),
    }

    // The session ended before the socket closed.
    assert!(handle.clients().iter().all(|client| client.id != id));
    assert!(!handle.disconnect_client(id));
    assert_eq!(
        commit_record_1(&mut back, "After the panic").as_deref(),
        Some("After the panic")
    );
    #[cfg(target_os = "linux")]
    wait_until("the server closed the connection's descriptors", || {
        server_descriptors(port) == descriptors
    });
    back.disconnect().unwrap();
}

/// A panic while holding every shared lock a session's end takes (the
/// lock table, registrations, mailboxes, script callbacks, connection ids)
/// leaves the server serving: the panicking session still ends, and other
/// clients connect, register, commit, are notified and leave.
#[cfg(debug_assertions)]
#[test]
fn a_panic_holding_the_shared_locks_leaves_the_server_serving() {
    let file = TempFile::new("admin-poisoned", &[], true);
    let (port, handle) =
        server::start_configured_with_handle(&file, |server| server.panicking_on(PANICKING, true));
    let (mut back, _) = server::logged_in(port, "Back Office");
    let (mut front, _) = server::logged_in(port, "Front Desk");
    let id = front.identity().connection_id;
    let txn = om_rctransaction::Builder::new(PANICKING).finish();
    let lost = front.perform(&txn).unwrap_err();
    assert!(closed(&lost), "{lost:?}");
    wait_until("Front Desk's session ended", || {
        handle.clients().iter().all(|client| client.id != id)
    });

    let (mut desk, _) = server::logged_in(port, "New Desk");
    assert_eq!(
        commit_record_1(&mut desk, "After the panic").as_deref(),
        Some("After the panic")
    );
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let record_1 = server::path_key(&[&table, &[5], &record]);
    assert!(server::notifications_of(&mut back)
        .iter()
        .any(|notification| server::notified(notification).1 == record_1));
    desk.disconnect().unwrap();
    back.disconnect().unwrap();
    wait_until("every client left", || handle.clients().is_empty());
}

/// A server whose first file cannot be opened hosts the others and raises
/// an alert, and its event log, read back as the Log Viewer reads it, holds
/// the server's start and stop and each file's opening and closing in
/// FileMaker Server's form (`docs/re/server-logs.md`).
#[test]
fn the_event_log_records_the_server_and_its_files() {
    let file = TempFile::new("admin-eventlog", &[], true);
    let folder = file.0.with_extension("logs");
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![
            HostedFile {
                name: "Missing.fmp12".into(),
                path: file.0.with_extension("missing"),
            },
            HostedFile {
                name: FILE.into(),
                path: file.0.clone(),
            },
        ],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .with_event_log(&folder)
        .unwrap();
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    let serving = std::thread::spawn(move || server.serve());
    // A client's session is served: the server has started.
    let (mut desk, file_id) = server::logged_in(port, "Front Desk");
    let (office, _) = server::logged_in(port, "Back Office");

    // The missing file is left out, its id unused, and is an alert.
    let hosted: Vec<(u32, String)> = handle.files().into_iter().map(|f| (f.id, f.name)).collect();
    assert_eq!(hosted, [(2, FILE.to_owned())]);
    let [alert] = handle.alerts().try_into().unwrap();
    let not_found = "Database \"Missing\" could not be opened; file was not found.";
    assert_eq!(
        (alert.level, alert.message.as_str()),
        (Level::Error, not_found)
    );

    // The log stays open after the stop, for the connections still served:
    // a logout after it is recorded, a client that then drops its own
    // connection leaves as any does, and the server closing the last one
    // says so. Then the log is closed.
    assert!(handle.shutdown(Duration::from_secs(5)));
    serving.join().unwrap().unwrap();
    let mut header = om_rctransaction::Header::new(Opcode::Logout as u8);
    header.file_id = file_id;
    let logout = om_rctransaction::Builder::with_header(header).finish();
    assert_eq!(desk.perform(&logout).unwrap(), [0, 0]);
    drop(office);
    wait_until("Back Office's session ended", || {
        handle.clients().len() == 1
    });
    assert_eq!(handle.close_connections(), 1);
    wait_until("Front Desk's session ended", || handle.clients().is_empty());
    assert!(handle.close_event_log(Duration::from_secs(5)));
    let desk = "openmaker (Front Desk) [127.0.0.1]";
    let office = "openmaker (Back Office) [127.0.0.1]";
    let closing = |client: &str| {
        format!("Information 98 Client \"{client}\" closing database \"Tasks\" as \"Admin\".")
    };
    assert_eq!(
        log_lines(&handle, "Access.log")[4..],
        [
            closing(desk),
            closing(office),
            format!("Information 22 Client \"{office}\" closing a connection."),
            format!(
                "Information 0 Client \"{desk}\" disconnected: the server closed its connection."
            ),
        ]
    );
    assert_eq!(handle.log_folder().as_deref(), Some(folder.as_path()));
    let names: Vec<String> = handle.log_files().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["Event.log", "Access.log"]);
    let page = handle.read_log("Event.log", None, 100).unwrap();
    assert_eq!(
        log_lines(&handle, "Event.log"),
        [
            "Information 418 Opening database \"Missing\"...".to_owned(),
            format!("Error 250 {not_found}"),
            "Information 418 Opening database \"Tasks\"...".to_owned(),
            "Information 184 Opened database \"Tasks\".".to_owned(),
            "Information 486 Database Server started.".to_owned(),
            "Information 488 Stopping Database Server...".to_owned(),
            "Information 168 Database \"Tasks\" closed.".to_owned(),
            "Information 490 Database Server stopped.".to_owned(),
        ]
    );
    assert!(page
        .entries
        .iter()
        .all(|entry| entry.time.is_some() && !entry.host.is_empty()));
    // Pages of two lines, newest first, follow each other to the start.
    let first = handle.read_log("Event.log", None, 2).unwrap();
    let second = handle.read_log("Event.log", first.next, 2).unwrap();
    assert_eq!(first.entries, page.entries[..2]);
    assert_eq!(second.entries, page.entries[2..4]);
    // Only the log's own files are read, by name.
    assert!(matches!(
        handle.read_log("../Event.log", None, 1),
        Err(LogReadError::UnknownFile)
    ));

    assert!(handle.acknowledge_alert(alert.id));
    assert!(handle.alerts().is_empty());
    assert!(!handle.acknowledge_alert(alert.id));
    std::fs::remove_dir_all(&folder).unwrap();
}

/// A file anyone could open with full access is not hosted, as FileMaker
/// Server does not host it by default (docs/re/local-files.md 3.2): an
/// active account in [Full Access] (or in a set that manages accounts) that
/// is [Guest], opens with the empty password, or is the one File Options
/// log in as with the password they store. Each is an alert with FileMaker
/// Server's message 1017 (Error), and is left as it was. With
/// `insecure_files` they are hosted, and the empty password opens the file.
#[test]
fn files_anyone_could_open_with_full_access_are_not_hosted() {
    use fixture::{put_account, ACTIVE_STATUS, INACTIVE_STATUS};
    use om_scripts::text_node;

    type Change = fn(&mut OwnedNode);
    let files: [(&str, bool, Change); 7] = [
        ("Tasks", true, |_| {}),
        ("Open", false, |root| {
            put_account(root, 2, "", Some(&ACTIVE_STATUS[..]), 1)
        }),
        ("Remembered", false, |root| {
            // The account's name in another case, its password stored.
            root.put(&[&[23], &[1], &[1], &[16]], text_node(&[], "admin"));
            root.put(&[&[23], &[1], &[1], &[6]], text_node(&[], PASSWORD));
        }),
        ("Guest", false, |root| {
            root.put(
                &[&[23], &[1], &[5], &[1], &[0x0b]],
                OwnedNode::leaf(Vec::new(), vec![1, 1]),
            )
        }),
        ("Manager", false, |root| {
            // Clerk's set may manage accounts, and Clerk needs no password.
            put_account(root, 3, "", None, 2);
            root.put(
                &[&[23], &[2], &[5], &[2], &[0x0a]],
                OwnedNode::leaf(Vec::new(), vec![0x63, 0x14]),
            );
        }),
        // An inactive account, and a stored password that is not the
        // account's: nobody logs in with either.
        ("Retired", true, |root| {
            put_account(root, 2, "", Some(&INACTIVE_STATUS[..]), 1)
        }),
        ("Forgotten", true, |root| {
            root.put(&[&[23], &[1], &[1], &[16]], text_node(&[], ACCOUNT));
            root.put(&[&[23], &[1], &[1], &[6]], text_node(&[], "not it"));
        }),
    ];
    let copies: Vec<TempFile> = files
        .iter()
        .map(|(name, _, change)| {
            let mut root = fixture::tree(&[], true);
            change(&mut root);
            TempFile::from_tree(&format!("insecure-{name}"), &root)
        })
        .collect();
    let start = |insecure_files: bool| {
        let config = ServerConfig {
            identity: ServerIdentity {
                name: "openmaker-test".into(),
                version_string: "OpenMaker Server test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: files
                .iter()
                .zip(&copies)
                .map(|((name, _, _), copy)| HostedFile {
                    name: format!("{name}.fmp12"),
                    path: copy.0.clone(),
                })
                .collect(),
            insecure_files,
            record_dir: None,
        };
        let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
        let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
        let port = server.local_addr().unwrap().port();
        let handle = server.handle().unwrap();
        std::thread::spawn(move || server.serve());
        (port, handle)
    };
    let before: Vec<Vec<u8>> = copies
        .iter()
        .map(|c| std::fs::read(&c.0).unwrap())
        .collect();

    let (_, handle) = start(false);
    let hosted: Vec<String> = handle.files().into_iter().map(|f| f.name).collect();
    let secure: Vec<String> = files
        .iter()
        .filter(|(_, secure, _)| *secure)
        .map(|(name, _, _)| format!("{name}.fmp12"))
        .collect();
    assert_eq!(hosted, secure);
    let alerts: Vec<(Level, String)> = handle
        .alerts()
        .into_iter()
        .map(|alert| (alert.level, alert.message))
        .collect();
    // Newest first.
    let refused: Vec<(Level, String)> = ["Manager", "Guest", "Remembered", "Open"]
        .iter()
        .map(|name| {
            let message = format!(
                "SECURITY: Database \"{name}\" cannot be opened because it is insecure. \
                 Require Password-Protected Databases is enabled."
            );
            (Level::Error, message)
        })
        .collect();
    assert_eq!(alerts, refused);
    assert!(handle.shutdown(Duration::from_secs(5)));
    handle.close_connections();
    // A refused file is left as it was.
    for (copy, bytes) in copies.iter().zip(&before) {
        assert_eq!(&std::fs::read(&copy.0).unwrap(), bytes);
    }

    let (port, handle) = start(true);
    assert_eq!(handle.files().len(), files.len());
    let mut session = connect(port, "Front Desk");
    session.open_file("127.0.0.1", "Open.fmp12").unwrap();
    assert_eq!(
        session.fetch_internal_creation().unwrap(),
        INTERNAL_CREATION
    );
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", "Open.fmp12");
    let login = session
        .login(ACCOUNT, "", INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    session.disconnect().unwrap();
    assert!(handle.shutdown(Duration::from_secs(5)));
}
