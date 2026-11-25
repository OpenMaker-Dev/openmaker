//! An `om-server` in the test's process hosting a [`super::fixture`] file,
//! and `om-session` clients logged in to it and registered for its change
//! notifications; shared by the schema, server script and administration
//! tests.
#![allow(dead_code)]

use om_hbam::slot::uint_segment;
use om_hbam::{serial, KeySegment, KeyTree, SlotReader};
use om_protocol::write;
use om_protocol::{LoginEnvironment, MachineInfo, Opcode, RequestNotification};
use om_server::{
    HostedFile, Identity, ScriptLimits, Server, ServerConfig, ServerHandle, ServerIdentity,
};
use om_session::Session;

use super::fixture::{TempFile, ACCOUNT, FILE, INTERNAL_CREATION, PASSWORD};

/// Start a server hosting `file` on an ephemeral loopback port; Admin has
/// [Full Access].
pub fn start(file: &TempFile) -> u16 {
    start_with_limits(file, ScriptLimits::DEFAULT)
}

/// [`start`] with `limits` for its scripts.
pub fn start_with_limits(file: &TempFile, limits: ScriptLimits) -> u16 {
    start_with_handle(file, limits).0
}

/// [`start_with_limits`], and a handle for administering the server.
pub fn start_with_handle(file: &TempFile, limits: ScriptLimits) -> (u16, ServerHandle) {
    serve(bind(file).with_script_limits(limits))
}

/// [`start`], the server configured by `configure` first.
pub fn start_configured(file: &TempFile, configure: impl FnOnce(Server) -> Server) -> u16 {
    start_configured_with_handle(file, configure).0
}

/// [`start_configured`], and a handle for administering the server.
pub fn start_configured_with_handle(
    file: &TempFile,
    configure: impl FnOnce(Server) -> Server,
) -> (u16, ServerHandle) {
    serve(configure(bind(file)))
}

/// [`start`], closing connections silent for `timeout`, and a handle.
pub fn start_with_idle_timeout(
    file: &TempFile,
    timeout: std::time::Duration,
) -> (u16, ServerHandle) {
    serve(bind(file).with_idle_timeout(timeout))
}

/// [`start_configured_with_handle`] for several files, each with the name
/// clients open it by.
pub fn start_hosting(
    files: &[(&str, &TempFile)],
    configure: impl FnOnce(Server) -> Server,
) -> (u16, ServerHandle) {
    serve(configure(bind_files(files)))
}

/// A server for `file`; Admin has [Full Access].
fn bind(file: &TempFile) -> Server {
    bind_files(&[(FILE, file)])
}

/// A server for `files`, each with the name clients open it by; Admin has
/// [Full Access].
fn bind_files(files: &[(&str, &TempFile)]) -> Server {
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: files
            .iter()
            .map(|(name, file)| HostedFile {
                name: (*name).into(),
                path: file.0.clone(),
            })
            .collect(),
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Every download and find is answered through the test view too, and
    // checked against the file engine's view (docs/server-storage.md 5,
    // "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads().checking_writes();
    server
}

/// Serve `server` on its own thread.
fn serve(server: Server) -> (u16, ServerHandle) {
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    (port, handle)
}

/// A session logged in to the file as Admin and registered for its
/// notifications, and the file's id.
pub fn logged_in(port: u16, name: &str) -> (Session, u32) {
    logged_in_to(port, name, FILE)
}

/// [`logged_in`] to the hosted file `file`.
pub fn logged_in_to(port: u16, name: &str, file: &str) -> (Session, u32) {
    logged_in_as(port, name, file, ACCOUNT, PASSWORD)
}

/// [`logged_in_to`] as `account` with `password`.
pub fn logged_in_as(
    port: u16,
    name: &str,
    file: &str,
    account: &str,
    password: &str,
) -> (Session, u32) {
    logged_in_reading(port, name, file, account, password, true)
}

/// [`logged_in`], without reading the status area `[2]` before Login (the
/// file's internal creation is the fixture's): a session that has read
/// nothing of the file.
pub fn logged_in_unread(port: u16, name: &str) -> (Session, u32) {
    logged_in_reading(port, name, FILE, ACCOUNT, PASSWORD, false)
}

fn logged_in_reading(
    port: u16,
    name: &str,
    file: &str,
    account: &str,
    password: &str,
    read_status: bool,
) -> (Session, u32) {
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
    let open = session.open_file("127.0.0.1", file).unwrap();
    if read_status {
        assert_eq!(
            session.fetch_internal_creation().unwrap(),
            INTERNAL_CREATION
        );
    }
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", file);
    let login = session
        .login(account, password, INTERNAL_CREATION, &environment)
        .unwrap();
    assert!(login.is_success(), "{login:?}");
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
    (session, open.file_id)
}

/// The records of `table` as a client downloads them: id -> (cell id,
/// text), with the modification count `fc` as cell 0xfc.
pub fn records_of(session: &mut Session, table: u64) -> Vec<(u64, Vec<(u64, String)>)> {
    let table = uint_segment(table);
    let path: [&[u8]; 2] = [&table, &[5]];
    let reply = session.download(&path).unwrap();
    let base = path.iter().map(|s| KeySegment(s)).collect();
    let tree = KeyTree::from_slots(SlotReader::with_path(&reply.slots, base)).unwrap();
    let Some(node) = tree.node(&path) else {
        return Vec::new();
    };
    node.children
        .iter()
        .map(|record| {
            let cells = record
                .children
                .iter()
                .filter_map(|cell| {
                    if cell.segment.0 == [0xfc] {
                        let count = cell.assembled()?;
                        let count = serial::decode(count.get(1..)?)?;
                        return Some((0xfc, count.to_string()));
                    }
                    let text = om_hbam::Cell::from_node(cell).text()?.ok()?;
                    Some((cell.id()?, text))
                })
                .collect();
            (serial::decode(record.segment.0).unwrap(), cells)
        })
        .collect()
}

/// A record's cell, from [`records_of`].
pub fn cell(records: &[(u64, Vec<(u64, String)>)], record: u64, field: u64) -> Option<&str> {
    let (_, cells) = records.iter().find(|(id, _)| *id == record)?;
    cells
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, text)| text.as_str())
}

/// Notifications another session gets: with the reply to its next request.
pub fn notifications_of(session: &mut Session) -> Vec<write::Notification> {
    let probe = om_rctransaction::Builder::new(Opcode::GetGuestCount as u8).finish();
    let reply = session.perform_reply(&probe).unwrap();
    write::parse_notification_backchannel(&reply.backchannel).unwrap_or_default()
}

/// The attention code and path a notification names (write-path.md 6.3):
/// the path is item 2, before the last length word, its first byte the
/// path's total length.
pub fn notified(notification: &write::Notification) -> (u32, Vec<u8>) {
    let message = &notification.message;
    let length = u16::from_be_bytes([message[message.len() - 2], message[message.len() - 1]]);
    let path = message[message.len() - 2 - usize::from(length) + 1..message.len() - 2].to_vec();
    (notification.code, path)
}

/// A key path as notifications carry it: `[u8 len][segment]...`.
pub fn path_key(path: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for segment in path {
        out.push(segment.len() as u8);
        out.extend(*segment);
    }
    out
}
