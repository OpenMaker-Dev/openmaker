//! End to end: Perform Script on Server (script-runtime.md 11) from
//! OpenMaker's own client (`om-session`) against an `om-server` running in
//! this process, over real TLS on loopback, on a file built for the test
//! (`fixtures/tasks.rs`): the script's result and error, the records it
//! wrote, the change notifications other sessions get, the callback
//! result, transactions, the record rules and privileges server scripts
//! apply, and the limits that end runaway scripts.

#[path = "fixtures/tasks.rs"]
mod fixture;

use std::time::{Duration, Instant};

use fixture::{
    TempFile, ACCOUNT, BODY, CLERK, CLERK_PASSWORD, CODE, COUNT, CREATOR, DOUBLED, FILE,
    INTERNAL_CREATION, NAME, NOTES, PASSWORD, TABLE,
};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, KeySegment, KeyTree, SlotReader};
use om_protocol::write::{self, attention};
use om_protocol::{LoginEnvironment, MachineInfo, Opcode, RequestNotification};
use om_scripts::ScriptStep;
use om_server::{
    HostedFile, Identity, ScriptLimits, Server, ServerConfig, ServerHandle, ServerIdentity,
};
use om_session::Session;

/// Start a server hosting `file` on an ephemeral loopback port.
fn start(file: &TempFile, script_limit: Option<usize>) -> u16 {
    let limits = ScriptLimits {
        concurrent: script_limit.unwrap_or(ScriptLimits::DEFAULT.concurrent),
        ..ScriptLimits::DEFAULT
    };
    start_with(file, limits).0
}

/// Start a server with script limits, and its administration handle.
fn start_with(file: &TempFile, limits: ScriptLimits) -> (u16, ServerHandle) {
    start_configured(file, limits, |server| server)
}

/// [`start_with`], the server configured by `configure` first.
fn start_configured(
    file: &TempFile,
    limits: ScriptLimits,
    configure: impl FnOnce(Server) -> Server,
) -> (u16, ServerHandle) {
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    // Admin has [Full Access]; Clerk is in [Data Entry Only].
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .with_script_limits(limits);
    let server = configure(server);
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    (port, handle)
}

fn connect(port: u16, name: &str) -> Session {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    Session::connect(
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
    .unwrap()
}

/// A session logged in to the file and registered for its notifications.
fn logged_in(port: u16, name: &str) -> Session {
    logged_in_as(port, name, ACCOUNT)
}

fn logged_in_as(port: u16, name: &str, account: &str) -> Session {
    let mut session = connect(port, name);
    let open = session.open_file("127.0.0.1", FILE).unwrap();
    assert_eq!(
        session.fetch_internal_creation().unwrap(),
        INTERNAL_CREATION
    );
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    let password = match account {
        CLERK => CLERK_PASSWORD,
        fixture::READER => fixture::READER_PASSWORD,
        CUSTOM => CUSTOM_PASSWORD,
        _ => PASSWORD,
    };
    let login = session
        .login(account, password, INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    assert_eq!(session.protocol_version(), Some(9));
    let register = om_protocol::build_request_notification(
        100,
        &RequestNotification {
            attention_mask: u32::MAX,
            message_address: 1,
            path: Vec::new(),
            file_id: open.file_id,
            client_file_id: -4,
        },
    );
    assert_eq!(session.perform(&register).unwrap(), [0, 0]);
    session
}

/// The records of Tasks as the client downloads them: id -> field -> text.
fn records(session: &mut Session) -> Vec<(u64, Vec<(u64, String)>)> {
    records_of(session, TABLE)
}

fn records_of(session: &mut Session, table: u64) -> Vec<(u64, Vec<(u64, String)>)> {
    let table = uint_segment(table);
    let path: [&[u8]; 2] = [&table, &[5]];
    let reply = session.download(&path).unwrap();
    let base = path.iter().map(|s| KeySegment(s)).collect();
    let tree = KeyTree::from_slots(SlotReader::with_path(&reply.slots, base)).unwrap();
    tree.node(&path)
        .unwrap()
        .children
        .iter()
        .map(|record| {
            let cells = record
                .children
                .iter()
                .filter_map(|cell| {
                    let text = om_hbam::Cell::from_node(cell).text()?.ok()?;
                    Some((cell.id()?, text))
                })
                .collect();
            (serial::decode(record.segment.0).unwrap(), cells)
        })
        .collect()
}

fn cell(records: &[(u64, Vec<(u64, String)>)], record: u64, field: u64) -> Option<&str> {
    let (_, cells) = records.iter().find(|(id, _)| *id == record)?;
    cells
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, text)| text.as_str())
}

/// Notifications another session gets: with the reply to its next request.
fn notifications_of(session: &mut Session) -> Vec<write::Notification> {
    let probe = om_rctransaction::Builder::new(Opcode::GetGuestCount as u8).finish();
    let reply = session.perform_reply(&probe).unwrap();
    write::parse_notification_backchannel(&reply.backchannel).unwrap_or_default()
}

/// The record path a notification names (`[T, 05, R]`, write-path.md 6.3).
fn notified_path(notification: &write::Notification) -> Vec<u8> {
    // Item 2 sits before the last length word; its first byte is the
    // path's total length.
    let message = &notification.message;
    let length = u16::from_be_bytes([message[message.len() - 2], message[message.len() - 1]]);
    message[message.len() - 2 - usize::from(length) + 1..message.len() - 2].to_vec()
}

fn record_key(id: u64) -> Vec<u8> {
    let table = uint_segment(TABLE);
    let record = serial::encode(id);
    let mut out = vec![table.len() as u8];
    out.extend(&table);
    out.extend([1, 5, record.len() as u8]);
    out.extend(record);
    out
}

