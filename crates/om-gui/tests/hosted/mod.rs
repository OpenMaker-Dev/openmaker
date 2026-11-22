//! A temporary copy of test project (or of another file, [`host_file`])
//! hosted by an in-process `om-server` on loopback over verified TLS, for
//! the native GUI tests. Nothing leaves this machine; the copy is removed
//! when the test ends.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::path::{Path, PathBuf};

use om_data::Connection;
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

use crate::common::{self, App};
use egui_kittest::kittest::Queryable;

/// A copy of test project taken with FileMaker Server's backup command, with
/// Contacts records 1 "Test Record", 2 "Hello World", 3 "Ima Computer";
/// git-ignored on the analysis host.
const TEST_PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../re/work/omserver/testproject-live-1234.fmp12"
);

pub struct Hosted {
    pub connection: Connection,
    ca_pem: Vec<u8>,
    /// The hosted copies, the connection's file first.
    copies: Vec<PathBuf>,
    /// The server, for [`Hosted::shut_down`].
    handle: Option<om_server::ServerHandle>,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        for copy in &self.copies {
            let _ = std::fs::remove_file(copy);
        }
    }
}

/// Where the hosted copies go: a RAM-backed folder when the system has one.
/// The server syncs the file to disk after every commit, and on a busy
/// disk (other builds writing gigabytes) each sync waited seconds, which
/// timed out the window's waits; the save and sync still run here.
fn copies_folder() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// Host a fresh copy of test project; `name` keeps parallel tests apart.
/// The window signs in as its Admin, with the password in
/// `OM_TEST_PROJECT_PASSWORD`, which the server checks against the file.
pub fn host(name: &str) -> Hosted {
    let password = std::env::var("OM_TEST_PROJECT_PASSWORD")
        .expect("OM_TEST_PROJECT_PASSWORD holds the test project's password");
    host_file(
        name,
        Path::new(TEST_PROJECT),
        "test project.fmp12",
        ("Admin", &password),
    )
}

/// Host a fresh copy of `source` as `file`, signing in as the file's
/// account `(name, password)`.
pub fn host_file(name: &str, source: &Path, file: &str, account: (&str, &str)) -> Hosted {
    host_file_with(name, source, file, account, |server| server)
}

/// [`host_file`], the server changed by `configure` before it serves.
pub fn host_file_with(
    name: &str,
    source: &Path,
    file: &str,
    account: (&str, &str),
    configure: impl FnOnce(Server) -> Server,
) -> Hosted {
    host_files_with(name, &[(source, file)], account, configure)
}

/// A fresh copy of each `(source, file)` hosted as `file`, the first
/// being the connection's, signing in as the file's account `(name,
/// password)`; the server changed by `configure` before it serves.
pub fn host_files_with(
    name: &str,
    files: &[(&Path, &str)],
    account: (&str, &str),
    configure: impl FnOnce(Server) -> Server,
) -> Hosted {
    host_files_recorded(name, files, account, configure, None)
}

/// [`host_files_with`], the server recording each connection's GIOP
/// streams into `record_dir` (`conn-NNN/c2s.bin`, `s2c.bin`), so a test can
/// count what the window asks for.
pub fn host_files_recorded(
    name: &str,
    files: &[(&Path, &str)],
    (account, password): (&str, &str),
    configure: impl FnOnce(Server) -> Server,
    record_dir: Option<PathBuf>,
) -> Hosted {
    // The server writes every change back to the files it hosts.
    let copies: Vec<PathBuf> = files
        .iter()
        .enumerate()
        .map(|(index, (source, _))| {
            let suffix = if index == 0 {
                String::new()
            } else {
                format!("-{index}")
            };
            let copy = copies_folder().join(format!(
                "om-gui-{name}{suffix}-{}.fmp12",
                std::process::id()
            ));
            std::fs::copy(source, &copy).expect("copy the file to host");
            copy
        })
        .collect();
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
    let connection = Connection {
        host: "127.0.0.1".into(),
        port: 0,
        file: files[0].1.into(),
        account: account.into(),
        password: password.into(),
    };
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
            .map(|((_, file), copy)| HostedFile {
                name: (*file).into(),
                path: copy.clone(),
            })
            .collect(),
        // Files are hosted as they are, a starter solution's Admin with an
        // empty password included.
        insecure_files: true,
        record_dir,
    };
    // Accounts in custom sets too, which a server refuses by default
    // (#734): the window's tests are about what it does with them.
    let server = configure(
        Server::bind("127.0.0.1:0", config, identity)
            .unwrap()
            .admitting_custom_sets(),
    );
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().ok();
    std::thread::spawn(move || server.serve());
    Hosted {
        connection: Connection { port, ..connection },
        ca_pem,
        copies,
        handle,
    }
}

impl Hosted {
    /// Stop the server: it accepts no more connections and closes the
    /// open ones. For a test that hosts many servers in one process.
    pub fn shut_down(&self) {
        if let Some(handle) = &self.handle {
            handle.shutdown(std::time::Duration::from_secs(5));
            handle.close_connections();
        }
    }

    /// The hosted copy, as the server last saved it.
    /// The server, as its administrator drives it (Send Message, clients).
    #[allow(dead_code)]
    pub fn server(&self) -> &om_server::ServerHandle {
        self.handle.as_ref().expect("the server's handle")
    }

    pub fn file(&self) -> &Path {
        &self.copies[0]
    }

    /// A native provider that trusts this server, not yet opened.
    pub fn provider(&self) -> NativeProvider {
        self.provider_trusting(&[])
    }

    /// [`Hosted::provider`], trusting the `others` servers too.
    pub fn provider_trusting(&self, others: &[&Hosted]) -> NativeProvider {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut additional_ca_pem = self.ca_pem.clone();
        for other in others {
            additional_ca_pem.extend_from_slice(&other.ca_pem);
        }
        NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem,
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        })
    }

    /// A provider opened on the hosted file, for checks outside the GUI.
    pub fn opened(&self) -> NativeProvider {
        use om_data::DataProvider;
        let mut provider = self.provider();
        provider
            .open(&self.connection)
            .expect("open the hosted file");
        provider
    }

    /// Fill in the Hosts dialog for this server.
    pub fn fill_form(&self, app: &mut App) {
        // These loopback servers do not require a client license. Tests must
        // never access the developer's real credential store.
        if app.query_by_label("Use saved license").is_some() {
            common::click(app, "Use saved license");
        }
        common::fill(app, "Host", &self.connection.host);
        replace(app, "Port", &self.connection.port.to_string());
        common::fill(app, "File", &self.connection.file);
        common::fill(app, "Account", &self.connection.account);
        common::fill(app, "Password", &self.connection.password);
    }
}

/// Replace the text of the field labelled `label`.
pub fn replace(app: &mut App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text(text.into()));
    app.run_steps(2);
}
