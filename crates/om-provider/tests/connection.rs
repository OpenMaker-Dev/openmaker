//! The desktop client's provider keeps an open file's connection alive and
//! reports when it is lost (network-protocol.md 6.1-6.3), against an
//! `om-server` on loopback over the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs`.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;

use std::path::Path;
use std::time::{Duration, Instant};

use fixture::{TempFile, ACCOUNT, FILE, LAYOUT, PASSWORD};
use om_data::{Connection, DataProvider};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerHandle, ServerIdentity};

/// A server hosting `path` that closes connections silent for
/// `idle_timeout`, its handle, and the CA to trust.
fn host(path: &Path, idle_timeout: Duration) -> (u16, ServerHandle, Vec<u8>) {
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
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .with_idle_timeout(idle_timeout);
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    (port, handle, ca_pem)
}

fn open(port: u16, ca_pem: &[u8], keepalive: Duration) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem.to_vec(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    })
    .with_keepalive(Some(keepalive));
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

#[test]
fn an_open_file_stays_connected_while_idle_and_reports_its_loss() {
    let file = TempFile::new("provider-keepalive", &[], true);
    let (port, handle, ca_pem) = host(&file.0, Duration::from_millis(500));
    let mut provider = open(port, &ca_pem, Duration::from_millis(100));
    let layout = u32::try_from(LAYOUT).unwrap();

    std::thread::sleep(Duration::from_millis(1500));
    assert!(provider.connection_lost().is_none());
    assert_eq!(provider.records(layout, 0, 10).unwrap().total, 2);

    // The administrator disconnects the client; its keepalive finds out.
    let client = handle.clients()[0].id;
    assert!(handle.disconnect_client(client));
    let deadline = Instant::now() + Duration::from_secs(10);
    let lost = loop {
        if let Some(lost) = provider.connection_lost() {
            break lost;
        }
        assert!(Instant::now() < deadline, "the loss went unnoticed");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(lost.lost);
    assert!(lost.message.contains("connection to the host was lost"));

    // Nothing more reaches the host, and closing does not wait for it
    // (a request on the dead socket would wait out a 30 s read timeout;
    // the bound leaves room for a loaded machine).
    let started = Instant::now();
    assert!(provider.records(layout, 0, 10).is_err());
    provider.close();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(provider.connection_lost().is_none(), "no file is open");
}

/// The value of field `name` in the page's first record.
fn first_value(page: &om_data::RecordPage, name: &str) -> Option<String> {
    let index = page.fields.iter().position(|f| f.name == name)?;
    page.records.first()?.values[index].clone()
}

/// Reconnecting after a loss is a fresh Connect, Open and Login: a wrong
/// password leaves no file open; the right one carries the window's found
/// set over (the record it omitted stays out though it still exists), and
/// a record read by id shows what another client changed meanwhile. The
/// version that read is the one a save is checked against: a page read
/// after another client's later change does not stand in for it, until the
/// record is read by id again or written. The fresh login also permits
/// later peer opens, as the original login did.
#[test]
fn a_reopened_file_keeps_the_found_set_and_reads_records_by_id() {
    use om_data::found::FoundSetChange;
    let file = TempFile::new("provider-reopen", &[], true);
    let (port, handle, ca_pem) = host(&file.0, Duration::from_secs(60));
    let mut provider = open(port, &ca_pem, Duration::from_millis(100));
    let layout = u32::try_from(LAYOUT).unwrap();
    let page = provider.records(layout, 0, 10).unwrap();
    assert_eq!(page.total, 2);
    let (first, second) = (page.records[0].id, page.records[1].id);
    provider
        .change_found_set(layout, FoundSetChange::Omit(vec![first]))
        .unwrap();
    // An open file logs into another file with the password it keeps.
    provider.open_peer(FILE).unwrap().close();

    let client = handle.clients()[0].id;
    assert!(handle.disconnect_client(client));
    let deadline = Instant::now() + Duration::from_secs(10);
    while provider.connection_lost().is_none() {
        assert!(Instant::now() < deadline, "the loss went unnoticed");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Another client changes a record meanwhile.
    let mut other = open(port, &ca_pem, Duration::from_secs(30));
    let name = |text: &str| {
        [("Name".to_owned(), Some(text.to_owned()))]
            .into_iter()
            .collect()
    };
    other
        .update_record(layout, second, name("Changed meanwhile"))
        .unwrap();

    let mut connection = Connection {
        host: "127.0.0.1".into(),
        port,
        file: FILE.into(),
        account: ACCOUNT.into(),
        password: "wrong".into(),
    };
    assert!(provider.reopen(&connection).is_err());
    assert!(provider.connection_lost().is_none(), "no file is open");
    assert!(provider.records(layout, 0, 10).is_err());
    connection.password = PASSWORD.into();
    provider.reopen(&connection).unwrap();
    assert!(provider.connection_lost().is_none());
    provider.open_peer(FILE).unwrap().close();

    // The window's found set: the omitted record, still on the host, stays
    // out (all records would be 2).
    assert_eq!(provider.records(layout, 0, 10).unwrap().total, 1);
    assert_eq!(provider.record_position(layout, second).unwrap(), Some(0));
    assert_eq!(provider.record_position(layout, first).unwrap(), None);
    assert_eq!(provider.record_by_id(layout, first).unwrap().total, 1);
    let read = provider.record_by_id(layout, second).unwrap();
    assert_eq!(read.total, 1);
    assert_eq!(read.records[0].id, second);
    assert_eq!(
        first_value(&read, "Name").as_deref(),
        Some("Changed meanwhile")
    );

    // Another change after that read, then an ordinary page read (the
    // window's reload): a save still answers to the version read by id.
    other
        .update_record(layout, second, name("Changed again"))
        .unwrap();
    provider.copy_window(1, 2);
    provider.use_window(2);
    provider.release_record(layout, second);
    provider.records(layout, 0, 10).unwrap();
    provider.use_window(1);
    let refused = provider
        .update_record(layout, second, name("Mine"))
        .unwrap_err();
    assert!(
        refused.message.contains("Another client changed"),
        "{refused}"
    );
    // Read by id again, the change is seen and the save goes over it.
    provider.record_by_id(layout, second).unwrap();
    provider
        .update_record(layout, second, name("Mine"))
        .unwrap();
    // Written, the record answers to page reads again. (The other client
    // reads first: its own save answers to the version it read.)
    other.records(layout, 0, 10).unwrap();
    other.update_record(layout, second, name("Theirs")).unwrap();
    provider.records(layout, 0, 10).unwrap();
    provider
        .update_record(layout, second, name("Mine again"))
        .unwrap();
    // A released version also gives way to the next page read.
    provider.record_by_id(layout, second).unwrap();
    other.records(layout, 0, 10).unwrap();
    other
        .update_record(layout, second, name("Theirs again"))
        .unwrap();
    provider.release_record(layout, second);
    provider.records(layout, 0, 10).unwrap();
    provider
        .update_record(layout, second, name("Mine last"))
        .unwrap();

    // A record deleted on the host reads as none.
    other.delete_record(layout, first).unwrap();
    let gone = provider.record_by_id(layout, first).unwrap();
    assert!(gone.records.is_empty() && gone.total == 0);
    other.close();
    provider.close();
}

/// A change of field Name to `text` (empty empties it).
fn name(text: &str) -> om_data::FieldChanges {
    [(
        "Name".to_owned(),
        (!text.is_empty()).then(|| text.to_owned()),
    )]
    .into_iter()
    .collect()
}

/// A window's unsaved edit answers to the version that window showed when
/// it began (`retain_record`): another window's page read before or after
/// that, the other window giving up its own edit of the record, and the
/// window's own reload leave it, so another client's change refuses the
/// save (306) until the record is read by id again. A record another
/// client holds refuses the save (301) without moving that version: a
/// retry after their change is refused too, a retry after they let go
/// without one goes through.
#[test]
fn a_retained_edit_answers_to_the_version_its_window_showed() {
    use om_data::runtime::TransactionOptions;
    let file = TempFile::new("provider-retain", &[], true);
    let (port, _handle, ca_pem) = host(&file.0, Duration::from_secs(60));
    let mut provider = open(port, &ca_pem, Duration::from_secs(30));
    let mut other = open(port, &ca_pem, Duration::from_secs(30));
    let layout = u32::try_from(LAYOUT).unwrap();
    let first_page = provider.records(layout, 0, 10).unwrap();
    let first = first_page.records[0].id;
    let first_version = first_page.versions.get(&first).copied();
    provider.copy_window(1, 2);
    other.records(layout, 0, 10).unwrap();
    other.update_record(layout, first, name("Theirs")).unwrap();

    // Window 2 shows the change; window 1, which does not, begins an edit.
    provider.use_window(2);
    let second_page = provider.records(layout, 0, 10).unwrap();
    provider.use_window(1);
    provider.retain_record(layout, first, first_version);
    // Window 2 begins and gives up its own edit; window 1 reloads.
    provider.use_window(2);
    provider.retain_record(layout, first, second_page.versions.get(&first).copied());
    provider.release_record(layout, first);
    provider.use_window(1);
    let reloaded = provider.records(layout, 0, 10).unwrap();
    assert_eq!(first_value(&reloaded, "Name").as_deref(), Some("Theirs"));
    let refused = provider.update_record(layout, first, name("")).unwrap_err();
    assert_eq!(refused.code, Some(306), "{refused}");
    // Read by id (the window compares and the user keeps the edit).
    provider.record_by_id(layout, first).unwrap();
    provider.update_record(layout, first, name("")).unwrap();

    // A new edit; another client opens the record in a transaction.
    let shown = provider.records(layout, 0, 10).unwrap();
    provider.retain_record(layout, first, shown.versions.get(&first).copied());
    other.records(layout, 0, 10).unwrap();
    other
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    other
        .update_record(layout, first, name("Held then changed"))
        .unwrap();
    let locked = provider
        .update_record(layout, first, name("Mine"))
        .unwrap_err();
    assert_eq!(locked.code, Some(301), "{locked}");
    other.commit_transaction(false).unwrap();
    let reloaded = provider.records(layout, 0, 10).unwrap();
    assert_eq!(
        first_value(&reloaded, "Name").as_deref(),
        Some("Held then changed")
    );
    let refused = provider
        .update_record(layout, first, name("Mine"))
        .unwrap_err();
    assert_eq!(refused.code, Some(306), "the reload is not a check");
    assert_eq!(
        first_value(&other.record_by_id(layout, first).unwrap(), "Name").as_deref(),
        Some("Held then changed")
    );

    // Read by id again; the other client holds the record and lets go
    // without a change: the retry goes through.
    provider.record_by_id(layout, first).unwrap();
    other.records(layout, 0, 10).unwrap();
    other
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    other
        .update_record(layout, first, name("Abandoned"))
        .unwrap();
    let locked = provider
        .update_record(layout, first, name("Mine"))
        .unwrap_err();
    assert_eq!(locked.code, Some(301), "{locked}");
    other.revert_transaction().unwrap();
    provider.update_record(layout, first, name("Mine")).unwrap();
    assert_eq!(
        first_value(&other.record_by_id(layout, first).unwrap(), "Name").as_deref(),
        Some("Mine")
    );
    other.close();
    provider.close();
}

/// A draft with no accepted host version cannot borrow a newer backend
/// observation. An explicit successful readback can establish its basis.
#[test]
fn an_unversioned_draft_requires_a_successful_check_before_saving() {
    let file = TempFile::new("provider-unbased-draft", &[], true);
    let (port, _handle, ca_pem) = host(&file.0, Duration::from_secs(60));
    let mut provider = open(port, &ca_pem, Duration::from_secs(30));
    let mut reader = open(port, &ca_pem, Duration::from_secs(30));
    let layout = u32::try_from(LAYOUT).unwrap();
    let page = provider.records(layout, 0, 10).unwrap();
    let id = page.records[0].id;
    assert!(page.versions.contains_key(&id));
    let before = first_value(&page, "Name");
    provider.retain_record(layout, id, None);
    provider.records(layout, 0, 10).unwrap();
    let error = provider
        .update_record(layout, id, name("Mine"))
        .unwrap_err();
    assert_eq!(error.code, Some(306));
    assert_eq!(
        first_value(&reader.record_by_id(layout, id).unwrap(), "Name"),
        before
    );
    provider.record_by_id(layout, id).unwrap();
    provider.update_record(layout, id, name("Mine")).unwrap();
    assert_eq!(
        first_value(&reader.record_by_id(layout, id).unwrap(), "Name").as_deref(),
        Some("Mine")
    );
    provider.close();
    reader.close();
}