#[test]
fn performs_scripts_on_the_server() {
    let file = TempFile::new(
        "e2e",
        &[
            fixture::server_work(),
            fixture::failing(),
            fixture::slow(1.5),
            // What FileMaker Server's script engine answers for an add-on,
            // a sensor, the media player and Furigana without its
            // dictionary (s45, calc-functions.md).
            fixture::script(
                4,
                "Device functions",
                vec![fixture::exit_with(
                    "GetAddonInfo ( \"x\" ) & \"|\" & EvaluationError ( GetAddonInfo ( \"\" ) ) & \
                     \"|\" & EvaluationError ( GetSensor ( \"battery\" ) ) & \"|\" & \
                     GetAVPlayerAttribute ( \"playbackState\" ) & \"|\" & \
                     EvaluationError ( GetAVPlayerAttribute ( \"playbackState\" ) ) & \"|\" & \
                     Furigana ( \"abc\" )",
                )],
            ),
            fixture::script(
                5,
                "No add-on name",
                vec![fixture::exit_with("GetAddonInfo ( \"\" )")],
            ),
            fixture::script(
                6,
                "Battery",
                vec![fixture::exit_with("GetSensor ( \"battery\" )")],
            ),
        ],
        true,
    );
    let port = start(&file, None);
    let mut client = logged_in(port, "requester");
    let mut other = logged_in(port, "other");
    let device = client.perform_script_on_server(4, "", true).unwrap();
    assert_eq!(
        (device.error, device.result.as_str()),
        (0, "{\r\t\"APIVers\" : 2\r}|3|1208||0|abc")
    );
    // The two errors' results are `?`, not empty, as s45 shows them.
    for script in [5, 6] {
        let bare = client.perform_script_on_server(script, "", true).unwrap();
        assert_eq!(bare.result, "?", "script {script}: {bare:?}");
    }

    // Wait for completion: the reply carries the Exit Script result and the
    // script's final Get ( LastError ).
    let reply = client.perform_script_on_server(1, "e2e", true).unwrap();
    assert_eq!(reply.error, 0, "{reply:?}");
    let parts: Vec<&str> = reply.result.split('|').collect();
    assert_eq!(parts[..4], ["3", "3", "From e2e", "Admin"], "{reply:?}");
    assert!(parts[4].starts_with("Server "), "{reply:?}");

    // The file afterwards: record 1 changed, record 3 created.
    let after = records(&mut client);
    assert_eq!(
        after.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(cell(&after, 1, fixture::COUNT), Some("13"));
    assert_eq!(cell(&after, 1, fixture::NAME), Some("Write tests"));
    assert_eq!(cell(&after, 3, fixture::NAME), Some("From e2e"));
    assert_eq!(cell(&after, 3, fixture::DUE), Some("10/28/2026"));
    // ... and on disk.
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    assert_eq!(saved.records(TABLE).unwrap().len(), 3);

    // Both sessions are told, the requester with the reply itself (11.1
    // step 5), the other with its next request, as for a client's commit.
    let expected = [
        (attention::MODIFY | attention::RECORD, record_key(1)),
        (attention::ADD | attention::RECORD, record_key(3)),
    ];
    for notifications in [client.take_notifications(), notifications_of(&mut other)] {
        let got: Vec<(u32, Vec<u8>)> = notifications
            .iter()
            .map(|n| (n.code, notified_path(n)))
            .collect();
        assert_eq!(got, expected);
    }

    // The script's last error comes back with its result.
    let failed = client.perform_script_on_server(2, "", true).unwrap();
    assert_eq!((failed.error, failed.result.as_str()), (201, "failed"));
    // A script that does not exist: 104.
    assert_eq!(
        client.perform_script_on_server(9, "", true).unwrap().error,
        104
    );

    // Not waiting: answered at once, without a result.
    let started = Instant::now();
    let quick = client.perform_script_on_server(3, "", false).unwrap();
    assert_eq!((quick.error, quick.result.as_str()), (0, ""));
    // (The script pauses 1.5 s.)
    assert!(started.elapsed() < Duration::from_secs(1));

    // With a callback: the result arrives as op105 on the back-channel.
    assert_eq!(client.poll(Duration::ZERO).unwrap(), 0);
    let started = client
        .perform_script_on_server_with_callback(3, "cb", 42)
        .unwrap();
    assert_eq!(started.error, 0);
    assert_eq!(client.poll(Duration::from_secs(10)).unwrap(), 1);
    let results = client.take_psos_results();
    assert_eq!(results.len(), 1);
    assert_eq!(
        (
            results[0].callback,
            results[0].error,
            results[0].result.as_str()
        ),
        (42, 0, "slow done")
    );
    // The connection goes on working after the server's call.
    assert_eq!(records(&mut client).len(), 3);
    client.disconnect().unwrap();
    other.disconnect().unwrap();
}

#[test]
fn refuses_scripts_beyond_the_limit_and_files_not_logged_in() {
    let file = TempFile::new("limit", &[fixture::slow(1.0), fixture::server_work()], true);
    let port = start(&file, Some(1));
    let mut client = logged_in(port, "busy");
    let mut second = logged_in(port, "second");
    // One script executing fills the engine: 812 for the next.
    assert_eq!(
        client.perform_script_on_server(3, "", false).unwrap().error,
        0
    );
    assert_eq!(
        second.perform_script_on_server(1, "", true).unwrap().error,
        812
    );
    // Once it is done, scripts run again.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = second.perform_script_on_server(1, "", true).unwrap();
        if reply.error == 0 {
            break;
        }
        assert_eq!(reply.error, 812);
        assert!(Instant::now() < deadline, "the slow script never ended");
        std::thread::sleep(Duration::from_millis(100));
    }

    // A file opened but not logged in to has no file user: 100.
    let mut stranger = connect(port, "stranger");
    stranger.open_file("127.0.0.1", FILE).unwrap();
    assert_eq!(
        stranger
            .perform_script_on_server(1, "", true)
            .unwrap()
            .error,
        100
    );
    for session in [client, second, stranger] {
        session.disconnect().unwrap();
    }
}

/// A script that does not wait is refused with 812 when no thread can be
/// started for it, as when the system has no threads left; the connection
/// goes on, and the script's slot is free again (#328).
#[test]
#[cfg(target_os = "linux")]
fn a_script_without_a_thread_is_refused_and_frees_its_slot() {
    let done = fixture::script(1, "Done", vec![fixture::exit_with("\"done\"")]);
    let file = TempFile::new("no-thread", &[done], true);
    let limits = ScriptLimits {
        concurrent: 1,
        ..ScriptLimits::DEFAULT
    };
    // No thread starts with a stack larger than the address space.
    let (port, handle) = start_configured(&file, limits, |server| {
        server.with_script_stack_size(1 << 50)
    });
    let mut client = logged_in(port, "no thread");
    for _ in 0..2 {
        let reply = client.perform_script_on_server(1, "", false).unwrap();
        assert_eq!(reply.error, 812);
    }
    // With a limit of one, a script that waits (it runs on the
    // connection's own thread) runs only if the slot is free.
    assert!(handle.scripts().is_empty());
    assert_eq!(run(&mut client, 1), (0, "done".to_string()));
    client.disconnect().unwrap();
}

/// Wait for completion: the error and the Exit Script result.
fn run(session: &mut Session, script: u64) -> (u16, String) {
    let reply = session.perform_script_on_server(script, "", true).unwrap();
    (reply.error, reply.result)
}

#[test]
fn transactions_keep_every_edit_of_a_record_and_commit_atomically() {
    use fixture::{exit_with, go_to_layout, go_to_record, on, script, set_field, set_field_in};
    let tasks = [
        // Record 1 changed, left, changed again: both edits commit.
        script(
            4,
            "Revisit",
            vec![
                on(86, true),
                ScriptStep::new(205),
                go_to_record(1),
                set_field(NAME, "\"Changed\""),
                go_to_record(4),
                go_to_record(1),
                set_field(COUNT, "99"),
                ScriptStep::new(206),
                exit_with(
                    "Get ( LastError ) & \"|\" & Tasks::Name & \"|\" & Tasks::Count & \"|\" & \
                     Tasks::Doubled",
                ),
            ],
        ),
        // Finds, found sets and deletions see the transaction's changes.
        script(
            5,
            "Overlay",
            vec![
                on(86, true),
                ScriptStep::new(205),
                go_to_record(1),
                set_field(NAME, "\"Zebra\""),
                ScriptStep::new(22),
                set_field(NAME, "\"Zebra\""),
                ScriptStep::new(28),
                fixture::set_variable("$found", "Get ( FoundCount ) & \"/\" & Get ( RecordID )"),
                ScriptStep::new(23),
                go_to_record(2),
                ScriptStep::new(9),
                ScriptStep::new(23),
                fixture::set_variable("$all", "Get ( FoundCount )"),
                ScriptStep::new(206),
                exit_with("Get ( LastError ) & \"|\" & $found & \"|\" & $all"),
            ],
        ),
        // A record of the second table fails validation at Commit
        // Transaction: nothing of the transaction is written, in either
        // table.
        script(
            6,
            "Atomic",
            vec![
                on(86, true),
                ScriptStep::new(205),
                go_to_record(1),
                set_field(NAME, "\"Atomic\""),
                go_to_layout(fixture::NOTES_LAYOUT),
                go_to_record(1),
                set_field_in(fixture::NOTES_OCCURRENCE, BODY, "\"far too long for it\""),
                ScriptStep::new(206),
                exit_with("Get ( LastError )"),
            ],
        ),
    ];
    let file = TempFile::new("txn", &tasks, true);
    let port = start(&file, None);
    let mut client = logged_in(port, "txn");

    assert_eq!(run(&mut client, 4), (0, "0|Changed|99|198".into()));
    let after = records(&mut client);
    assert_eq!(cell(&after, 1, NAME), Some("Changed"));
    assert_eq!(cell(&after, 1, COUNT), Some("99"));
    // The stored calculation follows the committed Count.
    assert_eq!(cell(&after, 1, DOUBLED), Some("198"));

    assert_eq!(run(&mut client, 5), (0, "0|1/1|1".into()));
    let after = records(&mut client);
    assert_eq!(after.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1]);
    assert_eq!(cell(&after, 1, NAME), Some("Zebra"));

    // 507: the validation calculation failed; the first table's change is
    // not committed either.
    assert_eq!(run(&mut client, 6), (507, "507".into()));
    assert_eq!(cell(&records(&mut client), 1, NAME), Some("Zebra"));
    assert_eq!(cell(&records_of(&mut client, NOTES), 1, BODY), Some("Hi"));
    client.disconnect().unwrap();
}

