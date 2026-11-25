//! `om-server --record`: every connection is saved under `conn-NNN`, and a
//! server started again over the same folder numbers its connections after
//! the ones already there instead of losing them.

use std::path::{Path, PathBuf};

use om_protocol::MachineInfo;
use om_server::{Identity, Server, ServerConfig, ServerIdentity, StartError};
use om_session::Session;

/// A fresh, empty folder for one test's recordings, removed afterwards.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("om-record-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(record_dir: &Path) -> ServerConfig {
    ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: Vec::new(),
        insecure_files: false,
        record_dir: Some(record_dir.to_path_buf()),
    }
}

/// Start a recording server on an ephemeral loopback port.
fn start(record_dir: &Path) -> u16 {
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config(record_dir), identity).unwrap();
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    port
}

/// Connect and disconnect once. The server records `_is_a` and `Connect`
/// before it answers them, so they are on disk when this returns.
fn visit(port: u16) {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let session = Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.disconnect().unwrap();
}

/// The client stream recorded for connection `index`.
fn recorded(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name).join("c2s.bin")).unwrap_or_default()
}

#[test]
fn a_restarted_server_records_after_the_existing_connections() {
    let dir = TempDir::new("restart");
    // A previous run's recordings, the highest one with a gap below it.
    for name in ["conn-000", "conn-002"] {
        std::fs::create_dir(dir.0.join(name)).unwrap();
        std::fs::write(dir.0.join(name).join("c2s.bin"), b"earlier run").unwrap();
    }

    visit(start(&dir.0));
    let first = recorded(&dir.0, "conn-003");
    assert!(
        first.starts_with(b"GIOP"),
        "conn-003 holds the new connection"
    );
    for name in ["conn-000", "conn-002"] {
        assert_eq!(
            recorded(&dir.0, name),
            b"earlier run",
            "{name} is untouched"
        );
    }
    assert!(!dir.0.join("conn-001").exists(), "gaps are not filled");

    // Restarted again: numbering continues after conn-003.
    visit(start(&dir.0));
    assert!(recorded(&dir.0, "conn-004").starts_with(b"GIOP"));
}

#[test]
fn a_connection_folder_made_meanwhile_is_skipped() {
    let dir = TempDir::new("taken");
    let port = start(&dir.0);
    // Another process takes the next number after the server started.
    std::fs::create_dir(dir.0.join("conn-000")).unwrap();
    visit(port);
    assert!(recorded(&dir.0, "conn-001").starts_with(b"GIOP"));
    assert!(recorded(&dir.0, "conn-000").is_empty());
}

#[test]
fn a_record_folder_that_cannot_be_made_stops_the_server_starting() {
    let dir = TempDir::new("unwritable");
    let file = dir.0.join("not a folder");
    std::fs::write(&file, b"").unwrap();
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let started = Server::bind("127.0.0.1:0", config(&file.join("record")), identity);
    assert!(
        matches!(&started, Err(StartError::Record { .. })),
        "{:?}",
        started.err()
    );
}