/// Commit Transaction computes each record's auto-entered formulas with
/// the layout of the step that last changed it, not the committing step's
/// (#187): a Tasks record made on Tasks' layout and a Notes record made on
/// Notes' layout each read their own layout's number, though the
/// transaction commits on Notes' layout.
#[test]
fn a_transaction_commits_each_record_with_the_layout_that_changed_it() {
    use fixture::{exit_with, go_to_layout, on, script, set_field, set_field_in};
    const SEEN: u64 = 20;
    let scripts = [script(
        4,
        "Layouts",
        vec![
            on(86, true),
            ScriptStep::new(205),
            ScriptStep::new(7),
            set_field(NAME, "\"Here\""),
            go_to_layout(fixture::NOTES_LAYOUT),
            ScriptStep::new(7),
            set_field_in(fixture::NOTES_OCCURRENCE, BODY, "\"There\""),
            ScriptStep::new(206),
            exit_with("Get ( LastError ) & \"|\" & Get ( LayoutNumber )"),
        ],
    )];
    let mut tree = fixture::tree(&scripts, true);
    let mut names = fixture::names();
    for (table, occurrence) in [
        (TABLE, fixture::OCCURRENCE),
        (NOTES, fixture::NOTES_OCCURRENCE),
    ] {
        names.add_field(table, SEEN, "Seen");
        fixture::add_field(&mut tree, table, SEEN, "Seen", 1, 1, 0, 8, 0);
        let seen = "Get ( LayoutNumber )";
        fixture::add_formula(&mut tree, &names, (table, occurrence), SEEN, 5, seen);
    }
    let file = TempFile::from_tree("txn-layouts", &tree);
    let port = start(&file, None);
    let mut client = logged_in(port, "txn-layouts");
    assert_eq!(run(&mut client, 4), (0, "0|2".into()));
    let made = |records: &[(u64, Vec<(u64, String)>)], field: u64, text: &str| {
        let (id, _) = records
            .iter()
            .find(|(id, _)| cell(records, *id, field) == Some(text))
            .unwrap_or_else(|| panic!("the record holding {text}"));
        cell(records, *id, SEEN).map(str::to_owned)
    };
    assert_eq!(
        made(&records(&mut client), NAME, "Here").as_deref(),
        Some("1")
    );
    assert_eq!(
        made(&records_of(&mut client, NOTES), BODY, "There").as_deref(),
        Some("2")
    );
    client.disconnect().unwrap();
}

#[test]
fn server_scripts_apply_the_files_record_rules() {
    use fixture::{exit_with, go_to_record, on, script, set_field, set_variable};
    let scripts = [
        script(
            4,
            "Rules",
            vec![
                on(86, true),
                // A stored calculation is recomputed when Count changes.
                go_to_record(1),
                set_field(COUNT, "21"),
                ScriptStep::new(75),
                // A new record gets its auto-entered creator.
                ScriptStep::new(7),
                set_field(NAME, "\"Ruled\""),
                set_field(COUNT, "1"),
                ScriptStep::new(75),
                // Validation by calculation (Always): the commit fails with
                // 507 and the record stays as it was.
                set_field(CODE, "\"TOOLONG\""),
                ScriptStep::new(75),
                set_variable("$invalid", "Get ( LastError )"),
                ScriptStep::new(51),
                set_field(CODE, "\"OK\""),
                ScriptStep::new(75),
                exit_with(
                    "$invalid & \"|\" & Get ( LastError ) & \"|\" & Tasks::Creator & \"|\" & \
                     Tasks::Doubled",
                ),
            ],
        ),
        // "Prohibit modification" stops data entry, not Set Field (the
        // Data API's prohibitMode "script").
        script(
            5,
            "Creator",
            vec![
                go_to_record(1),
                set_field(CREATOR, "\"Someone\""),
                ScriptStep::new(75),
                exit_with("Get ( LastError )"),
            ],
        ),
        // Duplicate Record copies the entries; the creation value and the
        // calculation are computed for the copy.
        script(
            6,
            "Duplicate",
            vec![
                go_to_record(1),
                ScriptStep::new(8),
                ScriptStep::new(75),
                exit_with("Get ( LastError ) & \"|\" & Get ( RecordID )"),
            ],
        ),
    ];
    let file = TempFile::new("rules", &scripts, true);
    let port = start(&file, None);
    let mut client = logged_in(port, "rules");
    assert_eq!(run(&mut client, 4), (0, "507|0|Admin|2".into()));
    let after = records(&mut client);
    assert_eq!(cell(&after, 1, DOUBLED), Some("42"));
    assert_eq!(cell(&after, 3, NAME), Some("Ruled"));
    assert_eq!(cell(&after, 3, CREATOR), Some("Admin"));
    assert_eq!(cell(&after, 3, DOUBLED), Some("2"));
    assert_eq!(cell(&after, 3, CODE), Some("OK"));
    // An existing record keeps its creator (a creation value).
    assert_eq!(cell(&after, 1, CREATOR), None);
    assert_eq!(run(&mut client, 5), (0, "0".into()));
    assert_eq!(cell(&records(&mut client), 1, CREATOR), Some("Someone"));
    assert_eq!(run(&mut client, 6), (0, "0|4".into()));
    let after = records(&mut client);
    assert_eq!(cell(&after, 4, COUNT), Some("21"));
    assert_eq!(cell(&after, 4, DOUBLED), Some("42"));
    assert_eq!(cell(&after, 4, CREATOR), Some("Admin"));
    client.disconnect().unwrap();

    // A rule the server cannot enforce (an auto-entered serial number)
    // refuses the write with error 3 instead of committing it unchecked.
    let mut tree = fixture::tree(
        &[script(
            4,
            "Serial",
            vec![
                on(86, true),
                go_to_record(1),
                set_field(NAME, "\"Unchecked\""),
                set_variable("$refused", "Get ( LastError )"),
                ScriptStep::new(7),
                set_variable("$refused", "$refused & \"|\" & Get ( LastError )"),
                ScriptStep::new(75),
                exit_with("$refused"),
            ],
        )],
        true,
    );
    fixture::set_field_options(&mut tree, TABLE, COUNT, 0x2);
    let file = TempFile::from_tree("serial", &tree);
    let port = start(&file, None);
    let mut client = logged_in(port, "serial");
    // Set Field and New Record are both refused.
    assert_eq!(run(&mut client, 4), (0, "3|3".into()));
    assert_eq!(records(&mut client).len(), 2);
    assert_eq!(cell(&records(&mut client), 1, NAME), Some("Write tests"));
    client.disconnect().unwrap();
}

#[test]
fn scripts_run_with_the_requesting_accounts_privileges() {
    use fixture::{exit_with, full_access, go_to_record, on, perform_script, script, set_field};
    let scripts = [
        fixture::server_work(),
        // Run with full access privileges: its steps may write for any
        // account; the sub-script it calls runs with the account's own.
        full_access(
            4,
            "Elevated",
            vec![
                on(86, true),
                go_to_record(1),
                set_field(NAME, "\"By clerk\""),
                ScriptStep::new(75),
                perform_script(5),
                exit_with("Get ( ScriptResult )"),
            ],
        ),
        script(
            5,
            "Sub",
            vec![
                on(86, true),
                set_field(COUNT, "1000"),
                fixture::set_variable("$write", "Get ( LastError )"),
                fixture::set_variable("$read", "Tasks::Name"),
                exit_with("$write & \"|\" & Get ( LastError ) & \"|\" & $read"),
            ],
        ),
        // Truncate Table (182) needs [Full Access] whatever the account may
        // delete (Claris's Truncate Table help); elevation counts.
        script(
            6,
            "Truncate",
            vec![
                on(86, true),
                ScriptStep::new(182),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        full_access(
            7,
            "Truncate elevated",
            vec![
                on(86, true),
                ScriptStep::new(182),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_reader(&mut root);
    let file = TempFile::from_tree("privileges", &root);
    let port = start(&file, None);

    // [Read-Only Access]: an ordinary script runs and reads every record;
    // its writes are refused with 200, as the account's own writes are,
    // and nothing changes. Under a script set to run with full access, its
    // steps write; the sub-script it calls is the account's own again.
    let mut reader = logged_in_as(port, "reader", fixture::READER);
    assert_eq!(run(&mut reader, 5), (0, "200|0|Write tests".into()));
    // New Record is refused too: the found set and record stay.
    let (error, result) = run(&mut reader, 1);
    assert_eq!(error, 0, "errors are captured");
    assert!(result.starts_with("2|1|Write tests|Reader|"), "{result}");
    assert_eq!(records(&mut reader).len(), 2);
    assert_eq!(run(&mut reader, 4), (0, "200|0|By clerk".into()));
    let after = records(&mut reader);
    assert_eq!(cell(&after, 1, NAME), Some("By clerk"));
    assert_eq!(cell(&after, 1, COUNT), Some("3"));
    reader.disconnect().unwrap();

    // [Data Entry Only]: an ordinary script runs under the account's set,
    // which creates, edits and deletes records (session-identity.md 2.7).
    let mut clerk = logged_in_as(port, "clerk", CLERK);
    assert_eq!(run(&mut clerk, 5), (0, "0|0|By clerk".into()));
    assert_eq!(cell(&records(&mut clerk), 1, COUNT), Some("1000"));
    let (error, result) = run(&mut clerk, 1);
    assert_eq!(error, 0);
    assert!(result.starts_with("3|3|From |Clerk|"), "{result}");
    assert_eq!(records(&mut clerk).len(), 3);
    assert_eq!(run(&mut clerk, 6).1, "9|3");
    let mut reader = logged_in_as(port, "reader", fixture::READER);
    assert_eq!(run(&mut reader, 6).1, "9|3");
    reader.disconnect().unwrap();
    assert_eq!(run(&mut clerk, 7).1, "0|0");
    clerk.disconnect().unwrap();
}

/// An account in a custom set, which [`add_custom_set`] adds.
const CUSTOM: &str = "Temp";
const CUSTOM_PASSWORD: &str = "temp's secret";

/// Add [`CUSTOM`] (security catalog record 7) in set 4, a custom set
/// OpenMaker enforces: records view only, every layout and value list,
/// every script executable only but scripts 4 and 200, which it gives no
/// access (session-identity.md 2.2: `09, 11 04` is script `04`'s entry,
/// `09, 11 80 48` script 200's, its key the serial encoding; `d5` no
/// access), every other privilege, no password lifetime.
fn add_custom_set(root: &mut om_hbam::OwnedNode) {
    use om_hbam::OwnedNode;
    let set: &[u8] = &[4];
    root.put(
        &[&[23], &[2], &[5], set, &[16]],
        om_scripts::text_node(&[], "Clerks"),
    );
    for (member, value) in [(2u8, [0x10u8, 0x03]), (0x0a, [0x9f, 0xff]), (0x0c, [0, 0])] {
        root.put(
            &[&[23], &[2], &[5], set, &[member]],
            OwnedNode::leaf(Vec::new(), value.to_vec()),
        );
    }
    for (entry, quad) in [
        (&[0x04u8][..], 0xdfu8),
        (&[0x11], 0xd6),
        (&[0x11, 0x04], 0xd5),
        // Script 200, keyed `80 48` (two bytes, its serial encoding).
        (&[0x11, 0x80, 0x48], 0xd5),
        (&[0x21], 0xd6),
    ] {
        root.put(
            &[&[23], &[2], &[5], set, &[9], entry],
            OwnedNode::leaf(Vec::new(), vec![quad]),
        );
    }
    root.put(
        &[&[23], &[1], &[5], &[7], &[16]],
        om_scripts::text_node(&[], CUSTOM),
    );
    fixture::put_account(root, 7, CUSTOM_PASSWORD, None, 4);
}

/// A custom set decides which scripts its account may run
/// (session-identity.md 2.2): script 4, set to run with full access
/// privileges, has no access in Temp's set, so op30 answers 9 and nothing
/// changes, as `CallScript` refuses a script the account has no access to
/// (script-runtime.md 3.7, 9.1). Before, being set to run with full access
/// let it run, and its steps wrote with full access (#227). Script 5 is
/// executable: it runs, and its record steps fail closed (9), since a
/// custom set's record limits are not applied to server scripts yet.
#[test]
fn a_custom_set_decides_which_scripts_its_account_may_run() {
    use fixture::{exit_with, full_access, go_to_record, on, script, set_field};
    let scripts = [
        full_access(
            4,
            "Elevated",
            vec![
                on(86, true),
                go_to_record(1),
                set_field(NAME, "\"By temp\""),
                ScriptStep::new(75),
                exit_with("\"ran\""),
            ],
        ),
        script(
            5,
            "Own",
            vec![
                on(86, true),
                set_field(COUNT, "1000"),
                exit_with("Get ( LastError ) & \"|\" & Get ( AccountName )"),
            ],
        ),
    ];
    let mut scripts = scripts.to_vec();
    scripts.push(full_access(200, "Elevated too", vec![exit_with("\"ran\"")]));
    let mut root = fixture::tree(&scripts, true);
    add_custom_set(&mut root);
    let file = TempFile::from_tree("custom-scripts", &root);
    let (port, _handle) = start_configured(&file, ScriptLimits::DEFAULT, |server| {
        server.admitting_custom_sets()
    });
    let mut temp = logged_in_as(port, "temp", CUSTOM);
    assert_eq!(run(&mut temp, 4), (9, String::new()));
    // A two-byte key: its entry is found by the key, not the id's bytes.
    assert_eq!(run(&mut temp, 200), (9, String::new()));
    // The script's last error is Set Field's.
    assert_eq!(run(&mut temp, 5), (9, "9|Temp".into()));
    let after = records(&mut temp);
    assert_eq!(cell(&after, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&after, 1, COUNT), Some("3"));
    temp.disconnect().unwrap();
}

#[test]
fn a_sub_script_cannot_change_the_record_its_elevated_caller_opened() {
    use fixture::{exit_with, full_access, go_to_record, on, perform_script, script, set_field};
    let scripts = [
        // Run with full access privileges: it opens record 1 by changing
        // it, and calls a sub-script while the record is still open.
        full_access(
            4,
            "Elevated",
            vec![
                on(86, true),
                go_to_record(1),
                set_field(NAME, "\"By clerk\""),
                perform_script(5),
                ScriptStep::new(75),
                exit_with("Get ( ScriptResult )"),
            ],
        ),
        // The sub-script runs with the account's own privileges: the open
        // record is no reason to let it write.
        script(
            5,
            "Sub",
            vec![
                on(86, true),
                set_field(COUNT, "1000"),
                fixture::set_variable("$write", "Get ( LastError )"),
                ScriptStep::new(51),
                fixture::set_variable("$revert", "Get ( LastError )"),
                exit_with("$write & \"|\" & $revert"),
            ],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_reader(&mut root);
    let file = TempFile::from_tree("open-privileges", &root);
    let port = start(&file, None);
    // [Read-Only Access]: Set Field is 200; Revert Record changes nothing,
    // so it may go on.
    let mut reader = logged_in_as(port, "reader", fixture::READER);
    assert_eq!(run(&mut reader, 4), (0, "200|0".into()));
    let after = records(&mut reader);
    assert_eq!(cell(&after, 1, COUNT), Some("3"));
    // Revert Record in the sub-script dropped the caller's change too.
    assert_eq!(cell(&after, 1, NAME), Some("Write tests"));
    reader.disconnect().unwrap();
}

/// Set Variable [name; ExecuteSQL ( sql ; "|" ; "," )].
fn query(name: &str, sql: &str) -> ScriptStep {
    fixture::set_variable(name, &format!("ExecuteSQL ( {sql:?} ; \"|\" ; \",\" )"))
}

#[test]
fn execute_sql_reads_the_file_as_the_session_sees_it() {
    use fixture::{exit_with, go_to_record, on, script, set_field, set_variable};
    let scripts =
        [script(
            4,
            "Query",
            vec![
            on(86, true),
            // Open Transaction; record 1 renamed (it waits in the
            // transaction), record 3 created and still open.
            ScriptStep::new(205),
            go_to_record(1),
            set_field(NAME, "\"Renamed\""),
            ScriptStep::new(7),
            set_field(NAME, "\"Pending\""),
            set_field(COUNT, "7"),
            query(
                "$rows",
                "SELECT ROWID, Name, \"Count\" FROM Tasks ORDER BY ROWID",
            ),
            // Name has a value index: these are finds on the file's
            // indexes, with the transaction's records and the open record.
            query("$pending", "SELECT ROWID FROM Tasks WHERE Name = 'Pending'"),
            query("$renamed", "SELECT ROWID FROM Tasks WHERE Name = 'Renamed'"),
            query("$old", "SELECT COUNT(*) FROM Tasks WHERE Name = 'Write tests'"),
            query(
                "$fields",
                "SELECT COUNT(*) FROM FileMaker_Fields WHERE TableName = 'Tasks'",
            ),
            set_variable(
                "$error",
                "ExecuteSQLe ( \"SELECT Nope FROM Tasks\" ; \"\" ; \"\" )",
            ),
            // Revert Transaction, then the block's end.
            ScriptStep::new(207),
            ScriptStep::new(206),
            exit_with(
                "$rows & \"/\" & $pending & \"/\" & $renamed & \"/\" & $old & \"/\" & $fields & \
                 \"/\" & Left ( $error ; 16 )",
            ),
        ],
        )];
    let mut tree = fixture::tree(&scripts, true);
    fixture::set_field_options(&mut tree, TABLE, NAME, 0x8000_0000);
    let file = TempFile::from_tree("sql", &tree);
    let port = start(&file, None);
    let mut client = logged_in(port, "sql");
    assert_eq!(
        run(&mut client, 4).1,
        "1|Renamed|3,2|Ship|5,3|Pending|7/3/1/0/7/? ERROR: FQL0007"
    );
    // The script reverted its transaction: nothing changed.
    let after = records(&mut client);
    assert_eq!(after.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(cell(&after, 1, NAME), Some("Write tests"));
    client.disconnect().unwrap();
}

/// ExecuteSQL reads as the session may: every account of a verified
/// built-in set reads every record, also in the sub-script an elevated
/// script calls. (An account the server fails closed reads nothing, 9:
/// `scripts::tests::unknown_privileges_fail_closed_unless_elevated`.)
#[test]
fn execute_sql_reads_with_the_requesting_accounts_privileges() {
    use fixture::{exit_with, full_access, perform_script, script, set_variable};
    let scripts = [
        // Run with full access privileges: its queries read for any
        // account; the sub-script's run with the account's own.
        full_access(
            4,
            "Elevated",
            vec![
                query("$count", "SELECT COUNT(*) FROM Tasks"),
                perform_script(5),
                exit_with("$count & \"|\" & Get ( ScriptResult )"),
            ],
        ),
        script(
            5,
            "Own",
            vec![
                set_variable(
                    "$text",
                    "ExecuteSQLe ( \"SELECT Name FROM Tasks\" ; \"\" ; \"\" )",
                ),
                exit_with(
                    "EvaluationError ( ExecuteSQL ( \"SELECT TableName FROM FileMaker_Tables\" ; \
                     \"\" ; \"\" ) ) & \"|\" & $text",
                ),
            ],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_reader(&mut root);
    let file = TempFile::from_tree("sql-privileges", &root);
    let port = start(&file, None);
    for (name, account) in [("clerk", CLERK), ("reader", fixture::READER)] {
        let mut session = logged_in_as(port, name, account);
        assert_eq!(run(&mut session, 4).1, "2|0|Write tests\rShip", "{account}");
        session.disconnect().unwrap();
    }
    let mut admin = logged_in(port, "admin");
    assert_eq!(run(&mut admin, 5).1, "0|Write tests\rShip");
    admin.disconnect().unwrap();
}

/// A file whose script 4 runs a three-way join over 300 tasks that would
/// take minutes (27 million rows to test, none matching).
fn slow_query_file(tag: &str) -> TempFile {
    use fixture::{exit_with, script};
    let scripts = [
        fixture::server_work(),
        script(
            4,
            "Slow Query",
            vec![
                query(
                    "$r",
                    "SELECT COUNT(*) FROM Tasks a JOIN Tasks b ON a.ROWID > 0 \
                     JOIN Tasks c ON a.\"Count\" + b.\"Count\" + c.\"Count\" < 0",
                ),
                exit_with("$r"),
            ],
        ),
    ];
    let mut tree = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut tree, 3..=300);
    TempFile::from_tree(tag, &tree)
}

#[test]
fn a_script_calculation_stays_within_the_servers_text_limit() {
    use fixture::{exit_with, script, set_variable};
    // 32,768 characters, and that text substituted for each of its own:
    // 64 KB within the limit, 1 GiB past the server's 256 MiB (FileMaker's
    // 2 GB would allow it), refused before it is built.
    let text = "While ( [ t = \"a\" ; i = 0 ] ; i < 15 ; [ t = t & t ; i = i + 1 ] ; t )";
    let big = script(
        4,
        "Big Text",
        vec![
            set_variable("$p", text),
            set_variable("$small", "Length ( Substitute ( $p ; \"a\" ; \"bb\" ) )"),
            set_variable("$big", "Length ( Substitute ( $p ; \"a\" ; $p ) )"),
            exit_with("$small & \"|\" & $big"),
        ],
    );
    let file = TempFile::new("calc-text", &[fixture::server_work(), big], true);
    let port = start(&file, None);
    let mut client = logged_in(port, "calc-text");
    assert_eq!(run(&mut client, 4), (0, "65536|?".into()));
    client.disconnect().unwrap();
}

#[test]
fn execute_sql_stops_when_its_script_runs_out_of_time() {
    let file = slow_query_file("sql-timeout");
    let (port, handle) = start_with(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_millis(500),
        },
    );
    let mut client = logged_in(port, "sql-timeout");
    let started = Instant::now();
    // The query ends with the script's time limit: the script is aborted
    // (1) and its slot is free at once.
    assert_eq!(run(&mut client, 4), (1, String::new()));
    assert!(started.elapsed() < Duration::from_secs(10), "{started:?}");
    assert!(handle.scripts().is_empty());
    client.disconnect().unwrap();
}

#[test]
fn a_calculation_stops_when_its_script_runs_out_of_time() {
    use fixture::{exit_with, go_to_record, on, script, set_field, set_variable};
    // 2.5 billion iterations in one Set Variable: about an hour of work in
    // one step, on record 1 locked in a transaction.
    let stuck = script(
        4,
        "Stuck Calculation",
        vec![
            on(86, true),
            ScriptStep::new(205),
            go_to_record(1),
            set_field(NAME, "\"Stuck\""),
            set_variable(
                "$r",
                "While ( [ i = 0 ] ; i < 50000 ; [ i = i + 1 ; j = While ( [ k = 0 ] ; \
                 k < 50000 ; [ k = k + 1 ] ; k ) ] ; i )",
            ),
            set_field(NAME, "$r"),
            ScriptStep::new(206),
            exit_with("$r"),
        ],
    );
    let file = TempFile::new("calc-timeout", &[fixture::server_work(), stuck], true);
    let (port, handle) = start_with(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_millis(500),
        },
    );
    let mut client = logged_in(port, "calc-timeout");
    let mut other = logged_in(port, "other");
    let started = Instant::now();
    // The time limit stops the calculation inside its step and aborts the
    // script (1): its transaction is reverted, its lock released and its
    // slot freed.
    assert_eq!(run(&mut client, 4), (1, String::new()));
    assert!(started.elapsed() < Duration::from_secs(10), "{started:?}");
    assert!(handle.scripts().is_empty());
    assert_eq!(cell(&records(&mut other), 1, NAME), Some("Write tests"));
    assert!(record_1_free(&mut other));
    client.disconnect().unwrap();
    other.disconnect().unwrap();
}

#[test]
fn shutdown_stops_a_script_inside_execute_sql() {
    let file = slow_query_file("sql-shutdown");
    let (port, handle) = start_with(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_secs(600),
        },
    );
    let mut client = logged_in(port, "sql-shutdown");
    assert_eq!(
        client.perform_script_on_server(4, "", false).unwrap().error,
        0
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.scripts().iter().all(|s| s.script != "Slow Query") {
        assert!(Instant::now() < deadline, "{:?}", handle.scripts());
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let it get into the join.
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    assert!(handle.shutdown(Duration::from_secs(10)));
    assert!(started.elapsed() < Duration::from_secs(5), "{started:?}");
    assert!(handle.scripts().is_empty());
    client.disconnect().unwrap();
}

/// Whether another session can lock record 1 now.
fn record_1_free(session: &mut Session) -> bool {
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    let granted = matches!(
        session.lock(&path, 77, false).unwrap(),
        write::LockReply::Granted { .. }
    );
    session.kill_all_locks(77).unwrap();
    granted
}

/// Wait until no script executes.
fn wait_idle(handle: &ServerHandle, within: Duration) {
    let deadline = Instant::now() + within;
    while !handle.scripts().is_empty() {
        assert!(Instant::now() < deadline, "{:?}", handle.scripts());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn runaway_scripts_end_at_their_limits_and_revert() {
    let scripts = [fixture::server_work(), fixture::runaway(4)];
    let file = TempFile::new("runaway", &scripts, true);
    let (port, handle) = start_with(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: 5_000,
            duration: Duration::from_secs(60),
        },
    );
    let mut client = logged_in(port, "runaway");
    let mut other = logged_in(port, "other");

    // The step limit ends a waiting script: its transaction is reverted
    // and its lock released. The loop's 201 stays its last error (an
    // abort's 1 never hides an earlier error).
    let started = Instant::now();
    assert_eq!(run(&mut client, 4), (201, String::new()));
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(cell(&records(&mut other), 1, NAME), Some("Write tests"));
    assert!(record_1_free(&mut other));

    // Scripts that do not wait end too, and free their slots; the callback
    // reports the error.
    for callback in [1, 2] {
        let started = client
            .perform_script_on_server_with_callback(4, "", callback)
            .unwrap();
        assert_eq!(started.error, 0);
    }
    let mut results = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while results.len() < 2 {
        assert!(Instant::now() < deadline, "no callback results");
        client.poll(Duration::from_millis(200)).unwrap();
        results.extend(client.take_psos_results());
    }
    assert!(results.iter().all(|r| r.error == 201), "{results:?}");
    wait_idle(&handle, Duration::from_secs(5));
    assert_eq!(run(&mut client, 1).0, 0);
    client.disconnect().unwrap();
    other.disconnect().unwrap();

    // The time limit ends a script too, also one that pauses.
    let file = TempFile::new("timeout", &[fixture::slow(60.0), fixture::runaway(4)], true);
    let (port, handle) = start_with(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_millis(300),
        },
    );
    let mut client = logged_in(port, "timeout");
    let started = Instant::now();
    assert_eq!(run(&mut client, 4).0, 201);
    assert_eq!(run(&mut client, 3).0, 1);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(handle.scripts().is_empty());
    assert_eq!(cell(&records(&mut client), 1, NAME), Some("Write tests"));
    client.disconnect().unwrap();
}

/// Each abort is recorded in the event log.
#[test]
fn administrators_abort_scripts_and_shut_down() {
    let scripts = [
        fixture::server_work(),
        fixture::slow(60.0),
        fixture::runaway(4),
    ];
    let file = TempFile::new("abort", &scripts, true);
    let folder = file.0.with_extension("logs");
    let (port, handle) = start_configured(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_secs(600),
        },
        |server| server.with_event_log(&folder).unwrap(),
    );
    let mut client = logged_in(port, "abort");
    let mut other = logged_in(port, "other");
    let running = |count| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let scripts = handle.scripts();
            // Named once their sessions have started.
            if scripts.len() == count && scripts.iter().all(|s| !s.script.starts_with('#')) {
                return scripts;
            }
            assert!(Instant::now() < deadline, "{scripts:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    // Two scripts that never end on their own fill the engine.
    assert_eq!(
        client.perform_script_on_server(4, "", false).unwrap().error,
        0
    );
    assert_eq!(
        client.perform_script_on_server(3, "", false).unwrap().error,
        0
    );
    let scripts = running(2);
    assert_eq!(
        scripts
            .iter()
            .map(|s| s.script.as_str())
            .collect::<Vec<_>>(),
        ["Runaway", "Slow"]
    );
    assert_eq!(scripts[0].account, ACCOUNT);
    assert_eq!(run(&mut other, 1).0, 812);
    // The runaway holds record 1 in its transaction once its Set Field has
    // run (it is listed as soon as its session starts).
    let deadline = Instant::now() + Duration::from_secs(10);
    while record_1_free(&mut other) {
        assert!(
            Instant::now() < deadline,
            "the runaway never locked record 1"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The administrator aborts the runaway: its transaction is reverted
    // and its lock released.
    assert!(handle.abort_script(scripts[0].id));
    running(1);
    assert!(record_1_free(&mut other));
    assert_eq!(cell(&records(&mut other), 1, NAME), Some("Write tests"));
    // Closing the file aborts the paused one.
    assert_eq!(handle.abort_file_scripts(FILE), 1);
    running(0);
    assert_eq!(run(&mut other, 1).0, 0);

    // Shutdown aborts what runs, waits for it, and refuses new scripts.
    assert_eq!(
        client.perform_script_on_server(4, "", false).unwrap().error,
        0
    );
    running(1);
    assert!(handle.shutdown(Duration::from_secs(10)));
    assert!(handle.scripts().is_empty());
    assert!(record_1_free(&mut other));
    assert_eq!(run(&mut other, 1).0, 820);
    let after = records(&mut other);
    assert_eq!(cell(&after, 1, NAME), Some("Write tests"));
    // (Server Work ran once in between.)
    assert_eq!(cell(&after, 1, COUNT), Some("13"));
    client.disconnect().unwrap();
    other.disconnect().unwrap();

    let script = |name: &str| {
        format!(
            "Warning 0 Perform Script on Server script \"{name}\" in database \"Tasks\" for \
             account \"Admin\" aborted; "
        )
    };
    let expected = [
        format!("{}an administrator aborted it", script("Runaway")),
        format!("{}its database was closed", script("Slow")),
        format!("{}the server is stopping", script("Runaway")),
    ];
    let aborts = || -> Vec<String> {
        let page = handle.read_log("Event.log", None, 100).unwrap();
        page.entries
            .iter()
            .rev()
            .filter(|entry| entry.message.starts_with("Perform Script on Server"))
            .map(|entry| {
                // Without the script's last error, which depends on the step
                // the abort stopped.
                let message = entry.message.split(" (error").next().unwrap();
                format!("{} {} {message}", entry.level, entry.number)
            })
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while aborts().len() < expected.len() {
        assert!(Instant::now() < deadline, "{:?}", aborts());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(aborts(), expected);
    std::fs::remove_dir_all(&folder).unwrap();
}

/// Change Password (83) in a server script sets the password of the
/// account the session logged in with, as FileMaker's `DoChangePassword`
/// and `SetPassword` do (local-files.md 2.4): with its dialog the step is
/// refused (3); a wrong old password is 212 and changes nothing; the right
/// one sets the new password, cut to 100 UTF-16 code units, which the old
/// one then no longer matches. The saved account carries the checksum its
/// members give, and the account catalog's index a raised modification
/// count, so clients holding a copy see it is stale (#83).
#[test]
fn change_password_sets_the_accounts_own_password() {
    use fixture::{calc, exit_with, on, script, set_variable};
    let change = |old: &str, new: &str, dialog: bool| {
        let mut step = ScriptStep::new(83);
        step.set_calculation(0, Some(&calc(old)));
        step.set_calculation(1, Some(&calc(new)));
        // "With dialog" is on unless the step stores kNoInteract (0x80).
        step.set_flag(0x80, !dialog);
        step
    };
    let old = format!("{CLERK_PASSWORD:?}");
    let long = "x".repeat(101);
    let scripts = [script(
        6,
        "Change",
        vec![
            on(86, true),
            change(&old, "\"fresh secret\"", true),
            set_variable("$dialog", "Get ( LastError )"),
            change("\"wrong\"", "\"fresh secret\"", false),
            set_variable("$wrong", "Get ( LastError )"),
            change(&old, &format!("{long:?}"), false),
            set_variable("$right", "Get ( LastError )"),
            change(&old, "\"another\"", false),
            exit_with("$dialog & \"|\" & $wrong & \"|\" & $right & \"|\" & Get ( LastError )"),
        ],
    )];
    // The account catalog's index, which every FileMaker file has
    // (local-files.md 3); the server refuses to change accounts without it.
    let mut root = fixture::tree(&scripts, true);
    root.put(
        &[&[23], &[1], &[1], &[4]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![2]),
    );
    let file = TempFile::from_tree("change-password", &root);
    let index_count = |file: &TempFile| {
        let saved = om_hbam::HbamFile::open(&file.0).unwrap();
        let count = saved
            .subtree(&[&[23], &[1], &[1], &[0xfc]])
            .unwrap()
            .and_then(|n| n.value)
            .map(<[u8]>::to_vec);
        count
    };
    let before = index_count(&file);
    let port = start(&file, None);
    let mut clerk = logged_in_as(port, "clerk", CLERK);
    assert_eq!(run(&mut clerk, 6).1, "3|212|0|212");
    clerk.disconnect().unwrap();
    // A new login takes the new password, its first 100 characters, and
    // not the old one.
    let login = |password: &str| {
        let mut session = connect(port, "check");
        session.open_file("127.0.0.1", FILE).unwrap();
        session.fetch_internal_creation().unwrap();
        let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
        let reply = session
            .login(CLERK, password, INTERNAL_CREATION, &environment)
            .unwrap();
        let _ = session.disconnect();
        reply.is_success()
    };
    assert!(login(&long[..100]));
    assert!(!login(CLERK_PASSWORD));

    // On disk: the checksum of the account as changed, and the catalog's
    // count raised.
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    let at = |path: &[&[u8]]| saved.subtree(path).unwrap().and_then(|n| n.value);
    let account = saved.subtree(&[&[23], &[1], &[5], &[3]]).unwrap().unwrap();
    let member = |key: u8| account.child(&[key]).and_then(|n| n.value);
    let name = account.child(&[16]).and_then(|n| n.text()?.ok()).unwrap();
    let expected = om_server::accounts::checksum(&om_server::accounts::ChecksumInput {
        version: at(&[&[2], &[5]]).map(|v| u16::from_be_bytes(v.try_into().unwrap())),
        access_uuid: at(&[&[23], &[4], &[1], &[0x10]]),
        creation: at(&[&[2], &[254]]),
        key: &[3],
        kind: member(2).and_then(|v| v.first().copied()),
        name: &name,
        password: member(6).unwrap(),
        description: None,
        status: member(0x0a),
        set: member(0x0b),
    });
    assert_eq!(member(4), Some(&expected[..]));
    assert_ne!(index_count(&file), before);
}

/// Reset Account Password (136) and Enable Account (137) in a server
/// script, as FileMaker's `DoResetAccount` and `DoEnableAccount` write them
/// (local-files.md 2.4): a full-access session resets Clerk's password,
/// makes the account inactive and then active again, each saved with the
/// checksum FileMaker Pro checks. A missing account is 213 (inferred), an
/// account whose stored checksum is not the one its members give is not
/// rewritten (3), a [Full Access] account is not made inactive (3),
/// Expire password is refused (3), and a session without full access gets
/// 9 (#83).
#[test]
fn account_steps_reset_passwords_and_enable_accounts() {
    use fixture::{calc, exit_with, on, script, set_variable};
    let named = |id: u16, name: &str| {
        let mut step = ScriptStep::new(id);
        step.set_calculation(0, Some(&calc(name)));
        step
    };
    let reset = |name: &str, password: &str| {
        let mut step = named(136, name);
        step.set_calculation(1, Some(&calc(password)));
        step
    };
    // Deactivate is the step's flag 0x100.
    let enable = |name: &str, active: bool| {
        let mut step = named(137, name);
        step.set_flag(0x100, !active);
        step
    };
    let mut expired = reset("\"Clerk\"", "\"later\"");
    expired.set_flag(0x100, true);
    let scripts = [
        script(
            6,
            "Reset and disable",
            vec![
                on(86, true),
                reset("\"clerk\"", "\"new secret\""),
                set_variable("$reset", "Get ( LastError )"),
                enable("\"Clerk\"", false),
                set_variable("$off", "Get ( LastError )"),
                reset("\"Nobody\"", "\"x\""),
                set_variable("$missing", "Get ( LastError )"),
                enable(&format!("{:?}", fixture::READER), false),
                set_variable("$unread", "Get ( LastError )"),
                // [Full Access] is never made inactive, and Expire password
                // is refused until a hosted login can set a new password.
                enable("\"Admin\"", false),
                set_variable("$admin", "Get ( LastError )"),
                expired,
                exit_with(
                    "$reset & \"|\" & $off & \"|\" & $missing & \"|\" & $unread & \"|\" & \
                     $admin & \"|\" & Get ( LastError )",
                ),
            ],
        ),
        script(
            7,
            "Enable",
            vec![
                on(86, true),
                enable("\"Clerk\"", true),
                exit_with("Get ( LastError )"),
            ],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    // The account catalog's index, which every FileMaker file has.
    root.put(
        &[&[23], &[1], &[1], &[4]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![2]),
    );
    fixture::add_reader(&mut root);
    root.put(
        &[&[23], &[1], &[5], &[6], &[4]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0; 16]),
    );
    let file = TempFile::from_tree("account-steps", &root);
    let port = start(&file, None);
    // Clerk ([Data Entry Only]) may not manage accounts.
    let mut clerk = logged_in_as(port, "clerk", CLERK);
    assert_eq!(run(&mut clerk, 7).1, "9");
    clerk.disconnect().unwrap();

    let login = |account: &str, password: &str| {
        let mut session = connect(port, "check");
        session.open_file("127.0.0.1", FILE).unwrap();
        session.fetch_internal_creation().unwrap();
        let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
        let reply = session
            .login(account, password, INTERNAL_CREATION, &environment)
            .unwrap();
        let _ = session.disconnect();
        reply.is_success()
    };
    let mut admin = logged_in(port, "admin");
    assert_eq!(run(&mut admin, 6).1, "0|0|213|3|3|3");
    assert!(!login(CLERK, "new secret"), "Clerk is inactive");
    assert!(login(fixture::READER, fixture::READER_PASSWORD));
    assert_eq!(run(&mut admin, 7).1, "0");
    assert!(login(CLERK, "new secret"));
    assert!(!login(CLERK, CLERK_PASSWORD));

    // The saved account carries the checksum its members give.
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    let at = |path: &[&[u8]]| saved.subtree(path).unwrap().and_then(|n| n.value);
    let account = saved.subtree(&[&[23], &[1], &[5], &[3]]).unwrap().unwrap();
    let member = |key: u8| account.child(&[key]).and_then(|n| n.value);
    let name = account.child(&[16]).and_then(|n| n.text()?.ok()).unwrap();
    let expected = om_server::accounts::checksum(&om_server::accounts::ChecksumInput {
        version: at(&[&[2], &[5]]).map(|v| u16::from_be_bytes(v.try_into().unwrap())),
        access_uuid: at(&[&[23], &[4], &[1], &[0x10]]),
        creation: at(&[&[2], &[254]]),
        key: &[3],
        kind: member(2).and_then(|v| v.first().copied()),
        name: &name,
        password: member(6).unwrap(),
        description: None,
        status: member(0x0a),
        set: member(0x0b),
    });
    assert_eq!(member(4), Some(&expected[..]));
}

/// Delete Account (135) in a server script, as FileMaker's
/// `DoDeleteAccount` removes an account (local-files.md 2.4 and 3): the
/// member goes, and the catalog's index loses its name list entry, its
/// place in the display order (numbered again from 1), its `07` entry and
/// one from the count, while the last key issued stays. A [Full Access]
/// account is not deleted (3) and a missing one is 213 (inferred) (#83).
/// Admin, second in the display order, becomes first.
#[test]
fn delete_account_removes_the_account_and_its_index_entries() {
    use fixture::{calc, exit_with, on, script, set_variable};
    let delete = |name: &str| {
        let mut step = ScriptStep::new(135);
        step.set_calculation(0, Some(&calc(name)));
        step
    };
    let scripts = [script(
        6,
        "Delete",
        vec![
            on(86, true),
            delete("\"Admin\""),
            set_variable("$admin", "Get ( LastError )"),
            delete("\"Nobody\""),
            set_variable("$missing", "Get ( LastError )"),
            delete("\"clerk\""),
            exit_with("$admin & \"|\" & $missing & \"|\" & Get ( LastError )"),
        ],
    )];
    // The account catalog's index as FileMaker keeps it (local-files.md
    // 3), for Admin (key 2) and Clerk (key 3), Clerk first in the display
    // order so that Admin moves up to its place.
    let mut root = fixture::tree(&scripts, true);
    let leaf = |value: Vec<u8>| om_hbam::OwnedNode::leaf(Vec::new(), value);
    let index: [&[u8]; 3] = [&[23], &[1], &[1]];
    fn at<'a>(rest: &[&'a [u8]]) -> Vec<&'a [u8]> {
        let mut path: Vec<&'a [u8]> = vec![&[23], &[1], &[1]];
        path.extend_from_slice(rest);
        path
    }
    root.put(&at(&[&[0]]), leaf(vec![1, 3]));
    for (position, key, name) in [(2u32, 2u8, ACCOUNT), (1, 3, CLERK)] {
        let listed = om_data::field_catalog::listed_key(name).unwrap();
        root.put(&at(&[&[1], &listed]), leaf(vec![1, key]));
        root.put(&at(&[&[3], &position.to_be_bytes()]), leaf(vec![1, key]));
        root.put(&at(&[&[7], &[key]]), leaf(Vec::new()));
    }
    root.put(&at(&[&[4]]), leaf(2u32.to_be_bytes().to_vec()));
    let file = TempFile::from_tree("delete-account", &root);
    let port = start(&file, None);
    let mut admin = logged_in(port, "admin");
    assert_eq!(run(&mut admin, 6).1, "3|213|0");
    let mut session = connect(port, "check");
    session.open_file("127.0.0.1", FILE).unwrap();
    session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    let reply = session
        .login(CLERK, CLERK_PASSWORD, INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(!reply.is_success(), "Clerk is gone");
    let _ = session.disconnect();

    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    let node = |path: &[&[u8]]| saved.subtree(path).unwrap();
    assert!(node(&[&[23], &[1], &[5], &[3]]).is_none());
    assert!(node(&[&[23], &[1], &[5], &[2]]).is_some());
    let index = node(&index).unwrap();
    let entries = |key: u8| -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
        index
            .child(&[key])
            .map(|n| {
                n.children
                    .iter()
                    .map(|c| (c.segment.0.to_vec(), c.value.map(<[u8]>::to_vec)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let admin_key = om_data::field_catalog::listed_key(ACCOUNT).unwrap();
    assert_eq!(entries(1), [(admin_key, Some(vec![1, 2]))]);
    assert_eq!(
        entries(3),
        [(1u32.to_be_bytes().to_vec(), Some(vec![1, 2]))]
    );
    assert_eq!(entries(7), [(vec![2], Some(Vec::new()))]);
    let value = |key: u8| index.child(&[key]).and_then(|n| n.value);
    assert_eq!(value(4), Some(&1u32.to_be_bytes()[..]));
    assert_eq!(value(0), Some(&[1, 3][..]));
}

/// Portal rows in a server script (script-runtime.md 13.24, s35-portals):
/// Go to Portal Row moves through the related records of the window's
/// record, entering the row's first field; a step's calculation reads the
/// active row's record for the portal's related fields; Next past the last
/// row keeps it; Go to Object on the portal keeps the row with no field
/// entered, by a name no object has is 116 and an empty name is 5; Commit
/// Records leaves the portal (#100).
#[test]
fn server_scripts_move_through_portal_rows() {
    use fixture::{
        exit_with, go_to_layout, go_to_record, on, script, set_field_in, set_variable, with_calc,
    };
    use om_hbam::OwnedNode;
    // Two notes of task 1 ("Write tests"), made by the script itself.
    let mut steps = vec![on(86, true), go_to_layout(fixture::NOTES_LAYOUT)];
    for body in ["first", "second"] {
        steps.extend([
            ScriptStep::new(7),
            set_field_in(fixture::NOTES_OCCURRENCE, fixture::TASK, "\"Write tests\""),
            set_field_in(
                fixture::NOTES_OCCURRENCE,
                fixture::BODY,
                &format!("{body:?}"),
            ),
            ScriptStep::new(75),
        ]);
    }
    let row = "Get ( ActivePortalRowNumber ) & \"/\" & Notes::Body & \"/\" & \
               Get ( ActiveLayoutObjectName ) & \"/\" & Get ( ActiveFieldContents )";
    steps.extend([
        go_to_layout(fixture::LAYOUT),
        go_to_record(1),
        {
            let mut step = ScriptStep::new(99);
            step.value = 1;
            step
        },
        set_variable("$first", row),
        {
            let mut step = ScriptStep::new(99);
            step.value = 4;
            step
        },
        set_variable("$second", row),
        // GetNthRecord reads the related set from the parent, not the row.
        set_variable(
            "$nth",
            "GetNthRecord ( Notes::Body ; 1 ) & \"/\" & Notes::Body",
        ),
        // Go to Record keeps the row, and the field entered in it.
        go_to_record(1),
        set_variable("$kept", row),
        {
            let mut step = ScriptStep::new(99);
            step.value = 4;
            step
        },
        set_variable(
            "$past",
            "Get ( LastError ) & \"/\" & Get ( ActivePortalRowNumber )",
        ),
        with_calc(145, "\"Notes portal\""),
        set_variable("$portal", row),
        with_calc(145, "\"Nope\""),
        set_variable("$missing", "Get ( LastError )"),
        with_calc(145, "\"\""),
        set_variable("$empty", "Get ( LastError )"),
        with_calc(145, "\"Notes portal\""),
        {
            // Go to Field outside the portal leaves it.
            let mut step = ScriptStep::new(17);
            step.set_field_spec(
                0,
                Some(om_scripts::FieldSpec::new(
                    om_scripts::StoredKey::from_id(fixture::OCCURRENCE).unwrap(),
                    om_scripts::StoredKey::from_id(fixture::NAME).unwrap(),
                    0,
                )),
            );
            step
        },
        set_variable("$left", "Get ( ActivePortalRowNumber )"),
        with_calc(145, "\"Notes portal\""),
        ScriptStep::new(75),
        exit_with(
            "$first & \"|\" & $second & \"|\" & $nth & \"|\" & $kept & \"|\" & $past & \"|\" \
             & $portal & \"|\" & $missing & \"|\" & $empty & \"|\" & $left & \"|\" & \
             Get ( ActivePortalRowNumber )",
        ),
    ]);
    let scripts = [script(6, "Portal rows", steps)];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_calculated(&mut root);
    // The Tasks layout gets a portal of Notes (object 50) holding Body
    // (object 51), named for Go to Object.
    let layout = uint_segment(fixture::LAYOUT);
    let alias = om_hbam::serial::id_of_raw(fixture::NOTES_OCCURRENCE).unwrap();
    let rect = |top: f64, left: f64, bottom: f64, right: f64| om_layout::Rect {
        top,
        left,
        bottom,
        right,
        ..Default::default()
    };
    let objects = [
        om_layout::encode::portal_object(50, &rect(100.0, 20.0, 190.0, 400.0), &[51], alias, 3),
        om_layout::encode::portal_field_object(
            51,
            &rect(100.0, 20.0, 125.0, 220.0),
            alias,
            fixture::BODY,
        ),
    ];
    let store = om_layout::encode::append_to_store(&[], &[50], &objects);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    for (id, name) in [(50u64, "Notes portal"), (51, "Note body")] {
        root.put(
            &[&[4], &[5], &layout, &[5], &serial::encode(id), &[0x10]],
            om_scripts::text_node(&[], name),
        );
    }
    let file = TempFile::from_tree("portal-rows", &root);
    let port = start(&file, None);
    let mut client = logged_in(port, "portals");
    assert_eq!(
        run(&mut client, 6).1,
        "1/first/Note body/first|2/second/Note body/second|first/second|\
         2/second/Note body/second|101/2|2/second/Notes portal/|116|5|0|0"
    );
    client.disconnect().unwrap();
}
